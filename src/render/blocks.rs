//! Block layout: the part that decides what a document actually looks like.

use std::collections::HashMap;

use super::code::Highlighter;
use super::inline::{Align, Kind, Token, display_width, tokenize_text, wrap};
use super::table;
use super::{
    HeadingEntry, ImagePlacement, ImageProvider, Line, LinkMode, RenderOptions, Screen, Span,
};
use crate::markdown::{
    AlertKind, Block, Document, ImageRef, Inline, Inlines, List, ir_plain_text as plain_text,
};
use crate::term::caps::{Capabilities, GraphicsProtocol};
use crate::term::style::{Color, Style};
use crate::theme::Theme;

/// Shared state for one render pass.
pub(super) struct Ctx<'a> {
    pub opts: &'a RenderOptions,
    pub theme: &'a Theme,
    pub caps: &'a Capabilities,
    pub images: &'a mut dyn ImageProvider,
    pub highlighter: &'a Highlighter,
    /// Link destinations, in first-use order.
    pub links: Vec<String>,
    pub headings: Vec<HeadingEntry>,
    /// Footnote label to its printed number.
    pub footnote_numbers: HashMap<String, usize>,
    /// Links to list at the end, in [`LinkMode::Reference`].
    pub reference_links: Vec<(usize, String)>,
}

impl Ctx<'_> {
    pub(super) fn glyphs(&self) -> super::Glyphs {
        self.opts.glyphs
    }

    /// Registers a link destination and returns its index.
    fn link_id(&mut self, dest: &str) -> usize {
        if let Some(i) = self.links.iter().position(|l| l == dest) {
            return i;
        }
        self.links.push(dest.to_string());
        self.links.len() - 1
    }

    /// Whether to attach OSC 8 hyperlinks to link spans.
    fn use_osc8(&self) -> bool {
        match self.opts.links {
            LinkMode::Auto => self.caps.hyperlinks,
            LinkMode::Hyperlink => true,
            _ => false,
        }
    }
}

pub(super) fn render_document(
    doc: &Document,
    opts: &RenderOptions,
    theme: &Theme,
    caps: &Capabilities,
    images: &mut dyn ImageProvider,
    highlighter: &Highlighter,
) -> Screen {
    let footnote_numbers: HashMap<String, usize> = doc
        .footnotes
        .iter()
        .enumerate()
        .map(|(i, (label, _))| (label.clone(), i + 1))
        .collect();

    let mut ctx = Ctx {
        opts,
        theme,
        caps,
        images,
        highlighter,
        links: Vec::new(),
        headings: Vec::new(),
        footnote_numbers,
        reference_links: Vec::new(),
    };

    let content_width = opts.width.saturating_sub(opts.margin * 2).max(8);
    let mut lines = render_blocks(&doc.blocks, &mut ctx, content_width);

    if !doc.footnotes.is_empty() {
        lines.extend(render_footnotes(doc, &mut ctx, content_width));
    }
    if !ctx.reference_links.is_empty() {
        lines.extend(render_reference_links(&mut ctx, content_width));
    }

    trim_blank_edges(&mut lines);

    // Apply the left margin once, at the end, so every layout decision above
    // could work in a simple zero-based coordinate space.
    if opts.margin > 0 {
        let pad = " ".repeat(opts.margin);
        for line in &mut lines {
            if !line.spans.is_empty() || line.image.is_some() {
                line.prefix(vec![Span::plain(pad.clone())]);
            }
        }
    }

    // Headings know their line numbers only now that everything is placed.
    let mut anchors = HashMap::new();
    let mut headings = ctx.headings;
    for (i, line) in lines.iter().enumerate() {
        if let Some(idx) = line.anchor {
            if let Some(h) = headings.get_mut(idx) {
                h.line = i;
                anchors.insert(h.id.clone(), i);
            }
        }
    }

    Screen {
        lines,
        links: ctx.links,
        headings,
        anchors,
        width: opts.width,
        title: doc.title.clone(),
    }
}

/// Renders a sequence of blocks, separating them with blank lines.
fn render_blocks(blocks: &[Block], ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for block in blocks {
        let rendered = render_block(block, ctx, width);
        if rendered.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(rendered);
    }
    out
}

fn render_block(block: &Block, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    match block {
        Block::Heading { level, id, content } => heading(*level, id, content, ctx, width),
        Block::Paragraph(inlines) => {
            let tokens = inline_tokens(inlines, ctx, ctx.theme.text);
            wrap(&tokens, width, Align::Left)
        }
        Block::CodeBlock { language, code } => code_block(language.as_deref(), code, ctx, width),
        Block::BlockQuote { kind, blocks } => quote(*kind, blocks, ctx, width),
        Block::List(list) => render_list(list, ctx, width, 0),
        Block::Table(t) => table::render(t, ctx, width),
        Block::Figure { image, caption } => figure(image, caption, ctx, width),
        Block::Rule => vec![Line::from_spans(vec![Span::new(
            ctx.glyphs().rule.repeat(width),
            ctx.theme.rule,
            None,
        )])],
        Block::Html(raw) => {
            let tokens = vec![Token::word(raw.trim(), ctx.theme.muted, None)];
            wrap(&tokens, width, Align::Left)
        }
        Block::DisplayMath(text) => display_math(text, ctx, width),
        Block::DefinitionList(items) => definition_list(items, ctx, width),
        Block::FootnoteDefinition { label, blocks } => {
            // Only reached for footnote syntax we could not lift out; render in
            // place rather than dropping the content.
            let mut lines = render_blocks(blocks, ctx, width.saturating_sub(4));
            indent(&mut lines, 4);
            let mut head = Line::from_spans(vec![Span::new(
                format!("[{label}]"),
                ctx.theme.footnote_ref,
                None,
            )]);
            head.anchor = None;
            let mut out = vec![head];
            out.append(&mut lines);
            out
        }
    }
}

fn heading(level: u8, id: &str, content: &Inlines, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let style = ctx.theme.heading(level);
    let marks = format!("{} ", "#".repeat(level as usize));
    let mark_width = display_width(&marks);
    let text_width = width.saturating_sub(mark_width).max(4);

    let tokens = inline_tokens(content, ctx, style);
    let mut lines = wrap(&tokens, text_width, Align::Left);

    let index = ctx.headings.len();
    ctx.headings.push(HeadingEntry {
        level,
        text: plain_text(content),
        id: id.to_string(),
        line: 0,
    });

    let mark_style = ctx.theme.muted.merge(Style::PLAIN);
    for (i, line) in lines.iter_mut().enumerate() {
        let prefix = if i == 0 {
            Span::new(marks.clone(), mark_style, None)
        } else {
            Span::plain(" ".repeat(mark_width))
        };
        line.prefix(vec![prefix]);
    }
    lines[0].anchor = Some(index);

    // A rule under the top two levels gives the page a visible spine when
    // scrolling, which matters more in a terminal than on a web page.
    if ctx.opts.heading_rules && level <= 2 {
        let rule_width = lines
            .iter()
            .map(|l| l.width())
            .max()
            .unwrap_or(0)
            .min(width);
        if rule_width > 0 {
            lines.push(Line::from_spans(vec![Span::new(
                ctx.glyphs().heading_rule.repeat(rule_width),
                ctx.theme.heading_rule,
                None,
            )]));
        }
    }
    lines
}

fn quote(kind: Option<AlertKind>, blocks: &[Block], ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let bar = ctx.glyphs().quote_bar;
    let bar_width = display_width(bar) + 1;
    let inner_width = width.saturating_sub(bar_width).max(8);

    let (bar_style, mut lines) = match kind {
        Some(k) => {
            let style = ctx.theme.alert(k);
            let mut lines = vec![Line::from_spans(vec![Span::new(k.label(), style, None)])];
            lines.extend(render_blocks(blocks, ctx, inner_width));
            (style, lines)
        }
        None => {
            let mut lines = render_blocks(blocks, ctx, inner_width);
            tint(&mut lines, ctx.theme.quote_text);
            (ctx.theme.quote_bar, lines)
        }
    };

    for line in &mut lines {
        line.prefix(vec![Span::new(bar, bar_style, None), Span::plain(" ")]);
    }
    lines
}

fn render_list(list: &List, ctx: &mut Ctx, width: usize, depth: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    // Ordered lists reserve room for their widest number so the text of every
    // item starts in the same column.
    let marker_width = match list.start {
        Some(start) => {
            let last = start
                .saturating_add(list.items.len() as u64)
                .saturating_sub(1);
            display_width(&format!("{last}.")) + 1
        }
        None => display_width(ctx.glyphs().bullet(depth)) + 1,
    };
    let inner_width = width.saturating_sub(marker_width).max(8);

    for (i, item) in list.items.iter().enumerate() {
        let mut lines = render_item_blocks(&item.blocks, ctx, inner_width, depth);

        if let Some(done) = item.task {
            let g = ctx.glyphs();
            let (glyph, style) = if done {
                (g.task_done, ctx.theme.task_done)
            } else {
                (g.task_todo, ctx.theme.task_todo)
            };
            if done {
                tint(&mut lines, ctx.theme.task_done_text);
            }
            let marker = format!("[{glyph}] ");
            prefix_first_line(&mut lines, marker, style, display_width("[x] "));
        } else {
            let (marker, style) = match list.start {
                Some(start) => (
                    format!("{}. ", start.saturating_add(i as u64)),
                    ctx.theme.number,
                ),
                None => (format!("{} ", ctx.glyphs().bullet(depth)), ctx.theme.bullet),
            };
            prefix_first_line(&mut lines, marker, style, marker_width);
        }

        if !out.is_empty() && !list.tight {
            out.push(Line::default());
        }
        out.extend(lines);
    }
    out
}

/// Renders the blocks of one list item, keeping nested lists at the right depth.
fn render_item_blocks(blocks: &[Block], ctx: &mut Ctx, width: usize, depth: usize) -> Vec<Line> {
    let mut out: Vec<Line> = Vec::new();
    for block in blocks {
        let rendered = match block {
            Block::List(nested) => render_list(nested, ctx, width, depth + 1),
            other => render_block(other, ctx, width),
        };
        if rendered.is_empty() {
            continue;
        }
        // A nested list follows its parent item directly; other blocks get air.
        if !out.is_empty() && !matches!(block, Block::List(_)) {
            out.push(Line::default());
        }
        out.extend(rendered);
    }
    out
}

/// Puts `marker` on the first line and aligns the rest under the text.
fn prefix_first_line(lines: &mut [Line], marker: String, style: Style, width: usize) {
    let pad = width.saturating_sub(display_width(&marker));
    for (i, line) in lines.iter_mut().enumerate() {
        if i == 0 {
            let mut spans = vec![Span::new(marker.clone(), style, None)];
            if pad > 0 {
                spans.push(Span::plain(" ".repeat(pad)));
            }
            line.prefix(spans);
        } else {
            line.prefix(vec![Span::plain(" ".repeat(width))]);
        }
    }
}

fn code_block(language: Option<&str>, code: &str, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let expanded = expand_tabs(code, ctx.opts.tab_width);
    let highlighted = ctx.highlighter.highlight(&expanded, language);

    let bg = ctx.theme.code_block_bg;
    let bg_style = bg
        .map(|c| Style::PLAIN.bg(Color::Rgb(c)))
        .unwrap_or(Style::PLAIN);

    let gutter_width = if ctx.opts.line_numbers {
        display_width(&highlighted.len().to_string()) + 1
    } else {
        0
    };
    // One column of padding on each side keeps text off the background edge.
    let text_width = width.saturating_sub(gutter_width + 2).max(8);

    let mut out = Vec::new();
    for (i, pieces) in highlighted.iter().enumerate() {
        // A highlighted line may be wider than the block; wrap it rather than
        // cutting code off, and mark continuations so the wrap is visible.
        let mut tokens: Vec<Token> = Vec::new();
        for (style, text) in pieces {
            let style = if bg.is_some() {
                bg_style.merge(*style)
            } else {
                *style
            };
            for chunk in super::inline::split_to_width(text, text_width) {
                tokens.push(Token {
                    text: chunk,
                    style,
                    link: None,
                    kind: Kind::Word,
                });
            }
        }
        let mut wrapped = pack_code_line(&tokens, text_width);
        if wrapped.is_empty() {
            wrapped.push(Line::default());
        }

        for (j, line) in wrapped.iter_mut().enumerate() {
            let mut prefix = Vec::new();
            if gutter_width > 0 {
                let label = if j == 0 {
                    (i + 1).to_string()
                } else {
                    String::new()
                };
                let pad = gutter_width.saturating_sub(display_width(&label) + 1);
                prefix.push(Span::new(
                    format!("{}{} ", " ".repeat(pad), label),
                    bg_style.merge(ctx.theme.code_gutter),
                    None,
                ));
            }
            prefix.push(Span::new(
                if j == 0 {
                    " ".into()
                } else {
                    ctx.glyphs().continuation.to_string()
                },
                if j == 0 {
                    bg_style
                } else {
                    bg_style.merge(ctx.theme.code_gutter)
                },
                None,
            ));
            line.prefix(prefix);
            super::pad_line_background(line, width, bg_style);
        }
        out.extend(wrapped);
    }

    if out.is_empty() {
        let mut blank = Line::default();
        super::pad_line_background(&mut blank, width, bg_style);
        out.push(blank);
    }

    // A dim language tag above the block, where it cannot be selected along
    // with the code itself.
    if let Some(lang) = language.filter(|_| ctx.highlighter.can_highlight(language, code)) {
        let mut tag = Line::from_spans(vec![Span::new(lang.to_string(), ctx.theme.muted, None)]);
        tag.align(width, Align::Right);
        out.insert(0, tag);
    }
    out
}

/// Lays out already-sized code tokens without re-flowing them like prose.
fn pack_code_line(tokens: &[Token], width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut spans: Vec<Span> = Vec::new();
    let mut used = 0;
    for t in tokens {
        let w = t.width();
        if used + w > width && used > 0 {
            lines.push(Line::from_spans(std::mem::take(&mut spans)));
            used = 0;
        }
        spans.push(Span::new(t.text.clone(), t.style, None));
        used += w;
    }
    if !spans.is_empty() || lines.is_empty() {
        lines.push(Line::from_spans(spans));
    }
    lines
}

fn expand_tabs(s: &str, tab_width: usize) -> String {
    if !s.contains('\t') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    for line in s.split_inclusive('\n') {
        let mut col = 0;
        for c in line.chars() {
            if c == '\t' {
                let n = tab_width - (col % tab_width);
                out.push_str(&" ".repeat(n));
                col += n;
            } else {
                out.push(c);
                col += display_width(&c.to_string());
                if c == '\n' {
                    col = 0;
                }
            }
        }
    }
    out
}

fn figure(image: &ImageRef, caption: &Inlines, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let placement = ctx
        .opts
        .images
        .then(|| measure_image(image, ctx, width))
        .flatten();

    let Some((cols, rows)) = placement else {
        // No picture: a labelled, clickable placeholder that says why. A reader
        // who cannot tell a blocked download from an unreadable format has no
        // idea whether the problem is theirs to fix.
        let alt = if image.alt.is_empty() {
            image.url.as_str()
        } else {
            image.alt.as_str()
        };
        let link = ctx.link_id(&image.url);
        let marker = ctx.glyphs().image_marker;
        let mut tokens = vec![
            Token::word(format!("{marker} "), ctx.theme.caption, None),
            Token::word(
                alt,
                ctx.theme.link_text,
                Some(link).filter(|_| ctx.use_osc8()),
            ),
        ];
        // Only ask when images are switched on: with them off there is no
        // failure to explain, and asking could start a download nobody wanted.
        if ctx.opts.images {
            if let Some(reason) = ctx.images.problem(image) {
                tokens.push(Token::space(ctx.theme.muted));
                tokens.push(Token::word(format!("({reason})"), ctx.theme.muted, None));
            }
        } else if !ctx.use_osc8() {
            tokens.push(Token::space(ctx.theme.muted));
            tokens.push(Token::word(
                format!("({})", image.url),
                ctx.theme.link_url,
                None,
            ));
        }
        return wrap(&tokens, width, Align::Left);
    };

    let indent = if ctx.opts.center_figures {
        ((width.saturating_sub(cols as usize)) / 2) as u16
    } else {
        0
    };

    let mut anchor = Line {
        image: Some(ImagePlacement {
            image: image.clone(),
            cols,
            rows,
            indent,
        }),
        ..Default::default()
    };
    // The anchor line carries the alt text as its plain-text form, so search and
    // `--plain` still see something meaningful where the picture is.
    if !image.alt.is_empty() {
        anchor.spans = Vec::new();
    }
    let mut out = vec![anchor];
    for _ in 1..rows {
        out.push(Line {
            image_filler: true,
            ..Default::default()
        });
    }

    if !caption.is_empty() {
        let tokens = inline_tokens(caption, ctx, ctx.theme.caption);
        let align = if ctx.opts.center_figures {
            Align::Center
        } else {
            Align::Left
        };
        out.extend(wrap(&tokens, width, align));
    }
    out
}

/// Works out how many cells an image should occupy.
///
/// Terminals address pixels through cells, so this is the one place that has to
/// reason in both units: we scale by pixel size, then round up to whole cells and
/// clamp to the space available.
fn measure_image(image: &ImageRef, ctx: &mut Ctx, width: usize) -> Option<(u16, u16)> {
    let (px_w, px_h) = ctx.images.measure(image)?;
    if px_w == 0 || px_h == 0 {
        return None;
    }
    let (cell_w, cell_h) = ctx.caps.cell_px_or_guess();
    let (cell_w, cell_h) = (cell_w.max(1) as f64, cell_h.max(1) as f64);

    let max_cols = width.max(1) as f64;
    let max_rows = ctx.opts.max_image_rows.max(1) as f64;

    // Natural size in cells, then shrink to fit both limits without distorting.
    let nat_cols = px_w as f64 / cell_w;
    let nat_rows = px_h as f64 / cell_h;
    let scale = (max_cols / nat_cols).min(max_rows / nat_rows).min(1.0);

    let cols = (nat_cols * scale).round().max(1.0) as u16;
    let rows = (nat_rows * scale).round().max(1.0) as u16;

    // The blocks backend packs two pixel rows into one cell, so it needs at
    // least two rows to show anything at all.
    if ctx.caps.graphics == GraphicsProtocol::Blocks && rows < 2 {
        return Some((cols, 2));
    }
    Some((cols.min(width as u16), rows))
}

fn display_math(text: &str, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    for raw in text.trim().lines() {
        let tokens = vec![Token::word(raw.trim(), ctx.theme.math, None)];
        lines.extend(wrap(&tokens, width, Align::Center));
    }
    lines
}

fn definition_list(items: &[(Inlines, Vec<Vec<Block>>)], ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for (term, definitions) in items {
        if !out.is_empty() {
            out.push(Line::default());
        }
        let tokens = inline_tokens(term, ctx, ctx.theme.strong);
        out.extend(wrap(&tokens, width, Align::Left));
        for blocks in definitions {
            let mut lines = render_blocks(blocks, ctx, width.saturating_sub(4));
            indent(&mut lines, 4);
            out.extend(lines);
        }
    }
    out
}

fn render_footnotes(doc: &Document, ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let mut out = vec![Line::default()];
    out.push(Line::from_spans(vec![Span::new(
        ctx.glyphs().rule.repeat(width.min(24)),
        ctx.theme.rule,
        None,
    )]));

    for (label, blocks) in &doc.footnotes {
        let number = ctx.footnote_numbers.get(label).copied().unwrap_or(0);
        let marker = format!("[{number}] ");
        let marker_width = display_width(&marker);
        let mut lines = render_blocks(blocks, ctx, width.saturating_sub(marker_width));
        if lines.is_empty() {
            lines.push(Line::default());
        }
        prefix_first_line(&mut lines, marker, ctx.theme.footnote_ref, marker_width);
        out.push(Line::default());
        out.extend(lines);
    }
    out
}

fn render_reference_links(ctx: &mut Ctx, width: usize) -> Vec<Line> {
    let mut out = vec![Line::default()];
    out.push(Line::from_spans(vec![Span::new(
        ctx.glyphs().rule.repeat(width.min(24)),
        ctx.theme.rule,
        None,
    )]));
    let refs = std::mem::take(&mut ctx.reference_links);
    for (number, url) in refs {
        let marker = format!("[{number}] ");
        let marker_width = display_width(&marker);
        let tokens = vec![Token::word(url, ctx.theme.link_url, None)];
        let mut lines = wrap(&tokens, width.saturating_sub(marker_width), Align::Left);
        prefix_first_line(&mut lines, marker, ctx.theme.footnote_ref, marker_width);
        out.extend(lines);
    }
    out
}

/// Converts inline content into styled tokens.
pub(super) fn inline_tokens(inlines: &Inlines, ctx: &mut Ctx, base: Style) -> Vec<Token> {
    let mut out = Vec::new();
    push_inlines(inlines, ctx, base, None, &mut out);
    out
}

fn push_inlines(
    inlines: &Inlines,
    ctx: &mut Ctx,
    style: Style,
    link: Option<usize>,
    out: &mut Vec<Token>,
) {
    for inline in inlines {
        push_inline(inline, ctx, style, link, out);
    }
}

fn push_inline(
    inline: &Inline,
    ctx: &mut Ctx,
    style: Style,
    link: Option<usize>,
    out: &mut Vec<Token>,
) {
    match inline {
        Inline::Text(t) => tokenize_text(t, style, link, out),
        Inline::Code(t) => {
            let s = style.merge(ctx.theme.inline_code);
            // Inline code keeps its interior spaces, so it is one token.
            out.push(Token::word(t.clone(), s, link));
        }
        Inline::Emph(c) => push_inlines(c, ctx, style.merge(ctx.theme.emphasis), link, out),
        Inline::Strong(c) => push_inlines(c, ctx, style.merge(ctx.theme.strong), link, out),
        Inline::Strike(c) => push_inlines(c, ctx, style.merge(ctx.theme.strikethrough), link, out),
        Inline::Superscript(c) => {
            out.push(Token::word("^", ctx.theme.muted, link));
            push_inlines(c, ctx, style, link, out);
        }
        Inline::Subscript(c) => {
            out.push(Token::word("_", ctx.theme.muted, link));
            push_inlines(c, ctx, style, link, out);
        }
        Inline::Math { text, .. } => {
            out.push(Token::word(text.clone(), style.merge(ctx.theme.math), link))
        }
        Inline::Html(_) => {}
        Inline::SoftBreak => out.push(Token::space(style)),
        Inline::HardBreak => out.push(Token::hard_break()),
        Inline::FootnoteRef(label) => {
            let number = ctx.footnote_numbers.get(label).copied();
            let text = match number {
                Some(n) => format!("[{n}]"),
                None => format!("[{label}]"),
            };
            out.push(Token::word(text, ctx.theme.footnote_ref, link));
        }
        Inline::Image(img) => {
            // An image inside a sentence becomes its alt text: there is nowhere
            // to put a picture mid-line without breaking the flow.
            let label = if img.alt.is_empty() {
                "image"
            } else {
                img.alt.as_str()
            };
            let id = ctx.link_id(&img.url);
            let marker = ctx.glyphs().image_marker;
            out.push(Token::word(
                format!("{marker} {label}"),
                style.merge(ctx.theme.caption),
                Some(id).filter(|_| ctx.use_osc8()),
            ));
        }
        Inline::Link { dest, content, .. } => push_link(dest, content, ctx, style, out),
    }
}

fn push_link(dest: &str, content: &Inlines, ctx: &mut Ctx, style: Style, out: &mut Vec<Token>) {
    let id = ctx.link_id(dest);
    let text_style = style.merge(ctx.theme.link_text);
    let osc8 = ctx.use_osc8();
    let link = osc8.then_some(id);

    push_inlines(content, ctx, text_style, link, out);

    let text = plain_text(content);
    // An autolink already shows its destination; repeating it is just noise.
    let is_autolink = text == dest || format!("mailto:{text}") == dest;

    match ctx.opts.links {
        LinkMode::Hide | LinkMode::Hyperlink => {}
        LinkMode::Auto if osc8 => {}
        LinkMode::Auto | LinkMode::Inline => {
            if !is_autolink && !dest.is_empty() {
                out.push(Token::space(style));
                out.push(Token::word(format!("({dest})"), ctx.theme.link_url, None));
            }
        }
        LinkMode::Reference => {
            if !is_autolink && !dest.is_empty() {
                let number = ctx.reference_links.len() + 1;
                ctx.reference_links.push((number, dest.to_string()));
                out.push(Token::word(format!("[{number}]"), ctx.theme.link_url, None));
            }
        }
    }
}

/// Applies a colour to spans that do not have one, leaving styled runs alone.
///
/// Used for quote bodies and completed tasks, where the whole block should read
/// as muted without flattening the colours of code spans or links inside it.
pub(super) fn tint(lines: &mut [Line], style: Style) {
    for line in lines {
        for span in &mut line.spans {
            if span.style.fg == Color::Default {
                span.style = span.style.merge(style);
            } else {
                // Keep the colour, adopt the attributes.
                let mut attrs = style;
                attrs.fg = Color::Default;
                attrs.bg = Color::Default;
                span.style = span.style.merge(attrs);
            }
        }
    }
}

fn indent(lines: &mut [Line], by: usize) {
    for line in lines {
        line.prefix(vec![Span::plain(" ".repeat(by))]);
    }
}

fn trim_blank_edges(lines: &mut Vec<Line>) {
    while lines
        .first()
        .is_some_and(|l| l.is_blank() && !l.image_filler)
    {
        lines.remove(0);
    }
    while lines
        .last()
        .is_some_and(|l| l.is_blank() && !l.image_filler)
    {
        lines.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::{ParseOptions, parse};
    use crate::render::{NoImages, RenderOptions};

    /// Renders to plain text at a fixed width, which is how most layout
    /// assertions in this crate are written.
    fn text_at(src: &str, width: usize) -> String {
        let doc = parse(src, ParseOptions::default());
        let opts = RenderOptions {
            width,
            margin: 0,
            images: false,
            glyphs: crate::render::glyphs::ASCII,
            ..Default::default()
        };
        let caps = Capabilities::default();
        let hl = Highlighter::plain();
        let screen = super::render_document(&doc, &opts, &Theme::mono(), &caps, &mut NoImages, &hl);
        screen.plain_text()
    }

    fn text(src: &str) -> String {
        text_at(src, 40)
    }

    #[test]
    fn renders_a_heading_with_marks_and_a_rule() {
        let out = text("# Title\n");
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines[0], "# Title");
        assert!(
            lines[1].starts_with('='),
            "expected a rule, got {:?}",
            lines[1]
        );
    }

    #[test]
    fn wraps_paragraphs_to_the_width() {
        let out = text_at("one two three four five six seven eight nine ten", 20);
        for line in out.lines() {
            assert!(display_width(line) <= 20, "{line:?} is too wide");
        }
        assert!(out.lines().count() >= 3);
    }

    #[test]
    fn nests_lists_with_increasing_indent() {
        let out = text("- one\n  - two\n    - three\n");
        let lines: Vec<&str> = out.lines().collect();
        let indent_of = |s: &str| s.len() - s.trim_start().len();
        assert!(indent_of(lines[1]) > indent_of(lines[0]));
        assert!(indent_of(lines[2]) > indent_of(lines[1]));
    }

    #[test]
    fn aligns_ordered_list_numbers() {
        let src: String = (1..=10).map(|i| format!("{i}. item\n")).collect();
        let out = text(&src);
        let lines: Vec<&str> = out.lines().collect();
        // "9. item" is padded so its text starts in the same column as "10. item".
        let col = |s: &str| s.find("item").unwrap();
        assert_eq!(col(lines[8]), col(lines[9]));
    }

    #[test]
    fn continuation_lines_align_under_item_text() {
        let out = text_at("- a very long list item that needs to wrap somewhere", 24);
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 2);
        assert!(
            lines[1].starts_with("  "),
            "continuation should be indented: {:?}",
            lines[1]
        );
    }

    #[test]
    fn renders_task_lists() {
        let out = text("- [x] done\n- [ ] todo\n");
        assert!(out.contains("[x] done"));
        assert!(out.contains("[ ] todo"));
    }

    #[test]
    fn prefixes_quotes_with_a_bar() {
        let out = text("> quoted text\n");
        assert!(out.starts_with("| quoted"), "got {out:?}");
    }

    #[test]
    fn labels_github_alerts() {
        let out = text("> [!WARNING]\n> Mind the gap.\n");
        assert!(out.contains("WARNING"));
        assert!(out.contains("Mind the gap"));
    }

    #[test]
    fn nested_quotes_stack_bars() {
        let out = text("> outer\n>\n> > inner\n");
        assert!(out.lines().any(|l| l.starts_with("| | ")), "got {out:?}");
    }

    #[test]
    fn shows_link_urls_when_hyperlinks_are_unavailable() {
        let out = text("[text](https://example.com)\n");
        assert!(out.contains("text"));
        assert!(out.contains("(https://example.com)"));
    }

    #[test]
    fn does_not_repeat_autolink_destinations() {
        let out = text("<https://example.com>\n");
        assert_eq!(out.matches("example.com").count(), 1, "got {out:?}");
    }

    #[test]
    fn numbers_footnotes_and_lists_them_at_the_end() {
        let out = text("Body[^a] text.\n\n[^a]: The note.\n");
        assert!(out.contains("Body[1] text."), "got {out:?}");
        assert!(out.contains("[1] The note."));
    }

    #[test]
    fn images_without_a_provider_become_placeholders() {
        let out = text("![A cat](cat.png)\n");
        assert!(out.contains("A cat"), "got {out:?}");
    }

    #[test]
    fn code_blocks_keep_their_indentation() {
        let out = text("```\n  indented\n```\n");
        assert!(out.lines().any(|l| l.contains("  indented")), "got {out:?}");
    }

    #[test]
    fn expands_tabs_in_code() {
        assert_eq!(expand_tabs("a\tb", 4), "a   b");
        assert_eq!(expand_tabs("\tx", 4), "    x");
        assert_eq!(expand_tabs("no tabs", 4), "no tabs");
    }

    #[test]
    fn horizontal_rules_span_the_width() {
        let out = text_at("---\n", 20);
        assert_eq!(display_width(out.trim_end()), 20);
    }

    #[test]
    fn headings_record_anchors() {
        let doc = parse("# One\n\ntext\n\n## Two\n", ParseOptions::default());
        let opts = RenderOptions {
            width: 40,
            margin: 0,
            images: false,
            glyphs: crate::render::glyphs::ASCII,
            ..Default::default()
        };
        let screen = super::render_document(
            &doc,
            &opts,
            &Theme::mono(),
            &Capabilities::default(),
            &mut NoImages,
            &Highlighter::plain(),
        );
        assert_eq!(screen.headings.len(), 2);
        assert_eq!(screen.anchor("one"), Some(0));
        let two = screen.anchor("two").expect("second anchor");
        assert!(two > 0);
        assert!(screen.lines[two].text().contains("Two"));
    }

    #[test]
    fn a_narrow_terminal_still_produces_output() {
        // Pathological width: everything must still lay out without panicking.
        for width in [1usize, 2, 3, 5, 10] {
            let out = text_at(
                "# Heading\n\n- list item\n\n> quote\n\n| a | b |\n|---|---|\n| 1 | 2 |\n",
                width,
            );
            assert!(!out.is_empty(), "width {width} produced nothing");
        }
    }

    #[test]
    fn empty_document_renders_nothing() {
        assert_eq!(text(""), "");
    }
}
