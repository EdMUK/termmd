//! What tmux knows about the terminal it is drawing on.
//!
//! Inside a multiplexer we deliberately assume the least: passthrough of
//! graphics sequences is not something to rely on, so the fallback is half
//! blocks, which are only coloured text. But tmux from 3.4 does not need
//! passthrough for sixel -- it parses the image into the pane itself and
//! redraws it for whichever client is attached -- and it has forwarded OSC 8
//! hyperlinks since the same release.
//!
//! What it will actually deliver depends on the terminal on the *outside*, and
//! asking the terminal directly cannot tell us: tmux answers Primary Device
//! Attributes on its own behalf, advertising sixel whether or not the client
//! could draw it. It does, however, keep a list of what it decided the client
//! supports, and will tell us:
//!
//! ```text
//! $ tmux display-message -p '#{client_termfeatures}'
//! bpaste,ccolour,clipboard,hyperlinks,cstyle,focus,RGB,sixel,title
//! ```
//!
//! That list is the outer terminal's, so it is the one worth believing. An
//! older tmux, or one built without sixel, simply does not name the feature,
//! which leaves us on the conservative path we were on anyway.

use std::env;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long tmux gets to answer. A live server replies in a millisecond or
/// two; a wedged one would otherwise hold termmd at startup, when the whole
/// point of asking was to fall back gracefully.
const TIMEOUT: Duration = Duration::from_millis(500);

/// What tmux says its client can do.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Features {
    pub sixel: bool,
    pub hyperlinks: bool,
}

/// Reads the feature list for the pane we are running in.
///
/// Returns `None` when we are not in tmux, or when tmux cannot be asked -- an
/// old version without the format, a server that has gone away, or no `tmux`
/// on `PATH` because the session was inherited from somewhere else.
pub fn features() -> Option<Features> {
    let pane = env::var("TMUX_PANE").ok()?;
    env::var_os("TMUX")?;

    // Targeting our own pane matters when more than one client is attached:
    // the answer is per-client, and the one showing this pane is the one whose
    // capabilities we are about to rely on.
    let mut child = Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{client_termfeatures}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let status = wait_within(&mut child, TIMEOUT)?;
    if !status.success() {
        return None;
    }
    // Read after waiting is safe only because the answer is one short line,
    // far below what a pipe holds before its writer would block.
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(parse(&out))
}

/// Waits for `child` to exit, and kills it if `timeout` passes first.
///
/// `None` means it was killed: whatever it was going to say, it did not say
/// it in time, and the caller should carry on as if it had not been asked.
pub(crate) fn wait_within(child: &mut Child, timeout: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Some(status),
            Ok(None) if start.elapsed() < timeout => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Picks the two features we care about out of tmux's comma-separated list.
pub fn parse(list: &str) -> Features {
    let mut features = Features::default();
    for name in list.trim().split(',') {
        match name.trim() {
            "sixel" => features.sixel = true,
            "hyperlinks" => features.hyperlinks = true,
            _ => {}
        }
    }
    features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_features_we_care_about() {
        // A real reply, from tmux 3.6 attached to a terminal with both.
        let f = parse("bpaste,ccolour,clipboard,hyperlinks,cstyle,focus,RGB,sixel,title\n");
        assert_eq!(
            f,
            Features {
                sixel: true,
                hyperlinks: true
            }
        );
    }

    #[test]
    fn a_terminal_without_them_is_not_promoted() {
        // The same tmux, attached to a terminal that has neither.
        let f = parse("bpaste,ccolour,clipboard,cstyle,focus,RGB,title\n");
        assert_eq!(f, Features::default());
    }

    #[test]
    fn nothing_at_all_is_not_a_yes() {
        // An old tmux answers an unknown format with the empty string, and a
        // failed lookup gives us nothing to split.
        assert_eq!(parse(""), Features::default());
        assert_eq!(parse("\n"), Features::default());
    }

    #[test]
    #[cfg(unix)]
    fn a_process_that_does_not_finish_is_killed() {
        let mut slow = Command::new("sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let start = Instant::now();
        assert_eq!(wait_within(&mut slow, Duration::from_millis(50)), None);
        assert!(start.elapsed() < Duration::from_secs(5), "did not give up");
        // And it is gone, not left behind.
        assert!(matches!(slow.try_wait(), Ok(Some(_))));

        let mut quick = Command::new("true").spawn().unwrap();
        assert!(wait_within(&mut quick, Duration::from_secs(5)).is_some_and(|s| s.success()));
    }

    #[test]
    fn a_feature_is_matched_whole() {
        // Not "sixel" -- a substring match would take this for one.
        assert_eq!(parse("nosixel,hyperlinksextra"), Features::default());
    }
}
