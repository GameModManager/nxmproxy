// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Optional fast path: hand the link to an already running manager over a
//! named pipe, instead of paying for a cold start.
//!
//! Kept from upstream, with one fix. Upstream's implementation is
//! `File::create(r"\\.\pipe\{name}")`, which is correct on Windows and wrong
//! everywhere else: on POSIX that is not a special path, it is a **relative
//! file name**, so every forwarded link on Linux or macOS would drop a stray
//! file called `\\.\pipe\<name>` into whatever directory the desktop happened
//! to launch us from. So the real implementation is Windows-only and the
//! others return `false`, which sends the caller down the spawn path.

#[cfg(windows)]
fn write_pipe(pipe: &str, value: &str) -> std::io::Result<usize> {
    std::fs::File::create(format!(r"\\.\pipe\{pipe}"))?.write(value.as_bytes())
}

#[cfg(not(windows))]
fn write_pipe(_pipe: &str, _value: &str) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "named pipes are windows only",
    ))
}

/// Try to deliver the link over the named pipe. `false` means "not delivered",
/// which is not an error: the manager is most likely simply not running.
pub fn try_write_pipe(pipe: &str, value: &str) -> bool {
    write_pipe(pipe, value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn a_failed_pipe_never_creates_a_file() {
        let dir = std::env::temp_dir().join(format!("nxmproxy-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        assert!(!try_write_pipe("vortex_download", "nxm://g/mods/1"));
        std::env::set_current_dir(previous).unwrap();
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "a failed pipe write must not leave files behind"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn an_unknown_pipe_is_not_fatal() {
        assert!(!try_write_pipe(
            "nxmproxy-no-such-pipe-test",
            "nxm://g/mods/1"
        ));
    }
}
