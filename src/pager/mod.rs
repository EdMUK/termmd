//! The interactive viewer.
//!
//! The pager owns a [`Screen`] and a viewport into it. Scrolling moves the
//! viewport; it never re-lays-out the document. Re-rendering happens only when
//! something changes the layout -- a resize, a reload, toggling images -- which
//! is what keeps scrolling instant on a large document.
//!
//! Images are the one part that cannot be handled by slicing lines, because an
//! escape sequence draws a whole picture at once. Each frame therefore positions
//! the cursor and re-emits the images that intersect the viewport, cropped to
//! the rows that are actually visible, so a picture scrolls off the top edge a
//! row at a time instead of vanishing.

mod overlay;
pub mod search;

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

use crate::cli::Settings;
use crate::images::{Store, kitty};
use crate::markdown::Document;
use crate::render::{Highlighter, ImageProvider, ImageRequest, Line, Screen, Span, WriteOptions};
use crate::source::{self, Origin, Source, build_document};
use crate::term::caps::GraphicsProtocol;
use crate::term::clipboard;
use crate::term::style::StyleWriter;
use search::Search;

/// How long to wait for input before looking at the file watcher again.
const TICK: Duration = Duration::from_millis(150);
/// A file save often arrives as several events; wait for them to settle.
const RELOAD_DEBOUNCE: Duration = Duration::from_millis(120);

/// What the pager is currently doing.
#[derive(Debug, Clone, PartialEq)]
enum Mode {
    Normal,
    /// Typing a search query. `backward` remembers which way to jump on accept.
    Searching {
        backward: bool,
    },
    /// An overlay panel is open.
    Panel(Panel),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    Contents,
    Links,
    Help,
}

/// Runs the interactive viewer.
///
/// A document that already fits on one screen is simply printed: starting a
/// full-screen session to show six lines, and wiping them on exit, is a worse
/// experience than the output just being there.
pub fn run(
    sources: &[Source],
    settings: &Settings,
    highlighter: &Highlighter,
    store: &mut Store,
    watch: bool,
) -> Result<()> {
    let document = build_document(sources, settings.parse);
    let screen = render(&document, settings, highlighter, store);

    // `--pager` means the pager, even for a short document: the shortcut below
    // is a convenience for the automatic case, not an override of an explicit
    // request.
    let fits = screen.len() < settings.caps.rows.saturating_sub(1) as usize;
    if fits && !watch && !settings.pager_forced {
        let opts = WriteOptions {
            hyperlinks: settings.caps.hyperlinks,
            images: settings.render.images,
        };
        let stdout = std::io::stdout();
        let mut out = stdout.lock();
        screen.write(&mut out, &settings.caps, &opts, store)?;
        return Ok(());
    }

    let watched = if watch {
        crate::source::watchable_paths(sources)
    } else {
        Vec::new()
    };
    let mut pager = Pager {
        sources: sources.to_vec(),
        settings: settings.clone(),
        highlighter,
        store,
        document,
        screen,
        top: 0,
        left: 0,
        cols: settings.caps.cols,
        rows: settings.caps.rows,
        mode: Mode::Normal,
        search: None,
        input: String::new(),
        message: None,
        selection: 0,
        dirty: true,
        history: Vec::new(),
        quit: false,
    };
    let _guard = TerminalGuard::enter(settings.mouse)?;
    pager.event_loop(&watched)
}

fn render(
    document: &Document,
    settings: &Settings,
    highlighter: &Highlighter,
    store: &mut Store,
) -> Screen {
    if settings.render.images {
        crate::render::render(
            document,
            &settings.render,
            &settings.theme,
            &settings.caps,
            store,
            highlighter,
        )
    } else {
        crate::render::render(
            document,
            &settings.render,
            &settings.theme,
            &settings.caps,
            &mut crate::render::NoImages,
            highlighter,
        )
    }
}

struct Pager<'a> {
    sources: Vec<Source>,
    settings: Settings,
    highlighter: &'a Highlighter,
    store: &'a mut Store,
    document: Document,
    screen: Screen,
    /// First visible document line.
    top: usize,
    /// Horizontal scroll offset in columns.
    left: usize,
    cols: u16,
    rows: u16,
    mode: Mode,
    search: Option<Search>,
    input: String,
    message: Option<String>,
    /// Selected row in the open panel.
    selection: usize,
    /// Set when the frame on screen no longer matches the state.
    dirty: bool,
    /// Documents we navigated away from, for going back.
    history: Vec<Visit>,
    quit: bool,
}

/// A document we can return to.
struct Visit {
    sources: Vec<Source>,
    top: usize,
}

impl Pager<'_> {
    /// Rows available for the document, leaving one for the status bar.
    fn viewport(&self) -> usize {
        self.rows.saturating_sub(1).max(1) as usize
    }

    /// The furthest we may scroll: the last screenful, not the last line.
    fn max_top(&self) -> usize {
        self.screen.len().saturating_sub(self.viewport())
    }

    fn event_loop(&mut self, watched: &[PathBuf]) -> Result<()> {
        let mut watcher = FileWatcher::start(watched);

        while !self.quit {
            // Redrawing only when something changed keeps an idle pager at zero
            // CPU, which matters because it is the state it spends most of its
            // life in -- open on a second monitor while you work.
            if self.dirty {
                self.draw()?;
                self.dirty = false;
            }

            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) => {
                        self.on_key(key)?;
                        self.dirty = true;
                    }
                    Event::Mouse(mouse) => self.on_mouse(mouse)?,
                    Event::Resize(cols, rows) => {
                        self.cols = cols;
                        self.rows = rows;
                        self.relayout();
                        self.dirty = true;
                    }
                    _ => {}
                }
            }
            if watcher.changed() {
                self.reload()?;
                self.dirty = true;
            }
        }
        Ok(())
    }

    // --- input ----------------------------------------------------------

    fn on_key(&mut self, key: KeyEvent) -> Result<()> {
        // Terminals speaking the kitty keyboard protocol also report releases;
        // acting on those would double every keystroke.
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        self.message = None;
        match self.mode.clone() {
            Mode::Searching { backward } => self.on_search_key(key, backward),
            Mode::Panel(panel) => self.on_panel_key(key, panel),
            Mode::Normal => self.on_normal_key(key),
        }
    }

    fn on_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.scroll(3);
                self.dirty = true;
            }
            MouseEventKind::ScrollUp => {
                self.scroll(-3);
                self.dirty = true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                // Taking over the mouse means taking over link clicks too.
                if let Some(index) = self.link_at(mouse.row, mouse.column) {
                    if let Some(url) = self.screen.links.get(index).cloned() {
                        self.follow(&url)?;
                        self.dirty = true;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The link under a screen cell, if any.
    fn link_at(&self, row: u16, column: u16) -> Option<usize> {
        if row as usize >= self.viewport() {
            return None;
        }
        let line = self.screen.lines.get(self.top + row as usize)?;
        let target = self.left + column as usize;
        let mut at = 0usize;
        for span in &line.spans {
            let width = span.width();
            if target < at + width {
                return span.link;
            }
            at += width;
        }
        None
    }

    fn on_normal_key(&mut self, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let page = self.viewport().saturating_sub(2).max(1) as isize;
        let half = (self.viewport() / 2).max(1) as isize;

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('d') if ctrl => self.scroll(half),
            KeyCode::Char('u') if ctrl => self.scroll(-half),
            KeyCode::Char('f') if ctrl => self.scroll(page),
            KeyCode::Char('b') if ctrl => self.scroll(-page),

            KeyCode::Char('j') | KeyCode::Down | KeyCode::Enter => self.scroll(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll(-1),
            KeyCode::Char(' ') | KeyCode::PageDown => self.scroll(page),
            KeyCode::Char('b') | KeyCode::PageUp => self.scroll(-page),
            KeyCode::Char('d') => self.scroll(half),
            KeyCode::Char('u') => self.scroll(-half),
            KeyCode::Char('g') | KeyCode::Home => self.top = 0,
            KeyCode::Char('G') | KeyCode::End => self.top = self.max_top(),

            KeyCode::Char('h') | KeyCode::Left => self.left = self.left.saturating_sub(4),
            KeyCode::Char('l') | KeyCode::Right => self.left = (self.left + 4).min(self.max_left()),
            KeyCode::Char('0') => self.left = 0,

            KeyCode::Char('/') => {
                self.mode = Mode::Searching { backward: false };
                self.input.clear();
            }
            KeyCode::Char('?') => {
                self.mode = Mode::Searching { backward: true };
                self.input.clear();
            }
            KeyCode::Char('n') => self.jump_search(false),
            KeyCode::Char('N') => self.jump_search(true),

            KeyCode::Char('t') => self.open_panel(Panel::Contents),
            KeyCode::Char('L') => self.open_panel(Panel::Links),
            KeyCode::Char('H') | KeyCode::F(1) => self.open_panel(Panel::Help),

            KeyCode::Char('y') => self.copy_visible_code(),
            KeyCode::Char('r') => self.reload()?,
            KeyCode::Char('i') => self.toggle_images(),
            KeyCode::Char('m') => self.toggle_mouse(),
            KeyCode::Backspace => self.go_back(),
            _ => {}
        }
        self.clamp();
        Ok(())
    }

    fn on_search_key(&mut self, key: KeyEvent, backward: bool) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.search = None;
                self.input.clear();
            }
            KeyCode::Enter => {
                self.mode = Mode::Normal;
                if let Some(search) = &mut self.search {
                    if search.is_empty() {
                        self.message = Some(format!("not found: {}", self.input));
                    } else {
                        // Jump from where the reader is looking, not from the
                        // top of the document.
                        let from = if backward { 0 } else { self.top + 1 };
                        let target = if backward {
                            search.retreat()
                        } else {
                            search.focus_from(from)
                        };
                        if let Some(m) = target {
                            self.scroll_to(m.line);
                        }
                    }
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                self.update_search();
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                self.update_search();
            }
            _ => {}
        }
        self.clamp();
        Ok(())
    }

    fn on_panel_key(&mut self, key: KeyEvent, panel: Panel) -> Result<()> {
        let count = self.panel_len(panel);
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Normal,
            KeyCode::Char('t') if panel == Panel::Contents => self.mode = Mode::Normal,
            KeyCode::Char('j') | KeyCode::Down => {
                self.selection = (self.selection + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char('k') | KeyCode::Up => self.selection = self.selection.saturating_sub(1),
            KeyCode::Char('g') | KeyCode::Home => self.selection = 0,
            KeyCode::Char('G') | KeyCode::End => self.selection = count.saturating_sub(1),
            KeyCode::Enter => self.activate_panel_selection(panel)?,
            KeyCode::Char('y') if panel == Panel::Links => {
                let url = self.screen.links.get(self.selection).cloned();
                match url {
                    Some(url) => self.put_on_clipboard(&url, &url),
                    None => self.message = Some("no link to copy".into()),
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Copies the first code block on screen.
    ///
    /// The first visible one rather than the nearest one: a reader looking at
    /// a block and pressing `y` means that block, and "the one I can see" is a
    /// rule that can be followed without thinking about it.
    fn copy_visible_code(&mut self) {
        let Some(code) = self.visible_code().cloned() else {
            self.message = Some("no code block on screen".into());
            return;
        };
        let lines = code.lines().count();
        let plural = if lines == 1 { "" } else { "s" };
        self.put_on_clipboard(&code, &format!("copied {lines} line{plural}"));
    }

    /// The source of the first code block showing in the viewport.
    fn visible_code(&self) -> Option<&String> {
        self.screen
            .lines
            .iter()
            .skip(self.top)
            .take(self.viewport())
            .find_map(|line| line.code)
            .and_then(|index| self.screen.code_blocks.get(index))
    }

    /// Puts `text` on the clipboard, saying what happened either way.
    fn put_on_clipboard(&mut self, text: &str, said: &str) {
        self.message = Some(match clipboard::copy(text) {
            // Written straight out rather than through the next frame: it draws
            // nothing, so it cannot disturb what is on screen.
            Ok(clipboard::Copy::Sequence(sequence)) => {
                let mut stdout = std::io::stdout();
                match stdout
                    .write_all(sequence.as_bytes())
                    .and_then(|()| stdout.flush())
                {
                    Ok(()) => said.to_string(),
                    Err(e) => format!("could not copy: {e}"),
                }
            }
            Ok(clipboard::Copy::Done) => said.to_string(),
            Err(why) => why.to_string(),
        });
    }

    fn panel_len(&self, panel: Panel) -> usize {
        match panel {
            Panel::Contents => self.screen.headings.len(),
            Panel::Links => self.screen.links.len(),
            Panel::Help => overlay::HELP.len(),
        }
    }

    fn activate_panel_selection(&mut self, panel: Panel) -> Result<()> {
        match panel {
            Panel::Contents => {
                if let Some(heading) = self.screen.headings.get(self.selection) {
                    let line = heading.line;
                    self.mode = Mode::Normal;
                    self.scroll_to(line);
                }
            }
            Panel::Links => {
                if let Some(url) = self.screen.links.get(self.selection).cloned() {
                    self.mode = Mode::Normal;
                    self.follow(&url)?;
                }
            }
            Panel::Help => self.mode = Mode::Normal,
        }
        Ok(())
    }

    /// Follows a link.
    ///
    /// Three cases, in order of how much the reader probably wanted to stay
    /// where they are: an in-document anchor scrolls, a link to another local
    /// Markdown file opens in termmd, and anything else is handed to the system.
    /// Following a document link pushes onto a history stack, so `backspace`
    /// comes back.
    fn follow(&mut self, url: &str) -> Result<()> {
        if let Some(anchor) = url.strip_prefix('#') {
            match self.screen.anchor(anchor) {
                Some(line) => self.scroll_to(line),
                None => self.message = Some(format!("no heading {url}")),
            }
            return Ok(());
        }

        // `other.md#section` targets a heading in another file.
        let (target, fragment) = match url.split_once('#') {
            Some((path, fragment)) => (path, Some(fragment)),
            None => (url, None),
        };
        if let Some(origin) = openable(target) {
            return self.open_document(origin, fragment);
        }

        // Opening a link leaves the terminal, so say what happened either way.
        match open::that_detached(url) {
            Ok(()) => self.message = Some(format!("opened {url}")),
            Err(e) => self.message = Some(format!("could not open {url}: {e}")),
        }
        Ok(())
    }

    /// Loads another document in place of the current one.
    fn open_document(&mut self, origin: Origin, fragment: Option<&str>) -> Result<()> {
        let mut source = Source::pending(origin);
        if let Err(error) = source.reload() {
            self.message = Some(format!("{error:#}"));
            return Ok(());
        }
        let name = source.name.clone();

        self.history.push(Visit {
            sources: std::mem::take(&mut self.sources),
            top: self.top,
        });
        self.sources = vec![source];
        self.document = build_document(&self.sources, self.settings.parse);
        self.rebuild();
        self.top = fragment.and_then(|f| self.screen.anchor(f)).unwrap_or(0);
        self.left = 0;
        self.clamp();
        self.message = Some(format!("{name} (backspace to go back)"));
        Ok(())
    }

    /// Returns to the document we came from.
    fn go_back(&mut self) {
        let Some(previous) = self.history.pop() else {
            self.message = Some("nothing to go back to".into());
            return;
        };
        self.sources = previous.sources;
        self.document = build_document(&self.sources, self.settings.parse);
        self.rebuild();
        self.top = previous.top.min(self.max_top());
    }

    fn open_panel(&mut self, panel: Panel) {
        self.selection = match panel {
            // Start the contents on whichever heading the reader is nearest.
            Panel::Contents => self
                .screen
                .headings
                .iter()
                .rposition(|h| h.line <= self.top)
                .unwrap_or(0),
            _ => 0,
        };
        self.mode = Mode::Panel(panel);
    }

    fn update_search(&mut self) {
        let search = Search::new(&self.input, &self.screen);
        self.search = Some(search);
        // Follow the first match while typing, so the search is incremental.
        if let Some(s) = &mut self.search {
            if let Some(m) = s.focus_from(self.top) {
                let line = m.line;
                self.scroll_into_view(line);
            }
        }
    }

    fn jump_search(&mut self, backward: bool) {
        let Some(search) = &mut self.search else {
            self.message = Some("no previous search".into());
            return;
        };
        let target = if backward {
            search.retreat()
        } else {
            search.advance()
        };
        match target {
            Some(m) => {
                let line = m.line;
                self.scroll_to(line);
            }
            None => self.message = Some("no matches".into()),
        }
    }

    // --- movement -------------------------------------------------------

    fn scroll(&mut self, delta: isize) {
        let next = self.top as isize + delta;
        self.top = next.clamp(0, self.max_top() as isize) as usize;
    }

    /// Puts `line` near the top, leaving a little context above it.
    fn scroll_to(&mut self, line: usize) {
        let context = (self.viewport() / 5).min(4);
        self.top = line.saturating_sub(context).min(self.max_top());
    }

    /// Scrolls only as far as needed to make `line` visible.
    fn scroll_into_view(&mut self, line: usize) {
        let height = self.viewport();
        if line < self.top {
            self.top = line;
        } else if line >= self.top + height {
            self.top = (line + 1 - height).min(self.max_top());
        }
    }

    fn max_left(&self) -> usize {
        let widest = self.screen.lines.iter().map(Line::width).max().unwrap_or(0);
        widest.saturating_sub(self.cols as usize)
    }

    fn clamp(&mut self) {
        self.top = self.top.min(self.max_top());
        self.left = self.left.min(self.max_left());
        if let Mode::Panel(panel) = self.mode {
            self.selection = self.selection.min(self.panel_len(panel).saturating_sub(1));
        }
    }

    // --- content --------------------------------------------------------

    /// Re-lays-out the document at the current size, without moving the reader.
    fn rebuild(&mut self) {
        // Recompute from what was originally asked for, so widening the window
        // gives the width back rather than keeping whatever the narrowest size
        // left behind.
        let target = self
            .settings
            .requested_width
            .unwrap_or_else(|| (self.cols as usize).min(100));
        self.settings.render.width = target.min(self.cols as usize).max(8);
        self.settings.render.margin = self
            .settings
            .render
            .margin
            .min(self.settings.render.width / 4);
        self.settings.caps.cols = self.cols;
        self.settings.caps.rows = self.rows;
        self.store.invalidate_encodings();
        self.screen = render(&self.document, &self.settings, self.highlighter, self.store);
        if let Some(query) = self.search.as_ref().map(|s| s.query.clone()) {
            self.search = Some(Search::new(&query, &self.screen));
        }
        self.clamp();
    }

    /// Re-lays-out the document, keeping the reader in roughly the same place.
    fn relayout(&mut self) {
        // Restore the reading position proportionally: after a resize the line
        // that was at the top may not exist any more.
        let anchor = self.progress();
        self.rebuild();
        self.top = ((self.screen.len() as f64) * anchor) as usize;
        self.clamp();
    }

    /// How far through the document the viewport is, as a fraction.
    fn progress(&self) -> f64 {
        if self.screen.is_empty() {
            return 0.0;
        }
        self.top as f64 / self.screen.len() as f64
    }

    fn reload(&mut self) -> Result<()> {
        for source in &mut self.sources {
            source.reload()?;
        }
        self.document = build_document(&self.sources, self.settings.parse);
        self.relayout();
        self.message = Some("reloaded".into());
        Ok(())
    }

    /// Switches between letting the terminal have the mouse and taking it.
    fn toggle_mouse(&mut self) {
        self.settings.mouse = !self.settings.mouse;
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(mouse_mode(self.settings.mouse).as_bytes());
        let _ = stdout.flush();
        self.message = Some(if self.settings.mouse {
            "mouse: termmd (click follows links, shift-drag to select)".into()
        } else {
            "mouse: terminal (drag to select, wheel scrolls)".into()
        });
    }

    fn toggle_images(&mut self) {
        self.settings.render.images = !self.settings.render.images;
        let protocol = if self.settings.render.images {
            self.settings.protocol
        } else {
            GraphicsProtocol::None
        };
        self.settings.caps.graphics = protocol;
        self.message = Some(if self.settings.render.images {
            "images on".into()
        } else {
            "images off".into()
        });
        self.relayout();
    }

    // --- drawing --------------------------------------------------------

    fn draw(&mut self) -> Result<()> {
        let mut out = String::with_capacity(8192);
        let width = self.cols as usize;
        let height = self.viewport();

        // Old placements must go before the new frame, or they linger on top of
        // whatever scrolled into their place. Only worth saying when the
        // document actually has pictures in it.
        if self.settings.caps.graphics == GraphicsProtocol::Kitty
            && self.settings.render.images
            && self.has_images()
        {
            out.push_str(kitty::clear_all());
        }
        out.push_str("\x1b[H");

        let mut writer = StyleWriter::new(self.settings.caps.color)
            .with_italic_fallback(self.settings.caps.italic_broken);

        for row in 0..height {
            out.push_str(&format!("\x1b[{};1H\x1b[K", row + 1));
            let Some(line) = self.screen.lines.get(self.top + row) else {
                continue;
            };
            let spans = line.slice_columns(self.left, self.left + width);
            let spans = match &self.search {
                Some(search) if !search.is_empty() => {
                    let ranges: Vec<_> = search.on_line(self.top + row).collect();
                    search::highlight(
                        spans,
                        self.left,
                        &ranges,
                        self.settings.theme.search_match,
                        self.settings.theme.search_current,
                    )
                }
                _ => spans,
            };
            write_spans(
                &mut out,
                &mut writer,
                &spans,
                &self.screen,
                self.settings.caps.hyperlinks,
            );
            writer.reset(&mut out);
        }

        if self.settings.render.images && self.has_images() {
            self.draw_images(&mut out, height);
        }
        self.draw_status(&mut out, &mut writer, width, height);

        if let Mode::Panel(panel) = self.mode {
            let items = self.panel_items(panel);
            overlay::draw(
                &mut out,
                &mut writer,
                &overlay::Panel {
                    title: overlay::title(panel),
                    items: &items,
                    selection: self.selection,
                    cols: self.cols,
                    rows: self.rows,
                    theme: &self.settings.theme,
                    glyphs: self.settings.render.glyphs,
                },
            );
        }

        let stdout = std::io::stdout();
        let mut lock = stdout.lock();
        lock.write_all(out.as_bytes())?;
        lock.flush()?;
        Ok(())
    }

    fn has_images(&self) -> bool {
        self.screen.lines.iter().any(|l| l.image.is_some())
    }

    /// Draws every image that intersects the viewport, cropped to fit.
    fn draw_images(&mut self, out: &mut String, height: usize) {
        // Collect first: encoding borrows the store mutably.
        let mut requests: Vec<(usize, crate::render::ImagePlacement, u16, u16)> = Vec::new();
        for (index, line) in self.screen.lines.iter().enumerate() {
            let Some(placement) = &line.image else {
                continue;
            };
            let rows = placement.rows as usize;
            // Skip images entirely above or below the viewport.
            if index + rows <= self.top || index >= self.top + height {
                continue;
            }
            let skip_top = self.top.saturating_sub(index);
            let screen_row = index.saturating_sub(self.top);
            let remaining = rows - skip_top;
            let space = height - screen_row;
            let skip_bottom = remaining.saturating_sub(space);
            requests.push((
                screen_row,
                placement.clone(),
                skip_top as u16,
                skip_bottom as u16,
            ));
        }

        for (screen_row, placement, skip_top, skip_bottom) in requests {
            // Horizontal scrolling moves images with the text; once the indent
            // is off-screen there is nothing sensible to draw.
            let Some(column) = (placement.indent as usize).checked_sub(self.left) else {
                continue;
            };
            let request = ImageRequest {
                image: &placement.image,
                cols: placement.cols,
                rows: placement.rows,
                skip_top,
                skip_bottom,
                indent: column as u16,
            };
            if request.visible_rows() == 0 {
                continue;
            }
            if let Some(sequence) = self.store.encode(request) {
                out.push_str(&format!("\x1b[{};{}H", screen_row + 1, column + 1));
                out.push_str(&sequence);
            }
        }
    }

    fn draw_status(&self, out: &mut String, writer: &mut StyleWriter, width: usize, height: usize) {
        let theme = &self.settings.theme;
        out.push_str(&format!("\x1b[{};1H\x1b[K", height + 1));

        let left = match (&self.mode, &self.message) {
            (Mode::Searching { backward }, _) => {
                let sigil = if *backward { '?' } else { '/' };
                let count = self
                    .search
                    .as_ref()
                    .map(|s| {
                        if s.is_empty() && !s.query.is_empty() {
                            "  (no matches)".to_string()
                        } else if s.is_empty() {
                            String::new()
                        } else {
                            format!("  ({} matches)", s.len())
                        }
                    })
                    .unwrap_or_default();
                format!("{sigil}{}{count}", self.input)
            }
            (_, Some(message)) => message.clone(),
            _ => self.screen.title.clone().unwrap_or_else(|| {
                self.sources
                    .first()
                    .map(|s| s.name.clone())
                    .unwrap_or_default()
            }),
        };

        let percent = if self.max_top() == 0 {
            100
        } else {
            (self.top * 100 / self.max_top()).min(100)
        };
        let right = format!(
            "{}/{}  {percent}%  {}",
            (self.top + 1).min(self.screen.len().max(1)),
            self.screen.len(),
            "H help  q quit"
        );

        let gap = width.saturating_sub(
            crate::render::inline::display_width(&left)
                + crate::render::inline::display_width(&right)
                + 2,
        );
        let text = format!(" {left}{}{right} ", " ".repeat(gap));
        let text = crate::render::inline::truncate(&text, width, "…");

        writer.transition(theme.status_bar, out);
        out.push_str(&text);
        // Fill to the edge so the bar reads as a bar, not a sentence.
        let pad = width.saturating_sub(crate::render::inline::display_width(&text));
        out.push_str(&" ".repeat(pad));
        writer.reset(out);
    }

    fn panel_items(&self, panel: Panel) -> Vec<overlay::Item> {
        match panel {
            Panel::Contents => self
                .screen
                .headings
                .iter()
                .map(|h| overlay::Item {
                    // Indent by level so the shape of the document shows.
                    text: format!(
                        "{}{}",
                        "  ".repeat(h.level.saturating_sub(1) as usize),
                        h.text
                    ),
                    detail: String::new(),
                })
                .collect(),
            Panel::Links => self
                .screen
                .links
                .iter()
                .map(|url| overlay::Item {
                    text: url.clone(),
                    detail: String::new(),
                })
                .collect(),
            Panel::Help => overlay::HELP
                .iter()
                .map(|(keys, description)| overlay::Item {
                    text: (*keys).to_string(),
                    detail: (*description).to_string(),
                })
                .collect(),
        }
    }
}

/// The path behind a link, if it points at a Markdown file we can read.
///
/// URLs with a scheme are somebody else's problem, and so is a local file that
/// is not Markdown -- a PDF or an image should open in whatever handles it.
/// What termmd can open in place of the document on screen.
///
/// Markdown, wherever it is: a file, a directory to list, or a URL. A link to a
/// remote document is fetched here rather than handed to a browser, which is
/// the same bargain as naming a URL on the command line -- following a link is
/// asking for what is behind it. Everything else goes to the browser, as
/// before.
fn openable(target: &str) -> Option<Origin> {
    if target.is_empty() || target.starts_with("mailto:") {
        return None;
    }
    if let Some(url) = source::as_url(Path::new(target)) {
        // A URL with no extension could be anything, and fetching it to find
        // out would take the click away from the browser that should have it.
        return source::is_markdown(Path::new(url.split(['?', '#']).next()?))
            .then_some(Origin::Url(url));
    }
    if target.split_once("://").is_some() {
        return None;
    }
    let path = PathBuf::from(target.strip_prefix("file://").unwrap_or(target));
    if path.is_dir() {
        return Some(Origin::Directory(path));
    }
    (source::is_markdown(&path) && path.is_file()).then_some(Origin::File(path))
}

/// Writes spans, opening and closing OSC 8 hyperlinks as the link changes.
fn write_spans(
    out: &mut String,
    writer: &mut StyleWriter,
    spans: &[Span],
    screen: &Screen,
    hyperlinks: bool,
) {
    let mut current: Option<usize> = None;
    for span in spans {
        if hyperlinks && span.link != current {
            if current.is_some() {
                out.push_str("\x1b]8;;\x1b\\");
            }
            if let Some(url) = span.link.and_then(|i| screen.links.get(i)) {
                out.push_str(&format!(
                    "\x1b]8;;{}\x1b\\",
                    crate::render::hyperlink_uri(url)
                ));
            }
            current = span.link;
        }
        writer.transition(span.style, out);
        out.push_str(&span.text);
    }
    if hyperlinks && current.is_some() {
        out.push_str("\x1b]8;;\x1b\\");
    }
}

/// Puts the terminal into full-screen mode and guarantees it is restored.
///
/// Restoration also runs from a panic hook: leaving a terminal in raw mode with
/// no cursor is a genuinely bad way to fail.
struct TerminalGuard;

impl TerminalGuard {
    fn enter(mouse: bool) -> Result<Self> {
        if !std::io::stdout().is_terminal() {
            anyhow::bail!("the pager needs a terminal; use --no-pager when redirecting output");
        }
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = restore();
            previous(info);
        }));

        enable_raw_mode().context("entering raw mode")?;
        let mut stdout = std::io::stdout();
        execute!(stdout, EnterAlternateScreen, crossterm::cursor::Hide)?;
        stdout.write_all(mouse_mode(mouse).as_bytes())?;
        stdout.flush()?;
        Ok(Self)
    }
}

/// The escape sequences that put the terminal into one of our two mouse modes.
///
/// Reporting the mouse to the application takes click-drag selection away from
/// the terminal, and selecting text to copy it is something people need far more
/// often than they need to click a link. So the default is not to ask for the
/// mouse at all, and to turn on alternate scroll (DECSET 1007) instead: the
/// terminal then translates wheel events into arrow keys, which the pager
/// already understands. The wheel scrolls, drag selects, and the terminal
/// handles OSC 8 clicks itself.
///
/// With reporting on (`m`, or `mouse = true`), termmd sees clicks and can follow
/// links itself -- including in-document anchors, which the terminal cannot.
fn mouse_mode(reporting: bool) -> &'static str {
    if reporting {
        // Button tracking plus SGR coordinates, deliberately without motion
        // tracking: clicks and the wheel are all we need.
        "\x1b[?1007l\x1b[?1000h\x1b[?1006h"
    } else {
        "\x1b[?1006l\x1b[?1000l\x1b[?1007h"
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = restore();
    }
}

fn restore() -> Result<()> {
    let mut stdout = std::io::stdout();
    // Any images still on screen belong to the alternate screen; clear them
    // before leaving so they cannot bleed through.
    let _ = stdout.write_all(kitty::clear_all().as_bytes());
    // Turn off every mode we might have set, whichever one is currently active:
    // asking a terminal to disable something it never enabled is harmless, and
    // leaving mouse reporting on would be anything but.
    let _ = stdout.write_all(b"\x1b[?1006l\x1b[?1000l\x1b[?1007l");
    let _ = execute!(stdout, crossterm::cursor::Show, LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = stdout.flush();
    Ok(())
}

/// Watches files for changes, debounced.
struct FileWatcher {
    receiver: Option<std::sync::mpsc::Receiver<()>>,
    /// Kept alive: dropping the watcher stops the notifications.
    _watcher: Option<Box<dyn notify::Watcher + Send>>,
    pending: Option<Instant>,
}

impl FileWatcher {
    fn start(paths: &[PathBuf]) -> Self {
        if paths.is_empty() {
            return Self {
                receiver: None,
                _watcher: None,
                pending: None,
            };
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
            if let Ok(event) = event {
                if event.kind.is_modify() || event.kind.is_create() {
                    let _ = tx.send(());
                }
            }
        });
        let Ok(mut watcher) = watcher else {
            return Self {
                receiver: None,
                _watcher: None,
                pending: None,
            };
        };
        for path in paths {
            // Watch the containing directory: editors that write atomically
            // replace the file, which a file-level watch would stop following.
            // A directory listing watches itself, since what changes it is a
            // file appearing inside it.
            let target = if path.is_dir() {
                Some(path.as_path())
            } else {
                path.parent().filter(|p| !p.as_os_str().is_empty())
            };
            let _ = notify::Watcher::watch(
                &mut watcher,
                target.unwrap_or(std::path::Path::new(".")),
                notify::RecursiveMode::NonRecursive,
            );
        }
        Self {
            receiver: Some(rx),
            _watcher: Some(Box::new(watcher)),
            pending: None,
        }
    }

    /// True once changes have stopped arriving for the debounce interval.
    fn changed(&mut self) -> bool {
        let Some(rx) = &self.receiver else {
            return false;
        };
        while rx.try_recv().is_ok() {
            self.pending = Some(Instant::now());
        }
        match self.pending {
            Some(at) if at.elapsed() >= RELOAD_DEBOUNCE => {
                self.pending = None;
                true
            }
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::Line;
    use crate::term::style::Style;

    fn screen_of(count: usize) -> Screen {
        Screen {
            lines: (0..count)
                .map(|i| Line::from_spans(vec![Span::plain(format!("line {i}"))]))
                .collect(),
            ..Default::default()
        }
    }

    /// A pager over a synthetic screen, with no terminal attached.
    fn pager(lines: usize, rows: u16) -> Pager<'static> {
        // Leaked so the test pager can hold the borrows without a terminal.
        let highlighter: &'static Highlighter = Box::leak(Box::new(Highlighter::plain()));
        let caps = crate::term::caps::Capabilities::default();
        let store: &'static mut Store = Box::leak(Box::new(Store::new(
            &caps,
            ".",
            crate::images::RemotePolicy::Deny,
        )));
        let settings = Settings {
            caps,
            theme: crate::theme::Theme::mono(),
            render: Default::default(),
            parse: Default::default(),
            use_pager: true,
            mouse: false,
            styled: true,
            watch: false,
            remote_images: false,
            protocol: GraphicsProtocol::None,
            syntax_theme: String::new(),
            pager_forced: false,
            requested_width: None,
        };
        Pager {
            sources: Vec::new(),
            settings,
            highlighter,
            store,
            document: Document::default(),
            screen: screen_of(lines),
            top: 0,
            left: 0,
            cols: 80,
            rows,
            mode: Mode::Normal,
            search: None,
            input: String::new(),
            message: None,
            selection: 0,
            dirty: true,
            history: Vec::new(),
            quit: false,
        }
    }

    fn press(p: &mut Pager<'_>, code: KeyCode) {
        p.on_key(KeyEvent::new(code, KeyModifiers::NONE)).unwrap();
    }

    /// A pager showing a real document, laid out as the renderer would.
    fn pager_of(markdown: &str, rows: u16) -> Pager<'static> {
        let mut p = pager(0, rows);
        p.document = crate::markdown::parse(markdown, Default::default());
        p.rebuild();
        p
    }

    #[test]
    fn y_copies_the_code_block_on_screen() {
        // Padded so that the two blocks cannot be on screen together.
        let filler = "Paragraph.\n\n".repeat(20);
        let doc = format!("```sh\nfirst --block\n```\n\n{filler}```sh\nsecond --block\n```\n");
        let mut p = pager_of(&doc, 10);
        assert_eq!(
            p.visible_code().map(String::as_str),
            Some("first --block"),
            "the first block on screen"
        );

        // Scrolled past it, the answer is the next one down.
        press(&mut p, KeyCode::Char('G'));
        assert_eq!(p.visible_code().map(String::as_str), Some("second --block"));
    }

    #[test]
    fn y_says_so_when_there_is_no_code_on_screen() {
        let mut p = pager_of("# Just a heading\n\nAnd a paragraph.\n", 10);
        assert_eq!(p.visible_code(), None);
        press(&mut p, KeyCode::Char('y'));
        assert_eq!(p.message.as_deref(), Some("no code block on screen"));
    }

    #[test]
    fn what_is_copied_is_the_source_not_the_drawing() {
        // Line numbers, a background and syntax highlighting all change what is
        // on screen; none of them belong in a paste.
        let p = pager_of("```rust\nfn main() {\n    let x = 1;\n}\n```\n", 12);
        let code = p.visible_code().map(String::as_str);
        assert_eq!(code, Some("fn main() {\n    let x = 1;\n}"));
        // No trailing newline, so a shell block pasted into a prompt waits to
        // be read rather than running itself.
        assert!(!code.unwrap().ends_with('\n'));
    }

    #[test]
    fn scrolls_within_bounds() {
        let mut p = pager(100, 25);
        assert_eq!(p.viewport(), 24);
        press(&mut p, KeyCode::Char('j'));
        assert_eq!(p.top, 1);
        press(&mut p, KeyCode::Char('k'));
        assert_eq!(p.top, 0);
        press(&mut p, KeyCode::Char('k'));
        assert_eq!(p.top, 0, "must not scroll above the first line");
    }

    #[test]
    fn cannot_scroll_past_the_last_screenful() {
        let mut p = pager(100, 25);
        press(&mut p, KeyCode::Char('G'));
        assert_eq!(p.top, 100 - 24);
        press(&mut p, KeyCode::Char('j'));
        assert_eq!(p.top, 100 - 24, "the end is the end");
    }

    #[test]
    fn a_short_document_does_not_scroll() {
        let mut p = pager(3, 25);
        press(&mut p, KeyCode::Char('G'));
        assert_eq!(p.top, 0);
        assert_eq!(p.max_top(), 0);
    }

    #[test]
    fn paging_moves_by_almost_a_screen() {
        let mut p = pager(500, 25);
        press(&mut p, KeyCode::Char(' '));
        // Two rows of overlap keep the reader's place.
        assert_eq!(p.top, 22);
    }

    #[test]
    fn quits_on_q_and_escape() {
        let mut p = pager(10, 25);
        press(&mut p, KeyCode::Char('q'));
        assert!(p.quit);

        let mut p = pager(10, 25);
        press(&mut p, KeyCode::Esc);
        assert!(p.quit);
    }

    #[test]
    fn search_is_incremental_and_jumps_to_matches() {
        let mut p = pager(100, 25);
        press(&mut p, KeyCode::Char('/'));
        assert!(matches!(p.mode, Mode::Searching { .. }));
        for c in "line 42".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        assert_eq!(p.search.as_ref().unwrap().len(), 1);
        press(&mut p, KeyCode::Enter);
        assert_eq!(p.mode, Mode::Normal);
        assert!(
            p.top <= 42 && p.top + p.viewport() > 42,
            "match should be on screen"
        );
    }

    #[test]
    fn escape_cancels_a_search() {
        let mut p = pager(50, 25);
        press(&mut p, KeyCode::Char('/'));
        press(&mut p, KeyCode::Char('x'));
        press(&mut p, KeyCode::Esc);
        assert_eq!(p.mode, Mode::Normal);
        assert!(p.search.is_none());
    }

    #[test]
    fn n_moves_between_matches() {
        let mut p = pager(100, 25);
        press(&mut p, KeyCode::Char('/'));
        for c in "line 1".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        press(&mut p, KeyCode::Enter);
        let first = p.search.as_ref().unwrap().focused().unwrap().line;
        press(&mut p, KeyCode::Char('n'));
        let second = p.search.as_ref().unwrap().focused().unwrap().line;
        assert_ne!(first, second);
    }

    #[test]
    fn reports_when_a_search_finds_nothing() {
        let mut p = pager(10, 25);
        press(&mut p, KeyCode::Char('/'));
        for c in "zzzz".chars() {
            press(&mut p, KeyCode::Char(c));
        }
        press(&mut p, KeyCode::Enter);
        assert!(p.message.as_deref().unwrap().contains("not found"));
    }

    #[test]
    fn horizontal_scrolling_is_bounded() {
        let mut p = pager(10, 25);
        p.screen.lines[0] = Line::from_spans(vec![Span::plain("x".repeat(200))]);
        for _ in 0..100 {
            press(&mut p, KeyCode::Char('l'));
        }
        assert_eq!(p.left, 200 - 80);
        press(&mut p, KeyCode::Char('0'));
        assert_eq!(p.left, 0);
    }

    #[test]
    fn panels_open_and_close() {
        let mut p = pager(10, 25);
        press(&mut p, KeyCode::Char('H'));
        assert_eq!(p.mode, Mode::Panel(Panel::Help));
        press(&mut p, KeyCode::Esc);
        assert_eq!(p.mode, Mode::Normal);
    }

    #[test]
    fn the_contents_panel_starts_at_the_nearest_heading() {
        let mut p = pager(100, 25);
        p.screen.headings = vec![
            crate::render::HeadingEntry {
                level: 1,
                text: "A".into(),
                id: "a".into(),
                line: 0,
            },
            crate::render::HeadingEntry {
                level: 1,
                text: "B".into(),
                id: "b".into(),
                line: 40,
            },
            crate::render::HeadingEntry {
                level: 1,
                text: "C".into(),
                id: "c".into(),
                line: 80,
            },
        ];
        p.top = 45;
        press(&mut p, KeyCode::Char('t'));
        assert_eq!(
            p.selection, 1,
            "should select the heading the reader is under"
        );
    }

    #[test]
    fn selecting_a_heading_jumps_to_it() {
        let mut p = pager(100, 25);
        p.screen.headings = vec![crate::render::HeadingEntry {
            level: 1,
            text: "Target".into(),
            id: "target".into(),
            line: 60,
        }];
        press(&mut p, KeyCode::Char('t'));
        press(&mut p, KeyCode::Enter);
        assert_eq!(p.mode, Mode::Normal);
        assert!(p.top <= 60 && p.top + p.viewport() > 60);
    }

    #[test]
    fn anchor_links_jump_instead_of_opening_a_browser() {
        let mut p = pager(100, 25);
        p.screen.anchors.insert("section".into(), 55);
        p.follow("#section").unwrap();
        assert!(p.top <= 55 && p.top + p.viewport() > 55);
        assert!(p.message.is_none(), "a successful jump needs no message");
    }

    #[test]
    fn unknown_anchors_say_so_rather_than_opening_anything() {
        let mut p = pager(100, 25);
        p.follow("#nowhere").unwrap();
        assert!(p.message.as_deref().unwrap().contains("nowhere"));
        assert_eq!(p.top, 0);
    }

    #[test]
    fn clicking_a_link_span_finds_its_target() {
        let mut p = pager(100, 25);
        p.screen.links = vec!["https://example.com".into()];
        p.screen.lines[2] = Line::from_spans(vec![
            Span::plain("see "),
            Span::new("here", Style::PLAIN, Some(0)),
            Span::plain(" now"),
        ]);
        // "here" occupies columns 4..8 on screen row 2.
        assert_eq!(p.link_at(2, 5), Some(0));
        assert_eq!(p.link_at(2, 0), None, "plain text is not a link");
        assert_eq!(p.link_at(2, 9), None, "past the link is not a link");
        assert_eq!(p.link_at(3, 5), None, "wrong row");
    }

    #[test]
    fn clicking_accounts_for_scrolling() {
        let mut p = pager(100, 25);
        p.screen.links = vec!["https://example.com".into()];
        p.screen.lines[40] = Line::from_spans(vec![Span::new("link", Style::PLAIN, Some(0))]);
        p.top = 38;
        assert_eq!(p.link_at(2, 1), Some(0), "row is relative to the viewport");

        p.left = 2;
        assert_eq!(
            p.link_at(2, 0),
            Some(0),
            "column accounts for horizontal scroll"
        );
    }

    #[test]
    fn clicks_on_the_status_bar_are_ignored() {
        let mut p = pager(100, 25);
        p.screen.links = vec!["x".into()];
        p.screen.lines[24] = Line::from_spans(vec![Span::new("link", Style::PLAIN, Some(0))]);
        assert_eq!(p.link_at(24, 1), None, "row 24 is the status bar");
    }

    #[test]
    fn local_markdown_links_are_recognised() {
        let dir = std::env::temp_dir().join("termmd-follow-test");
        std::fs::create_dir_all(&dir).unwrap();
        let md = dir.join("other.md");
        std::fs::write(&md, "# Other\n").unwrap();
        std::fs::write(dir.join("thing.pdf"), "x").unwrap();

        assert_eq!(
            openable(md.to_str().unwrap()),
            Some(Origin::File(md.clone()))
        );
        assert_eq!(
            openable(dir.join("thing.pdf").to_str().unwrap()),
            None,
            "not markdown"
        );
        assert_eq!(
            openable(dir.join("missing.md").to_str().unwrap()),
            None,
            "not a file"
        );
        assert_eq!(openable("mailto:someone@example.com"), None);
        assert_eq!(openable(""), None);

        // A directory is opened as an index of what is in it.
        assert_eq!(
            openable(dir.to_str().unwrap()),
            Some(Origin::Directory(dir.clone()))
        );

        // Remote Markdown is fetched; anything else at a URL is the browser's.
        assert_eq!(
            openable("https://example.com/a.md"),
            Some(Origin::Url("https://example.com/a.md".into()))
        );
        assert_eq!(
            openable("https://example.com/docs/guide.markdown#top"),
            Some(Origin::Url(
                "https://example.com/docs/guide.markdown#top".into()
            ))
        );
        assert_eq!(openable("https://example.com/"), None, "not markdown");
        assert_eq!(openable("https://example.com/page.html"), None);
        assert_eq!(openable("ftp://example.com/a.md"), None, "not fetchable");
    }

    #[test]
    fn following_a_local_document_navigates_and_comes_back() {
        let dir = std::env::temp_dir().join("termmd-nav-test");
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("other.md");
        std::fs::write(&other, "# The Other Document\n\nBody.\n").unwrap();
        let first = dir.join("first.md");
        std::fs::write(&first, "# First\n").unwrap();

        let mut p = pager(100, 25);
        p.sources = vec![Source {
            origin: Origin::File(first.clone()),
            name: "first.md".into(),
            text: "# First\n".into(),
        }];
        p.top = 7;

        p.follow(other.to_str().unwrap()).unwrap();
        assert_eq!(p.sources[0].path(), Some(other.as_path()));
        assert!(p.screen.plain_text().contains("The Other Document"));
        assert_eq!(p.top, 0, "a new document starts at the top");
        assert_eq!(p.history.len(), 1);

        p.go_back();
        assert_eq!(p.sources[0].path(), Some(first.as_path()));
        assert!(p.history.is_empty());
    }

    #[test]
    fn a_document_link_with_a_fragment_lands_on_the_heading() {
        let dir = std::env::temp_dir().join("termmd-frag-test");
        std::fs::create_dir_all(&dir).unwrap();
        let other = dir.join("other.md");
        std::fs::write(&other, "# Top\n\nfiller\n\n## Deep Section\n\nmore\n").unwrap();

        let mut p = pager(100, 25);
        p.follow(&format!("{}#deep-section", other.display()))
            .unwrap();
        let line = p
            .screen
            .anchor("deep-section")
            .expect("anchor should exist");
        assert!(
            p.top <= line && p.top + p.viewport() > line,
            "should land on the heading"
        );
    }

    #[test]
    fn going_back_with_no_history_says_so() {
        let mut p = pager(10, 25);
        p.go_back();
        assert!(p.message.as_deref().unwrap().contains("nothing to go back"));
    }

    #[test]
    fn the_default_mouse_mode_leaves_selection_to_the_terminal() {
        let terminal_owns = mouse_mode(false);
        assert!(terminal_owns.contains("?1000l"), "must not report clicks");
        assert!(
            terminal_owns.contains("?1007h"),
            "but the wheel should still scroll"
        );

        let termmd_owns = mouse_mode(true);
        assert!(termmd_owns.contains("?1000h") && termmd_owns.contains("?1006h"));
        assert!(
            termmd_owns.contains("?1007l"),
            "alternate scroll would double up"
        );
        assert!(
            !termmd_owns.contains("?1003h"),
            "motion tracking is never wanted"
        );
    }

    #[test]
    fn m_toggles_which_side_owns_the_mouse() {
        let mut p = pager(10, 25);
        assert!(!p.settings.mouse, "the terminal owns the mouse by default");
        // Writes escape sequences to stdout, which is harmless under test.
        press(&mut p, KeyCode::Char('m'));
        assert!(p.settings.mouse);
        assert!(p.message.as_deref().unwrap().contains("termmd"));
        press(&mut p, KeyCode::Char('m'));
        assert!(!p.settings.mouse);
        assert!(p.message.as_deref().unwrap().contains("select"));
    }

    #[test]
    fn key_releases_are_ignored() {
        let mut p = pager(100, 25);
        let mut key = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE);
        key.kind = KeyEventKind::Release;
        p.on_key(key).unwrap();
        assert_eq!(p.top, 0, "a release must not scroll");
    }

    #[test]
    fn ctrl_c_quits() {
        let mut p = pager(10, 25);
        p.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(p.quit);
    }
}
