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
    /// A note about what could not be determined here, if anything.
    pub caveat: Option<String>,
}

/// One place the OS was told `nxm://` is handled, and by whom.
///
/// The file and the group name are the whole point of this type. `nxmproxy test`
/// used to print a bare application name, and on the machine that prompted all
/// of this the scheme was declared **twice by two different applications**, so
/// a name with nothing behind it could not be checked by hand and was not
/// enough to act on. "gamemodmanager-nxm.desktop" does not say whether it came
/// from `[Added Associations]` (this app says it handles nxm) or from
/// `[Default Applications]` (this app is what the desktop opens), and those two
/// have opposite precedence.
///
/// Serialised straight into `list --json`, so the contract a GUI reads is this
/// structure and not a hand-written summary of it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Declaration {
    /// The file it came from: a path on unix, a registry key on Windows. Never
    /// invented - a declaration we cannot point at is not one we can explain.
    pub source: String,
    /// The group it came from, spelled as the specification spells it.
    pub section: String,
    /// The desktop file ids listed there, in the order the file lists them.
    /// The **first** one is the handler; the rest are fallbacks the desktop may
    /// offer.
    pub ids: Vec<String>,
}

/// Every declaration the OS could have been given, in precedence order.
///
/// `declarations[0]` is the winner when the list is not empty; that is the
/// ordering rule of `mimeapps.list` (first file that names the scheme decides,
/// and within a file `[Default Applications]` before `[Added Associations]`).
/// It is a property of the *order*, so it is stored once rather than as a
/// separate field that could disagree with it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Resolution {
    /// Where the OS was told, highest precedence first. Empty when nothing on
    /// this machine declares the scheme.
    pub declarations: Vec<Declaration>,
    /// Why the winner won, in the specification's own terms.
    pub rule: String,
    /// False when the platform **cannot answer**, as opposed to answering "no".
    ///
    /// The two are not the same and reporting one as the other is a lie in the
    /// direction that reads best: on macOS nothing here can read LaunchServices'
    /// answer without a GUI session, so an empty [`Self::declarations`] means
    /// "I do not know", and printing "nothing claims nxm://" there would be a
    /// confident wrong sentence about a machine that has very probably got an
    /// owner. Linux and Windows answer, so theirs is always true and an empty
    /// list really does mean nothing claims it.
    pub readable: bool,
}

impl Resolution {
    /// The declaration the OS would use, if any.
    pub fn winner(&self) -> Option<&Declaration> {
        self.declarations.first()
    }

    /// The ranked ids of the winner: `foo.desktop;bar.desktop` and friends.
    ///
    /// Blank entries are dropped, because a `key=;` value names no application
    /// and must not become a handler of `";"`. The raw ids are still in
    /// [`Resolution::declarations`]; this is the list the desktop acts on.
    pub fn handler(&self) -> Option<String> {
        let ids: Vec<&str> = self
            .winner()?
            .ids
            .iter()
            .map(|id| id.trim())
            .filter(|id| !id.is_empty())
            .collect();
        (!ids.is_empty()).then(|| ids.join(";"))
    }

    /// The one application that actually gets the link.
    ///
    /// **The first id, not the whole list.** A `[Default Applications]` value is
    /// a ranked list of applications the desktop may offer, and the first entry
    /// is the one it opens - so `vortex.desktop;nxmproxy.desktop` means Vortex
    /// handles it and nxmproxy is merely a fallback. Comparing the joined
    /// string against `OWN_ID` would call that a win, which is the same false
    /// green in a new place.
    ///
    /// Empty list elements are skipped, because the trailing `;` the
    /// specification writes produces one and `xdg-mime` omits it.
    pub fn first_id(&self) -> Option<&str> {
        self.winner()?
            .ids
            .iter()
            .map(|id| id.trim())
            .find(|id| !id.is_empty())
    }

    /// Every application that claims the scheme, deduplicated, in precedence
    /// order.
    ///
    /// More than one is a genuine contest - two different programs each told the
    /// desktop they handle `nxm://` - and it is the state a user cannot
    /// interpret from an application name alone.
    pub fn claimants(&self) -> Vec<&str> {
        let mut seen: Vec<&str> = Vec::new();
        for declaration in &self.declarations {
            if let Some(id) = declaration
                .ids
                .iter()
                .map(|id| id.trim())
                .find(|id| !id.is_empty())
            {
                if !seen.contains(&id) {
                    seen.push(id);
                }
            }
        }
        seen
    }

    /// Does the OS's own answer name **nxmproxy**?
    ///
    /// `registered` and "is the handler" are different facts, and this fork used
    /// to let the first stand in for the second: `test` printed
    /// `registered: true` next to `nxm:// handler: gamemodmanager-nxm.desktop`,
    /// which reads as "you are fine" while nxmproxy is bypassed entirely, and
    /// it exited 0. `install`, `test` and `list --json` now all read this one.
    ///
    /// Per platform because the answer is: Linux and macOS compare an
    /// application id, Windows stores a whole command line under the bare scheme
    /// key and cannot be reduced to one, and macOS cannot read the answer back
    /// at all. Those are different facts, not one rule written three times.
    pub fn owns(&self) -> bool {
        imp::owns(self)
    }
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

    /// The false green this module exists to prevent, asserted in every shape
    /// the OS can hand back.
    ///
    /// Pure, and built by hand rather than read from a file: the rule is about
    /// *which declaration wins* and *which id in it counts*, and those two are
    /// separately falsifiable. A test that only ever ran against the real
    /// `mimeapps.list` chain could not tell a broken [`Resolution::first_id`]
    /// from a broken ordering.
    fn resolution_of(declarations: Vec<(&str, &str, &str)>) -> Resolution {
        Resolution {
            declarations: declarations
                .into_iter()
                .map(|(source, section, ids)| Declaration {
                    source: source.to_string(),
                    section: section.to_string(),
                    ids: ids.split(';').map(str::to_string).collect(),
                })
                .collect(),
            rule: String::new(),
            readable: true,
        }
    }

    #[test]
    fn the_winner_is_the_first_declaration_and_not_a_search_for_our_own_name() {
        // The failure the user reported, exactly: nxmproxy is named, and it is
        // named **second** in the value the desktop opens, so it is bypassed. A
        // reader that looks for its own id anywhere in the file calls this a
        // success. The declarations are listed in the order the OS considered
        // them, which is what a `mimeapps.list` is parsed into.
        let resolution = resolution_of(vec![
            (
                "/etc/mimeapps.list",
                "Default Applications",
                "gamemodmanager-nxm.desktop;nxmproxy.desktop",
            ),
            ("/etc/mimeapps.list", "Added Associations", "vortex.desktop"),
        ]);
        assert_eq!(resolution.first_id(), Some("gamemodmanager-nxm.desktop"));
        assert_eq!(
            resolution.handler().as_deref(),
            Some("gamemodmanager-nxm.desktop;nxmproxy.desktop"),
            "the whole ranked list is still what the desktop was told, nxmproxy \
             second in it or not"
        );
        assert_eq!(
            resolution.claimants(),
            vec!["gamemodmanager-nxm.desktop", "vortex.desktop"],
            "both applications that could get the link must be named, and a \
             fallback in second place is not one of them"
        );
    }

    #[test]
    fn our_id_first_is_a_win_and_our_id_alone_is_too() {
        let first = resolution_of(vec![(
            "/home/u/.config/mimeapps.list",
            "Default Applications",
            "nxmproxy.desktop;gamemodmanager-nxm.desktop",
        )]);
        assert_eq!(first.first_id(), Some("nxmproxy.desktop"));

        let alone = resolution_of(vec![(
            "/home/u/.config/mimeapps.list",
            "Default Applications",
            "nxmproxy.desktop",
        )]);
        assert_eq!(alone.first_id(), Some("nxmproxy.desktop"));
    }

    #[test]
    fn whitespace_and_empty_entries_do_not_change_which_id_wins() {
        // The specification's trailing `;` produces an empty element, and
        // `xdg-mime` omits the `;` entirely; a hand-edited file has spaces.
        let resolution = resolution_of(vec![(
            "/home/u/.config/mimeapps.list",
            "Default Applications",
            " nxmproxy.desktop ;other.desktop",
        )]);
        assert_eq!(resolution.first_id(), Some("nxmproxy.desktop"));

        // A value that is nothing but separators names no application at all.
        for empty in ["", "   ", ";"] {
            let blank = resolution_of(vec![(
                "/home/u/.config/mimeapps.list",
                "Default Applications",
                empty,
            )]);
            assert_eq!(blank.first_id(), None, "{empty:?} named nobody");
            assert_eq!(blank.handler(), None, "{empty:?} produced a handler");
            assert!(blank.claimants().is_empty(), "{empty:?} claimed the scheme");
        }
    }

    #[test]
    fn no_declaration_means_no_handler_rather_than_ours_by_default() {
        // Nothing claims it: the desktop will ask, and the answer is not ours.
        let resolution = Resolution {
            declarations: Vec::new(),
            rule: String::new(),
            readable: true,
        };
        assert_eq!(resolution.first_id(), None);
        assert_eq!(resolution.handler(), None);
        assert_eq!(resolution.winner(), None);
        assert!(resolution.claimants().is_empty());
    }

    #[test]
    fn one_application_listed_twice_is_not_a_contest() {
        // The same id in two sections is one application asserting itself twice,
        // which is what `xdg-mime default` and a desktop entry both produce. It
        // is not "two applications declare nxm://".
        let resolution = resolution_of(vec![
            (
                "/a",
                "Default Applications",
                "nxmproxy.desktop;other.desktop",
            ),
            ("/a", "Added Associations", "nxmproxy.desktop"),
            ("/b", "Default Applications", "nxmproxy.desktop"),
        ]);
        assert_eq!(resolution.claimants(), vec!["nxmproxy.desktop"]);
    }
}
