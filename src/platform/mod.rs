// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Per-platform protocol-handler registration.
//!
//! Every platform module here compiles on every platform: none of them contains
//! FFI or OS headers. Linux publishes an XDG desktop entry, macOS publishes an
//! app bundle, Windows drives `reg.exe`. That is a deliberate trade - a little
//! shelling out instead of a bindings crate - and it buys one real thing: the
//! argument construction, the plist text and the registry key names are all
//! compiled and unit tested on whatever machine the tests happen to run on.
//!
//! The rule all three obey: **register, do not seize.** Nothing here writes a
//! default association, calls `xdg-mime default`, or calls
//! `LSSetDefaultHandlerForURLScheme`. Taking the scheme by force is the
//! behaviour this tool exists to stop, and a tool that fixes it while doing the
//! same thing on the way in is worse than the thing it replaces.

#[allow(dead_code)]
pub mod linux;
#[allow(dead_code)]
pub mod macos;
#[allow(dead_code)]
pub mod windows;

#[cfg(target_os = "linux")]
pub use linux as imp;
#[cfg(target_os = "macos")]
pub use macos as imp;
#[cfg(target_os = "windows")]
pub use windows as imp;

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
compile_error!("nxmproxy supports Linux, macOS and Windows");

/// What `test` could determine about the current handler.
pub struct Status {
    /// True when nxmproxy is registered as a possible handler.
    pub registered: bool,
    /// What the OS reports as the current handler, or `None` when nothing
    /// claims the scheme and the OS will ask the user.
    pub handler: Option<String>,
    /// A note about what could not be determined here, if anything.
    pub caveat: Option<String>,
}

#[cfg(test)]
/// The code that actually ships, as text.
///
/// Two things are dropped: everything from the `#[cfg(test)]` module onwards,
/// and comment lines. Both are needed, and for the same reason - the tests
/// that use this helper contain the very strings they are looking for, and the
/// module docs *describe* the forbidden APIs in order to explain why they are
/// not called. Scanning the test binary itself is worse than useless: those
/// literals are compiled into it, so the check could never pass.
///
/// What is left is real production code, which is what the check is about.
pub fn production_code(source: &str) -> String {
    source
        .split("#[cfg(test)]")
        .next()
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}
