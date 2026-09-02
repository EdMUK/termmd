//! Tests that need a terminal.
//!
//! CONTRIBUTING says tests do not need one, and for layout that rule holds:
//! a document rendered at a fixed width is plain text, and a capability parser
//! can be fed recorded bytes. Two features are different. Copying to the
//! clipboard and drawing pictures inside tmux exist only for what reaches the
//! terminal, so the only test that means anything is one that watches the
//! terminal. These run termmd on a pty, or in a real tmux pane, and look at
//! what comes out.
//!
//! Everything here skips rather than fails when the machine cannot run it:
//! `tmux` missing, or too old to say what its client can draw. A skip is
//! printed, so it is visible in `--nocapture` output and in a CI log.

#![cfg(unix)]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

const TERMMD: &str = env!("CARGO_BIN_EXE_termmd");
const FIXTURE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/clipboard.md");

/// The one code block in the fixture, as `y` should copy it: the source as
/// written, without the trailing newline that would run it at a prompt.
const CODE: &str = "cargo install termmd\ntermmd README.md";

/// How long a step may take before the test gives up on it. Generous, because
/// CI machines are slow and a pager that takes two seconds to start is not a
/// bug this file is looking for.
const DEADLINE: Duration = Duration::from_secs(10);

// --- A pty ---------------------------------------------------------------

/// A pseudo-terminal: `master` is our end, `slave` is the child's.
struct Pty {
    master: OwnedFd,
    slave: OwnedFd,
}

impl Pty {
    fn open(cols: u16, rows: u16) -> Pty {
        let mut master = 0;
        let mut slave = 0;
        let mut size = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // Linux declares the size a const pointer and macOS a mutable one;
        // going through a raw pointer satisfies both.
        let size_ptr: *mut libc::winsize = &mut size;
        // SAFETY: openpty writes two descriptors into the integers we pass
        // and reads the winsize; nothing else is touched.
        let rc = unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                size_ptr as _,
            )
        };
        assert_eq!(rc, 0, "openpty failed");
        // SAFETY: both descriptors were just created for us and are owned by
        // nobody else.
        unsafe {
            Pty {
                master: OwnedFd::from_raw_fd(master),
                slave: OwnedFd::from_raw_fd(slave),
            }
        }
    }

    /// Spawns `cmd` with the slave as its controlling terminal.
    fn spawn(&self, cmd: &mut Command) -> Child {
        let slave = self.slave.as_raw_fd();
        // SAFETY: login_tty is the textbook sequence -- setsid, make the tty
        // controlling, dup it onto stdin, stdout and stderr -- and touches
        // nothing but that descriptor.
        unsafe {
            cmd.pre_exec(move || {
                if libc::login_tty(slave) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        cmd.spawn().expect("spawn on pty")
    }

    /// Starts draining the master into a shared buffer. Without a reader the
    /// child blocks once the pty's buffer fills, and a pager fills it fast.
    fn drain(&self) -> Arc<Mutex<Vec<u8>>> {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&buffer);
        // SAFETY: a duplicate of the master, owned by the thread.
        let mut reader = unsafe { File::from_raw_fd(libc::dup(self.master.as_raw_fd())) };
        thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            // EIO is how a pty reports that the other side hung up.
            while let Ok(n) = reader.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        buffer
    }

    fn write(&self, bytes: &[u8]) {
        // SAFETY: a duplicate of the master, closed when `file` drops.
        let mut file = unsafe { File::from_raw_fd(libc::dup(self.master.as_raw_fd())) };
        file.write_all(bytes).unwrap();
        file.flush().unwrap();
    }
}

/// Waits until `pred` is true of the bytes drained so far, or the deadline
/// passes. Returns a snapshot of the output either way, so a failure message
/// can show what the terminal actually received.
fn wait_for(buffer: &Mutex<Vec<u8>>, pred: impl Fn(&[u8]) -> bool) -> Vec<u8> {
    let start = Instant::now();
    loop {
        let snapshot = buffer.lock().unwrap().clone();
        if pred(&snapshot) || start.elapsed() > DEADLINE {
            return snapshot;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Waits for a child to exit, killing it if it will not.
fn reap(mut child: Child) -> bool {
    let start = Instant::now();
    while start.elapsed() < DEADLINE {
        if let Ok(Some(status)) = child.try_wait() {
            return status.success();
        }
        thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

/// termmd with the environment a test can reason about: no config file, no
/// terminal markers inherited from whoever ran cargo, no tmux unless a test
/// puts it there.
fn termmd() -> Command {
    let mut cmd = Command::new(TERMMD);
    cmd.env_remove("TERM_PROGRAM")
        .env_remove("COLORTERM")
        .env_remove("KITTY_WINDOW_ID")
        .env_remove("GHOSTTY_RESOURCES_DIR")
        .env_remove("TMUX")
        .env_remove("TMUX_PANE")
        .env("TERM", "xterm-256color")
        .env("TERMMD_CONFIG", "/nonexistent/termmd.toml");
    cmd
}

// --- OSC 52, outside tmux --------------------------------------------------

#[test]
fn y_writes_the_code_block_to_the_terminal_as_osc_52() {
    let pty = Pty::open(80, 24);
    let output = pty.drain();
    // `--pager` because the fixture fits on one screen, and a document that
    // fits is printed rather than paged.
    let child = pty.spawn(termmd().args(["--pager", "--no-probe", "--images=none", FIXTURE]));

    // The pager is up once it has switched to the alternate screen. Keys sent
    // before that are queued by the pty, but waiting keeps the failure mode
    // honest: a pager that never starts should fail here, not on the copy.
    let seen = wait_for(&output, |out| find(out, b"\x1b[?1049h").is_some());
    assert!(
        find(&seen, b"\x1b[?1049h").is_some(),
        "pager never started: {}",
        String::from_utf8_lossy(&seen)
    );

    pty.write(b"y");
    let seen = wait_for(&output, |out| {
        find(out, b"\x1b]52;c;").is_some_and(|at| find(&out[at..], b"\x07").is_some())
    });
    let start = find(&seen, b"\x1b]52;c;").unwrap_or_else(|| {
        panic!(
            "no OSC 52 reached the terminal: {}",
            String::from_utf8_lossy(&seen)
        )
    }) + "\x1b]52;c;".len();
    let end = start + find(&seen[start..], b"\x07").expect("sequence terminated");
    let copied = BASE64.decode(&seen[start..end]).expect("payload is base64");
    assert_eq!(String::from_utf8(copied).unwrap(), CODE);

    // The pager says what it did, in the status line rather than the document.
    let seen = wait_for(&output, |out| find(out, b"copied 2 lines").is_some());
    assert!(
        find(&seen, b"copied 2 lines").is_some(),
        "no confirmation drawn: {}",
        String::from_utf8_lossy(&seen)
    );

    pty.write(b"q");
    assert!(reap(child), "termmd did not exit cleanly after q");
}

// --- tmux ----------------------------------------------------------------

/// A private tmux server, on its own socket, that dies with the test.
struct Tmux {
    socket: String,
}

impl Tmux {
    /// Starts a detached server, or explains why the test is skipping.
    fn start() -> Option<Tmux> {
        let version = match Command::new("tmux").arg("-V").output() {
            Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).into_owned(),
            _ => {
                eprintln!("skipped: no tmux on PATH");
                return None;
            }
        };
        // One socket per test, so tests in the same process cannot share a
        // server or a paste buffer.
        let socket = format!(
            "termmd-test-{}-{:?}",
            std::process::id(),
            thread::current().id()
        )
        .replace(|c: char| !c.is_ascii_alphanumeric(), "-");
        let tmux = Tmux { socket };
        // `-f /dev/null` keeps the developer's own configuration out of it.
        let ok = tmux
            .cmd(&[
                "-f",
                "/dev/null",
                "new-session",
                "-d",
                "-x",
                "80",
                "-y",
                "24",
            ])
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!(
                "skipped: could not start a tmux server ({})",
                version.trim()
            );
            return None;
        }
        Some(tmux)
    }

    fn cmd(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new("tmux");
        cmd.arg("-L").arg(&self.socket).args(args);
        cmd.stdin(Stdio::null());
        cmd
    }

    fn output(&self, args: &[&str]) -> String {
        let out = self.cmd(args).output().expect("run tmux");
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// Runs a command in a new window and returns the pane id.
    fn window(&self, command: &str) -> String {
        self.output(&["new-window", "-d", "-P", "-F", "#{pane_id}", command])
            .trim()
            .to_string()
    }

    /// Polls the pane's contents until `pred` accepts them.
    fn wait_pane(&self, pane: &str, pred: impl Fn(&str) -> bool) -> String {
        let start = Instant::now();
        loop {
            let text = self.output(&["capture-pane", "-p", "-t", pane]);
            if pred(&text) || start.elapsed() > DEADLINE {
                return text;
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Tmux {
    fn drop(&mut self) {
        let _ = self.cmd(&["kill-server"]).status();
    }
}

/// The termmd invocation for a tmux pane, quoted for the shell tmux runs it
/// with. tmux sets `TMUX` and `TMUX_PANE` in the pane itself, which is what
/// termmd looks at, so no environment needs arranging here.
fn termmd_in_pane(args: &str) -> String {
    format!(
        "env TERM=xterm-256color TERMMD_CONFIG=/nonexistent/termmd.toml {} {}",
        shell_quote(TERMMD),
        args
    )
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[test]
fn inside_tmux_y_hands_the_code_block_to_tmux() {
    let Some(tmux) = Tmux::start() else { return };
    let pane = tmux.window(&termmd_in_pane(&format!(
        "--pager --no-probe --images=none {}",
        shell_quote(FIXTURE)
    )));
    // The fixture's heading, once the pager has drawn the document.
    let drawn = tmux.wait_pane(&pane, |text| text.contains("Clipboard"));
    assert!(drawn.contains("Clipboard"), "pager never drew: {drawn}");

    tmux.cmd(&["send-keys", "-t", &pane, "y"]).status().unwrap();

    // The copy goes to tmux's own paste buffer, not down the pane: that is the
    // whole point, since tmux would otherwise swallow the OSC 52.
    let start = Instant::now();
    let mut buffer = String::new();
    while start.elapsed() < DEADLINE {
        buffer = tmux.output(&["show-buffer"]);
        if buffer == CODE {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(buffer, CODE, "tmux paste buffer");

    let told = tmux.wait_pane(&pane, |text| text.contains("copied 2 lines"));
    assert!(
        told.contains("copied 2 lines"),
        "no confirmation drawn: {told}"
    );
}

/// What `--caps` reported in a pane, as (images, hyperlinks).
fn caps_in_pane(tmux: &Tmux) -> (String, String) {
    // `--caps` has to write to the pane, not a file: with stdout redirected
    // termmd rightly reports no images at all. `sleep` keeps the pane open
    // long enough to read it.
    let pane = tmux.window(&format!("{}; sleep 30", termmd_in_pane("--caps")));
    let text = tmux.wait_pane(&pane, |text| text.contains("pager"));
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.trim_start().strip_prefix(name))
            .map(|rest| rest.trim().to_string())
            .unwrap_or_else(|| panic!("no `{name}` line in --caps output:\n{text}"))
    };
    let caps = (field("images"), field("hyperlinks"));
    tmux.cmd(&["kill-pane", "-t", &pane]).status().unwrap();
    caps
}

#[test]
fn inside_tmux_images_follow_what_tmux_says_its_client_can_draw() {
    let Some(tmux) = Tmux::start() else { return };

    // A client has to be attached for tmux to have an opinion about one, and
    // attaching needs a terminal. This is the one place a pty is needed for
    // tmux: the pty is the outer terminal, and tmux decides what it can do
    // from TERM plus the `terminal-features` option, exactly as it would for
    // a person's terminal.
    let pty = Pty::open(80, 24);
    let _output = pty.drain();
    let mut client = pty.spawn(
        Command::new("tmux")
            .args(["-L", &tmux.socket, "attach"])
            .env("TERM", "xterm-256color"),
    );
    let start = Instant::now();
    while tmux
        .output(&["display-message", "-p", "#{client_termfeatures}"])
        .trim()
        .is_empty()
        && start.elapsed() < DEADLINE
    {
        thread::sleep(Duration::from_millis(50));
    }
    let plain = tmux.output(&["display-message", "-p", "#{client_termfeatures}"]);
    assert!(!plain.trim().is_empty(), "client never attached");

    // Plain xterm: tmux does not credit it with sixel or hyperlinks, so termmd
    // must not either. Half blocks are what it had before tmux was asked.
    let (images, hyperlinks) = caps_in_pane(&tmux);
    assert_eq!((images.as_str(), hyperlinks.as_str()), ("blocks", "no"));

    // Now tell tmux the outer terminal can draw both, the way a person with a
    // capable terminal that tmux does not recognise would, and attach again
    // so the new client is judged by it.
    let _ = client.kill();
    let _ = client.wait();
    // A fresh pty, not the old one: once its session leader has gone, macOS
    // revokes the slave, and every descriptor to it answers EBADF from then on.
    let pty = Pty::open(80, 24);
    let _output = pty.drain();
    tmux.cmd(&[
        "set",
        "-as",
        "terminal-features",
        "xterm-256color:sixel:hyperlinks",
    ])
    .status()
    .unwrap();
    let mut client = pty.spawn(
        Command::new("tmux")
            .args(["-L", &tmux.socket, "attach"])
            .env("TERM", "xterm-256color"),
    );
    let start = Instant::now();
    let mut features = String::new();
    while start.elapsed() < DEADLINE {
        features = tmux.output(&["display-message", "-p", "#{client_termfeatures}"]);
        if features.contains("sixel") {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !features.contains("sixel") || !features.contains("hyperlinks") {
        // A tmux before 3.4, or built without sixel, has no such features to
        // grant. Nothing to promote and nothing to check.
        eprintln!(
            "skipped: this tmux cannot grant sixel and hyperlinks (client_termfeatures: {})",
            features.trim()
        );
        let _ = client.kill();
        let _ = client.wait();
        return;
    }

    let (images, hyperlinks) = caps_in_pane(&tmux);
    let _ = client.kill();
    let _ = client.wait();
    assert_eq!((images.as_str(), hyperlinks.as_str()), ("sixel", "yes"));
}
