//! End-to-end tests that run the built binary.
//!
//! These check the things unit tests cannot: argument handling, exit codes, what
//! reaches stdout versus stderr, and that piped output is clean.

use std::io::Write;
use std::process::{Command, Stdio};

/// Runs termmd with `args` and no stdin, returning (stdout, stderr, success).
fn run(args: &[&str]) -> (String, String, bool) {
    run_with_stdin(args, "")
}

fn run_with_stdin(args: &[&str], stdin: &str) -> (String, String, bool) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_termmd"))
        .args(args)
        // A fixed environment keeps results the same on a developer's machine
        // and in CI, where TERM and friends differ wildly.
        .env_remove("TERM_PROGRAM")
        .env_remove("COLORTERM")
        .env_remove("KITTY_WINDOW_ID")
        .env_remove("TMUX")
        .env("TERM", "xterm-256color")
        .env("TERMMD_CONFIG", "/nonexistent/termmd.toml")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to run termmd");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

fn fixture(name: &str) -> String {
    format!("{}/tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn renders_a_file_to_stdout() {
    let (out, err, ok) = run(&["--width", "60", &fixture("sample.md")]);
    assert!(ok, "termmd failed: {err}");
    assert!(out.contains("Sample Document"));
    assert!(err.is_empty(), "unexpected stderr: {err}");
}

#[test]
fn reads_markdown_from_stdin() {
    let (out, _, ok) = run_with_stdin(&["--width", "40"], "# From stdin\n\nBody text.\n");
    assert!(ok);
    assert!(out.contains("From stdin"));
    assert!(out.contains("Body text."));
}

#[test]
fn a_dash_also_means_stdin() {
    let (out, _, ok) = run_with_stdin(&["-", "--width", "40"], "# Dash\n");
    assert!(ok);
    assert!(out.contains("Dash"));
}

#[test]
fn piped_output_carries_no_escape_sequences() {
    // stdout is a pipe here, so colour must be off without being asked.
    let (out, _, ok) = run(&["--width", "60", &fixture("sample.md")]);
    assert!(ok);
    assert!(
        !out.contains('\x1b'),
        "piped output should be plain: {out:?}"
    );
}

#[test]
fn colour_can_be_forced_through_a_pipe() {
    let (out, _, ok) = run(&[
        "--color",
        "truecolor",
        "--width",
        "60",
        &fixture("sample.md"),
    ]);
    assert!(ok);
    assert!(out.contains("\x1b[38;2;"), "expected truecolor sequences");
}

#[test]
fn plain_strips_everything() {
    let (out, _, ok) = run(&["--plain", "--color", "always", &fixture("sample.md")]);
    assert!(ok);
    assert!(!out.contains('\x1b'), "--plain must win over --color");
}

#[test]
fn width_is_respected() {
    for width in [40usize, 72] {
        let (out, _, ok) = run(&["--width", &width.to_string(), &fixture("sample.md")]);
        assert!(ok);
        for line in out.lines() {
            let columns = unicode_width::UnicodeWidthStr::width(line);
            assert!(
                columns <= width,
                "line exceeds width {width}: {line:?} ({columns})"
            );
        }
    }
}

#[test]
fn prints_a_table_of_contents() {
    let (out, _, ok) = run(&["--toc", &fixture("sample.md")]);
    assert!(ok);
    assert!(out.contains("Sample Document"));
    assert!(
        out.contains("#sample-document"),
        "anchors should be shown: {out}"
    );
    // The TOC is not the document.
    assert!(
        !out.contains("paragraph"),
        "TOC should list headings only: {out}"
    );
}

#[test]
fn reports_capabilities() {
    let (out, _, ok) = run(&["--caps"]);
    assert!(ok);
    for field in ["colour", "images", "hyperlinks", "theme", "width"] {
        assert!(out.contains(field), "--caps should report {field}: {out}");
    }
}

#[test]
fn lists_themes_and_languages() {
    let (out, _, ok) = run(&["--list-themes"]);
    assert!(ok);
    assert!(out.lines().count() > 5, "expected several themes");

    let (out, _, ok) = run(&["--list-languages"]);
    assert!(ok);
    assert!(out.contains("Rust"), "expected Rust among the languages");
}

#[test]
fn missing_files_fail_with_a_useful_message() {
    let (out, err, ok) = run(&["/no/such/file.md"]);
    assert!(!ok, "should exit non-zero");
    assert!(out.is_empty());
    assert!(
        err.contains("no/such/file.md"),
        "error should name the file: {err}"
    );
}

#[test]
fn an_unknown_theme_is_rejected() {
    let (_, err, ok) = run(&["--theme", "no-such-theme", &fixture("sample.md")]);
    assert!(!ok);
    assert!(err.contains("no-such-theme"));
}

#[test]
fn several_files_are_concatenated_and_labelled() {
    // Run from the fixtures directory and name the files relatively. The label
    // echoes the path as typed, so an absolute path on a deep checkout wraps
    // mid-filename at this width -- which says nothing about the behaviour
    // under test, but does make the test fail on some machines and not others.
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let (out, err, ok) = run_in(&dir, &["--width", "60", "sample.md", "second.md"]);
    assert!(ok, "stderr: {err}");
    assert!(out.contains("Sample Document"));
    assert!(out.contains("Second Document"));
    assert!(
        out.contains("second.md"),
        "each file should be labelled: {out}"
    );
}

#[test]
fn tables_are_drawn_within_the_width() {
    let (out, _, ok) = run(&["--width", "50", &fixture("sample.md")]);
    assert!(ok);
    assert!(
        out.contains('│') || out.contains('|'),
        "expected a table: {out}"
    );
    let table_rows: Vec<&str> = out.lines().filter(|l| l.contains('│')).collect();
    assert!(table_rows.len() >= 3, "expected several table rows");
    let widths: Vec<usize> = table_rows
        .iter()
        .map(|l| unicode_width::UnicodeWidthStr::width(*l))
        .collect();
    assert!(
        widths.windows(2).all(|w| w[0] == w[1]),
        "table rows are ragged: {widths:?}"
    );
}

#[test]
fn images_become_placeholders_when_no_protocol_is_available() {
    let (out, _, ok) = run(&["--images", "none", "--width", "60", &fixture("sample.md")]);
    assert!(ok);
    assert!(
        out.contains("A picture"),
        "the alt text should survive: {out}"
    );
}

#[test]
fn remote_images_are_refused_by_default() {
    let markdown = "![remote](https://example.invalid/nope.png)\n";
    let (out, _, ok) = run_with_stdin(&["--images", "kitty", "--width", "40"], markdown);
    assert!(ok, "a remote image must not be a fatal error");
    assert!(out.contains("remote"), "should fall back to the alt text");
}

#[test]
fn the_pager_refuses_to_run_without_a_terminal() {
    // `--pager` is a request for the pager whatever the document's length, so
    // this must fail rather than quietly printing a short file. Windows
    // consoles report a size even with stdout redirected, which is how the
    // shortcut used to swallow this.
    let (_, err, ok) = run(&["--pager", &fixture("sample.md")]);
    assert!(!ok);
    assert!(err.contains("terminal"), "should explain why: {err}");
}

#[test]
fn empty_input_is_not_an_error() {
    let (out, err, ok) = run_with_stdin(&[], "");
    assert!(ok, "stderr: {err}");
    assert!(out.trim().is_empty());
}

#[test]
fn help_and_version_work() {
    let (out, _, ok) = run(&["--help"]);
    assert!(ok);
    assert!(out.contains("--width"));
    assert!(
        out.contains("--caps"),
        "the help should mention the diagnostic flag"
    );

    let (out, _, ok) = run(&["--version"]);
    assert!(ok);
    assert!(out.contains(env!("CARGO_PKG_VERSION")));
}

/// A 16x8 PNG, embedded so the tests need no binary fixtures on disk.
const TINY_PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x08, 0x08, 0x02, 0x00, 0x00, 0x00, 0x7f, 0x14, 0xe8,
    0xc0, 0x00, 0x00, 0x00, 0x17, 0x49, 0x44, 0x41, 0x54, 0x78, 0xda, 0x63, 0x3c, 0x11, 0xa0, 0xc1,
    0x40, 0x0a, 0x60, 0x62, 0x20, 0x11, 0x8c, 0x6a, 0x20, 0x06, 0x00, 0x00, 0xf5, 0xf7, 0x01, 0x50,
    0x0f, 0x2c, 0x93, 0x87, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

/// Runs termmd from a given working directory.
fn run_in(dir: &std::path::Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_termmd"))
        .args(args)
        .current_dir(dir)
        .env_remove("TERM_PROGRAM")
        .env_remove("COLORTERM")
        .env("TERM", "xterm-256color")
        .env("TERMMD_CONFIG", "/nonexistent/termmd.toml")
        .stdin(Stdio::null())
        .output()
        .expect("failed to run termmd");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// Builds `<tmp>/<name>/doc/page.md` next to `<tmp>/<name>/doc/images/pic.png`.
fn image_tree(name: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(name);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("doc/images")).unwrap();
    std::fs::write(root.join("doc/images/pic.png"), TINY_PNG).unwrap();
    std::fs::write(
        root.join("doc/page.md"),
        "# Page\n\n![The picture](images/pic.png)\n",
    )
    .unwrap();
    root
}

#[test]
fn a_relative_image_resolves_against_the_document_not_the_shell() {
    // Regression: the image URL is rebased against the document's directory at
    // parse time, and was then joined with a base directory a second time, so
    // `doc/images/pic.png` was looked for at `doc/doc/images/pic.png` and every
    // image silently became a placeholder. Only absolute paths worked.
    let root = image_tree("termmd-relative-image");
    let (out, err, ok) = run_in(&root, &["--images", "kitty", "-P", "doc/page.md"]);
    assert!(ok, "stderr: {err}");
    assert!(
        out.contains("\x1b_G"),
        "expected a kitty image sequence, got a placeholder instead:\n{out}"
    );
}

#[test]
fn images_resolve_the_same_way_from_any_directory() {
    let root = image_tree("termmd-image-cwd");
    let absolute = root.join("doc/page.md");

    // From the repository root, from inside the document's directory, and by
    // absolute path: all three must find the same picture.
    let cases: Vec<(std::path::PathBuf, String)> = vec![
        (root.clone(), "doc/page.md".to_string()),
        (root.join("doc"), "page.md".to_string()),
        (std::env::temp_dir(), absolute.display().to_string()),
    ];
    for (dir, arg) in cases {
        let (out, err, ok) = run_in(&dir, &["--images", "kitty", "-P", &arg]);
        assert!(ok, "stderr: {err}");
        assert!(
            out.contains("\x1b_G"),
            "no image when run from {} with {arg}:\n{out}",
            dir.display()
        );
    }
}

#[test]
fn a_missing_image_still_shows_its_alt_text() {
    let root = image_tree("termmd-missing-image");
    std::fs::remove_file(root.join("doc/images/pic.png")).unwrap();
    let (out, _, ok) = run_in(&root, &["--images", "kitty", "-P", "doc/page.md"]);
    assert!(ok);
    assert!(
        out.contains("The picture"),
        "alt text should survive: {out}"
    );
}

#[test]
fn local_document_links_are_emitted_as_file_uris() {
    let root = image_tree("termmd-file-uri");
    std::fs::write(root.join("doc/other.md"), "# Other\n").unwrap();
    std::fs::write(root.join("doc/page.md"), "[see other](other.md)\n").unwrap();
    let (out, _, ok) = run_in(
        &root,
        &[
            "--links",
            "hyperlink",
            "--color",
            "always",
            "-P",
            "doc/page.md",
        ],
    );
    assert!(ok);
    assert!(
        out.contains("\x1b]8;;file:///"),
        "expected a file:// hyperlink:\n{out}"
    );
}

#[test]
fn writes_completion_scripts() {
    for (shell, marker) in [
        ("bash", "_termmd()"),
        ("zsh", "#compdef termmd"),
        ("fish", "complete -c termmd"),
        ("elvish", "termmd"),
        ("powershell", "termmd"),
    ] {
        let (out, err, ok) = run(&["--completions", shell]);
        assert!(ok, "--completions {shell} failed: {err}");
        assert!(
            out.contains(marker),
            "the {shell} script should contain {marker:?}: {out}"
        );
        // A completion script is redirected into a file, so a stray escape
        // sequence from the capability probe would be written into it.
        assert!(
            !out.contains('\x1b'),
            "the {shell} script carries an escape sequence"
        );
        // Generated from the parser itself, so a flag added without touching
        // this test still turns up. Named without its dashes, because fish
        // spells a long option `-l remote-images`.
        assert!(
            out.contains("remote-images"),
            "the {shell} script is missing a flag termmd accepts"
        );
    }

    let (_, err, ok) = run(&["--completions", "tcsh"]);
    assert!(!ok, "an unsupported shell should be an error");
    assert!(
        err.contains("tcsh"),
        "the error should name the shell: {err}"
    );
}

#[test]
fn writes_a_man_page() {
    let (out, err, ok) = run(&["--man"]);
    assert!(ok, "--man failed: {err}");
    assert!(out.starts_with(".ie"), "roff starts with its own preamble");
    for section in [".TH termmd 1", ".SH NAME", ".SH SYNOPSIS", ".SH OPTIONS"] {
        assert!(
            out.contains(section),
            "the man page has no {section}: {out}"
        );
    }
    // The hand-written sections, which clap knows nothing about.
    assert!(out.contains(".SH FILES") && out.contains("config.toml"));
    assert!(
        !out.contains('\x1b'),
        "the man page carries an escape sequence"
    );
    assert!(
        out.contains("--remote-images"),
        "the man page is missing a flag termmd accepts"
    );
}
