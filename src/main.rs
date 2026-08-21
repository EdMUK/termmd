//! `termmd`: a Markdown viewer for the terminal.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;
use clap::Parser;

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
