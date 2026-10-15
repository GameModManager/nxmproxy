// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! A small file logger.
//!
//! Upstream's hand-rolled logger is kept, minus the `once_cell` dependency:
//! `std::sync::OnceLock` does the same job from the standard library.
//!
//! The log matters more than it looks. We are launched by the OS with no
//! terminal attached and we exit in milliseconds, so when a link does not
//! arrive the only place the reason can live is here.

use std::fs::OpenOptions;
use std::io::Write;
use std::sync::{Mutex, OnceLock};

static SINK: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

fn sink() -> &'static Mutex<Option<std::fs::File>> {
    SINK.get_or_init(|| Mutex::new(None))
}

/// Log to `path` from now on, creating it if needed. Failure is not an error:
/// we fall back to stderr rather than refusing to run.
pub fn set_file(path: &std::path::Path) {
    let file = OpenOptions::new().create(true).append(true).open(path).ok();
    *sink().lock().unwrap() = file;
}

fn log(level: &str, message: &str) {
    let mut guard = sink().lock().unwrap();
    match guard.as_mut() {
        Some(file) => {
            let _ = writeln!(file, "{level}: {message}");
            let _ = file.flush();
        }
        None => eprintln!("{level}: {message}"),
    }
}

pub fn info<S: AsRef<str>>(message: S) {
    log("info", message.as_ref());
}

pub fn warn<S: AsRef<str>>(message: S) {
    log("warn", message.as_ref());
}

pub fn error<S: AsRef<str>>(message: S) {
    log("error", message.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_never_panics_with_or_without_a_file() {
        set_file(std::path::Path::new(
            "/nonexistent-directory-for-tests/nxm.log",
        ));
        info("hello");
        warn("careful");
        error("bad");
    }
}
