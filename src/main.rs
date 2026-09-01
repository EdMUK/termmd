//! `termmd`: a Markdown viewer for the terminal.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::{CommandFactory, Parser};

use termmd::cli::{Cli, Settings};
use termmd::config::Config;
use termmd::images::{RemotePolicy, Store};
use termmd::markdown::Document;
use termmd::render::{Highlighter, NoImages, Screen, WriteOptions};

fn main() {
    if let Err(error) = run() {
        // `{:#}` prints the whole context chain on one line, which is what a
        // command-line tool should say rather than a Rust backtrace.
        eprintln!("termmd: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();

    // Before anything reads the config or speaks to the terminal. Both of these
    // write something meant to be redirected into a file, and the capability
    // probe would put escape sequences in the middle of it.
    if let Some(shell) = cli.completions {
        return write_completions(shell);
    }
    if cli.man {
        return write_man_page();
    }

    let config = load_config(&cli)?;
    let settings = Settings::resolve(&cli, &config)?;

    if cli.caps {
        return print_capabilities(&settings);
    }
    let highlighter = Highlighter::new(&settings.syntax_theme);
    if cli.list_themes {
        return print_list(&highlighter.theme_names());
    }
    if cli.list_languages {
        return print_list(&highlighter.language_names());
    }

    let sources = termmd::source::read(&cli.files)?;

    if cli.toc {
        return print_toc(&termmd::source::build_document(&sources, settings.parse));
    }
    if cli.front_matter {
        return print_front_matter(&termmd::source::build_document(&sources, settings.parse));
    }

    let remote = if settings.remote_images {
        RemotePolicy::Allow
    } else {
        RemotePolicy::Deny
    };
    let base_dir = sources
        .first()
        .map(|s| s.base_dir())
        .unwrap_or_else(|| PathBuf::from("."));
    let mut store = Store::new(&settings.caps, base_dir, remote).with_protocol(settings.protocol);

    if settings.use_pager {
        return termmd::pager::run(
            &sources,
            &settings,
            &highlighter,
            &mut store,
            settings.watch,
        );
    }

    let document = termmd::source::build_document(&sources, settings.parse);
    let screen = render(&document, &settings, &highlighter, &mut store);
    print_screen(&screen, &settings, &mut store)?;

    if store.skipped_remote {
        eprintln!("termmd: some images were skipped; pass --remote-images to fetch them");
    }
    Ok(())
}

/// Renders a document with the resolved settings.
fn render(
    document: &Document,
    settings: &Settings,
    highlighter: &Highlighter,
    store: &mut Store,
) -> Screen {
    if settings.render.images {
        termmd::render::render(
            document,
            &settings.render,
            &settings.theme,
            &settings.caps,
            store,
            highlighter,
        )
    } else {
        termmd::render::render(
            document,
            &settings.render,
            &settings.theme,
            &settings.caps,
            &mut NoImages,
            highlighter,
        )
    }
}

fn print_screen(screen: &Screen, settings: &Settings, store: &mut Store) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    if !settings.styled {
        out.write_all(screen.plain_text().as_bytes())?;
        return Ok(out.flush()?);
    }
    let opts = WriteOptions {
        hyperlinks: settings.caps.hyperlinks
            || settings.render.links == termmd::render::LinkMode::Hyperlink,
        images: settings.render.images,
    };
    screen.write(&mut out, &settings.caps, &opts, store)?;
    Ok(())
}

/// Writes a shell's completion script to stdout.
///
/// Generated from the same `Command` clap parses with, rather than checked in,
/// so a flag added here cannot be missing there.
fn write_completions(shell: clap_complete::Shell) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    clap_complete::generate(shell, &mut Cli::command(), "termmd", &mut out);
    Ok(out.flush()?)
}

/// Writes the man page to stdout, as roff.
///
/// Assembled section by section rather than in one call, so that FILES lands
/// where a reader expects it rather than after the version.
fn write_man_page() -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    // A fuller description than `--help` carries: a man page is read cold, by
    // someone who has not necessarily run the program yet.
    let command = Cli::command().long_about(MAN_DESCRIPTION);
    let man = clap_mangen::Man::new(command).manual(String::from("termmd manual"));

    man.render_title(&mut out)?;
    man.render_name_section(&mut out)?;
    man.render_synopsis_section(&mut out)?;
    man.render_description_section(&mut out)?;
    man.render_options_section(&mut out)?;
    out.write_all(MAN_FILES.as_bytes())?;
    man.render_version_section(&mut out)?;
    Ok(out.flush()?)
}

const MAN_DESCRIPTION: &str = "\
termmd renders Markdown for terminals that can do more than plain text: images \
through the kitty, iTerm2 or sixel protocols, or Unicode half blocks where none \
of those are available; tables measured to fit the width; syntax highlighting; \
and an interactive pager with search, a table of contents and live reload.

With no FILE, or with -, it reads standard input. Several files are concatenated \
and labelled. Output through a pipe carries no escape sequences unless --color \
says otherwise, and the pager is used only when stdout is a terminal and the \
document does not fit on one screen.

What termmd draws depends on what it detects, which --caps reports.";

/// The one section clap cannot know about: where termmd looks for things.
///
/// Everything else a reader might want -- the pager's keys, the theme format --
/// is a moving target better read from `H` in the pager and the README than
/// from roff that would quietly fall out of date.
const MAN_FILES: &str = r#".SH FILES
.TP
.I ~/.config/termmd/config.toml
Configuration. Overridden by \fB--config\fR, or by \fITERMMD_CONFIG\fR in the
environment. \fB--config none\fR ignores it.
.TP
.I ~/.config/termmd/themes/
Theme files, selected by name with \fB--theme\fR.
.SH NOTES
Press \fBH\fR in the pager for its keys. Remote images are not fetched unless
\fB--remote-images\fR is given.
.SH SEE ALSO
Full documentation at
.UR https://github.com/EdMUK/termmd
.UE
"#;

fn load_config(cli: &Cli) -> Result<Config> {
    match cli.config.as_deref() {
        Some("none") => Ok(Config::default()),
        Some(path) => Config::from_file(Path::new(path)),
        None => Config::load(),
    }
}

fn print_toc(document: &Document) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for (level, text, id) in document.headings() {
        let indent = "  ".repeat(level.saturating_sub(1) as usize);
        writeln!(out, "{indent}{text}  #{id}")?;
    }
    Ok(out.flush()?)
}

/// Prints the front matter, which the renderer deliberately does not show.
///
/// It is metadata for whatever publishes the document, not part of it, so the
/// page has no place for it -- but a script that wants a document's title or
/// tags should not have to parse the file a second time to get them.
fn print_front_matter(document: &Document) -> Result<()> {
    let Some(text) = document.front_matter.as_deref() else {
        return Ok(());
    };
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    writeln!(out, "{}", text.trim_end())?;
    Ok(out.flush()?)
}

fn print_list(items: &[String]) -> Result<()> {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for item in items {
        writeln!(out, "{item}")?;
    }
    Ok(out.flush()?)
}

/// Reports what termmd detected, which is the first thing to check when images
/// or colours are not doing what someone expects.
fn print_capabilities(settings: &Settings) -> Result<()> {
    let caps = &settings.caps;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();

    writeln!(out, "terminal")?;
    writeln!(
        out,
        "  TERM              {}",
        std::env::var("TERM").unwrap_or_default()
    )?;
    writeln!(
        out,
        "  TERM_PROGRAM      {}",
        std::env::var("TERM_PROGRAM").unwrap_or_else(|_| "-".into())
    )?;
    writeln!(
        out,
        "  size              {} x {} cells",
        caps.cols, caps.rows
    )?;
    match caps.cell_px {
        Some((w, h)) => writeln!(out, "  cell size         {w} x {h} px")?,
        None => writeln!(out, "  cell size         unknown (assuming 8 x 16)")?,
    }
    writeln!(out, "  stdout is a tty   {}", yes_no(caps.is_tty))?;
    writeln!(out, "  multiplexer       {}", yes_no(caps.multiplexed))?;

    writeln!(out, "\ncapabilities")?;
    writeln!(out, "  colour            {:?}", caps.color)?;
    writeln!(out, "  images            {}", caps.graphics.name())?;
    writeln!(out, "  hyperlinks        {}", yes_no(caps.hyperlinks))?;
    writeln!(out, "  unicode           {:?}", caps.unicode)?;
    match caps.background {
        Some(bg) => writeln!(
            out,
            "  background        #{:02x}{:02x}{:02x} ({})",
            bg.0,
            bg.1,
            bg.2,
            if caps.prefers_dark() { "dark" } else { "light" }
        )?,
        None => writeln!(out, "  background        unknown (assuming dark)")?,
    }

    writeln!(out, "\nsettings")?;
    writeln!(out, "  theme             {}", settings.theme.name)?;
    writeln!(out, "  syntax theme      {}", settings.syntax_theme)?;
    writeln!(out, "  width             {}", settings.render.width)?;
    writeln!(out, "  pager             {}", yes_no(settings.use_pager))?;
    match Config::path() {
        Some(p) => writeln!(
            out,
            "  config            {} ({})",
            p.display(),
            if p.exists() { "loaded" } else { "not present" }
        )?,
        None => writeln!(out, "  config            -")?,
    }
    Ok(out.flush()?)
}

fn yes_no(v: bool) -> &'static str {
    if v { "yes" } else { "no" }
}
