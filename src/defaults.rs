// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! A curated list of mod managers that take an nxm link on their command line.
//!
//! Upstream had none, so this is seeded rather than inherited. It is **data,
//! not behaviour**: nothing here is ever auto-registered. A manager only
//! handles links once the user runs `register-manager` with a real path, and
//! an entry whose executable we could not verify says so in its note rather
//! than inventing a path that would fail at click time.
//!
//! Every one of these managers has the same interface and nothing richer: it
//! takes the uri as a command line argument. No mod manager exposes a routing
//! API, so there is nothing else to model.

/// One entry in the curated list.
#[derive(Debug)]
pub struct Known {
    /// Suggested id for `register-manager`.
    pub id: &'static str,
    /// Display name.
    pub name: &'static str,
    /// Executable, where it is known.
    pub exec: &'static str,
    /// Arguments, with `%1` where the link goes.
    pub args: &'static [&'static str],
    /// Honest note about where this came from and what is not verified.
    pub note: &'static str,
}

pub const KNOWN: &[Known] = &[
    Known {
        id: "gamemodmanager",
        name: "GameMod Manager",
        exec: "gamemodmanager",
        args: &["--handle-nxm", "%1"],
        note: "Our own manager. On PATH by default after installation.",
    },
    Known {
        id: "vortex",
        name: "Vortex Mod Manager",
        exec: r"C:\Program Files (x86)\Black Tree Gaming\Vortex\Vortex.exe",
        args: &["%1"],
        note: "Windows default install path. Also exposes a named pipe: vortex_download.",
    },
    Known {
        id: "nxmhandler",
        name: "Mod Organizer 2 NXM Handler",
        exec: "nxmhandler.exe",
        args: &["%1"],
        note: "Installed by MO2 itself, path varies. Resolved through PATH. \
               On Linux MO2 uses a shell script broker that sets up Proton first.",
    },
    Known {
        id: "nmm",
        name: "Nexus Mods Manager (legacy)",
        exec: r"C:\Program Files (x86)\Nexus Mods\Nexus Mods Manager\NMM.exe",
        args: &["%1"],
        note: "Legacy and no longer maintained. Listed because installs still exist.",
    },
    Known {
        id: "stardrop",
        name: "Stardrop",
        exec: "stardrop",
        args: &["%1"],
        note: "Floogen/Stardrop. Linux focused. Executable path depends on how you installed it.",
    },
    Known {
        id: "arsenal",
        name: "Arsenal",
        exec: "arsenal",
        args: &["%1"],
        note: "Helldivers 2. UNVERIFIED: no public source, so the executable name is a guess. \
               Set the real path yourself.",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_is_usable_as_registered() {
        for known in KNOWN {
            assert!(!known.id.trim().is_empty(), "{known:?}");
            assert!(!known.name.trim().is_empty(), "{}", known.id);
            assert!(!known.exec.trim().is_empty(), "{}", known.id);
            assert!(
                known.args.iter().any(|a| a.contains("%1")),
                "{} would never receive a url",
                known.id
            );
            assert!(
                !known.note.trim().is_empty(),
                "{} has no provenance note",
                known.id
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<&str> = KNOWN.iter().map(|k| k.id).collect();
        ids.sort_unstable();
        let count = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), count, "duplicate manager id in the curated list");
    }
}
