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
//! ## The rule all three obey: ask, then say whether it worked
//!
//! An earlier version of this fork obeyed the opposite rule, "register, never
//! seize", on the grounds that seizing is the behaviour this tool exists to fix.
//! That was right about the *method* and wrong about the *outcome*: nxmproxy
//! takes `nxm://` and forwards it to whichever manager owns that game, so a
//! user who installed it and got a chooser instead of a working link had a
//! broken install.
//!
//! So `install` now claims, and each platform claims only as hard as the OS
//! allows:
//!
//! * **Linux** - one `[Default Applications]` line, written with the same
//!   `xdg-mime` every other well-behaved tool uses, and removable afterwards.
//! * **macOS** - `LSSetDefaultHandlerForURLScheme`, which modern macOS turns
//!   into a user prompt. The OS asking is the consent step.
//! * **Windows** - it cannot be done at all, and says so.
//!
//! What is still forbidden everywhere: touching **another mod manager's**
//! entry. Claiming is for *us*. `imp::claim` returns a [`Claim`] so the caller
//! can tell a script "done" from "one click left", and can never report a win
//! it did not verify.

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

/// The command succeeded and the result was verified.
pub const EXIT_OK: i32 = 0;

/// The command failed. A link was not delivered, or a write did not happen.
pub const EXIT_FAILED: i32 = 1;

/// The OS forbade or gated the thing, and one step is left for the user.
///
/// This is the code that makes `install` scriptable. It is deliberately **not**
/// [`EXIT_FAILED`]: nothing went wrong, nxmproxy did everything the platform
/// allows, and the remaining step is a click in system settings that no program
/// is permitted to perform. A wrapper can branch on it - "done" versus "tell
/// the user to click" - instead of parsing the message.
///
/// It is also deliberately **not** 0. Reporting success over an install that
/// routes nothing is how a broken install gets automated.
pub const EXIT_NEEDS_USER: i32 = 3;

/// The answer to "did the OS give nxmproxy the scheme?", as something a script
/// can act on.
///
/// `claimed` is true **only** when this program can point at evidence that
/// nxmproxy really is the handler. A platform that refuses, or that asks the
/// user and cannot be read back, returns false plus the instruction to print.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claim {
    /// True when nxmproxy is the handler and that was verified here.
    pub claimed: bool,
    /// What the user must do by hand. Empty when `claimed`.
    pub instruction: String,
}

impl Claim {
    /// Verified, nothing left to do.
    pub fn verified() -> Self {
        Claim {
            claimed: true,
            instruction: String::new(),
        }
    }

    /// Not done from here. `instruction` says exactly what to do instead, and
    /// is never empty, so a caller cannot print success by accident.
    pub fn needs_user(instruction: impl Into<String>) -> Self {
        Claim {
            claimed: false,
            instruction: instruction.into(),
        }
    }

    /// The exit code this claim implies.
    ///
    /// This is the whole point of the type. `install` on macOS and on Windows
    /// cannot finish the job, and a script driving it has to be able to tell
    /// "done, you may proceed" from "registered, but one click is left". Both
    /// used to be 0, which is how a broken install gets automated.
    pub fn exit_code(&self) -> i32 {
        if self.claimed {
            EXIT_OK
        } else {
            EXIT_NEEDS_USER
        }
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_verified_claim_exits_zero() {
        // The scriptable contract. If `needs_user` ever reports 0 again, a
        // wrapper automates an install that routes nothing - which is the exact
        // failure the distinct code exists to prevent.
        assert_eq!(Claim::verified().exit_code(), EXIT_OK);
        assert_eq!(Claim::needs_user("click here").exit_code(), EXIT_NEEDS_USER);
    }

    #[test]
    fn the_needs_user_code_is_distinct_from_every_failure_code() {
        // If it collided with 1 a script could not tell "it failed" from "it
        // worked, go click this", which is the only reason it exists.
        assert_ne!(EXIT_NEEDS_USER, EXIT_FAILED);
        assert_ne!(EXIT_NEEDS_USER, EXIT_OK);
        assert_ne!(EXIT_NEEDS_USER, 2, "2 is clap's usage-error code");
    }
}
