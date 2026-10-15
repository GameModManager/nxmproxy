// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Finding a mod manager by what it **declares**, not by what we guessed.
//!
//! The curated list in [`crate::defaults`] can only find the managers whose
//! desktop file name is already written into it, which was exactly one: ours.
//! HD2Arsenal, a third-party Helldivers 2 manager, was undetectable from any
//! input - not because it is hard to find, but because nothing in this crate
//! had heard of it, and a third-party app never can appear in our own list.
//!
//! What settles it is the declaration the app already publishes:
//!
//! ```ini
//! MimeType=x-scheme-handler/rsnl;x-scheme-handler/nxm;application/x-hd2a-project;x-scheme-handler/nxm;
//! ```
//!
//! `x-scheme-handler/nxm` is the XDG key an application sets to say it takes
//! nxm links. It is not a heuristic and not a name matched against: it is the
//! key the desktop entry specification defines for precisely this, it is what
//! the desktop itself uses to route a link, and MO2 and Vortex publish it the
//! same way. `Categories=` is no help at all - HD2Arsenal ships
//! `Categories=Utility;`, indistinguishable from a text editor - so category
//! matching would have been a guess, and a guess here delivers downloads into
//! the wrong application.
//!
//! Deciding whether a manager is *usable* is deliberately not this module's
//! job. That is `resolve_exec`'s, and keeping it there is why detection can
//! refuse rather than warn.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use crate::config::Manager;

/// The MIME type an application declares to say it takes nxm links.
pub const NXM_MIME: &str = "x-scheme-handler/nxm";

/// The desktop entry group whose keys describe the application itself.
const ENTRY_GROUP: &str = "Desktop Entry";

/// One application that declares it takes nxm links, and how to invoke it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    /// `Name=`, or the file stem when the entry has no name.
    pub name: String,
    /// Where it was found, so a refusal can name the file.
    pub entry: PathBuf,
    /// The declared MIME types, deduplicated. Kept because "why was this
    /// picked" is a question a user asking about a misdetection will ask.
    pub mime_types: Vec<String>,
    pub manager: Manager,
}

/// Every key of every group in a desktop file, grouped.
///
/// A desktop file is several groups and each has its own keys, so reading by
/// scanning for a key does not read the file at all - it reads whichever
/// group happened to come first. That is not hypothetical: `[Desktop Action
/// <id>]` groups carry their own `Exec=`, and in the real HD2Arsenal entry the
/// action's is
/// `AppManager uninstall "/home/petrica/Applications/HD2Arsenal"`. A scanner
/// that takes the last, or the first past the group boundary, hands that to
/// [`crate::route::deliver`] and asks a package manager to delete the user's
/// mod manager. Every lookup below is therefore by group.
type Groups<'a> = BTreeMap<&'a str, Vec<(&'a str, &'a str)>>;

/// Split desktop entry text into its groups.
fn groups_of(text: &str) -> Groups<'_> {
    let mut groups: Groups<'_> = Groups::new();
    let mut current = "";
    for line in text.lines() {
        let line = line.trim();
        // A comment is not a key, and neither is a key that only exists to
        // carry a localised name (`Name[de]=...`). Both are skipped without
        // being mistaken for the key they decorate.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[') {
            current = rest.strip_suffix(']').unwrap_or(rest);
            groups.entry(current).or_default();
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            groups
                .entry(current)
                .or_default()
                .push((key.trim(), value.trim()));
        }
    }
    groups
}

/// One key's value from one group.
fn key<'a>(groups: &'a Groups<'a>, group: &str, name: &str) -> Option<&'a str> {
    groups
        .get(group)?
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
}

/// The MIME types one group declares, in order.
///
/// Repetitions are kept here; [`parse_entry`] is what folds them into a set.
/// The real HD2Arsenal entry lists `x-scheme-handler/nxm` **twice** in one
/// value, so a reader that counts occurrences reports an application as two
/// handlers for the same scheme. It is one.
fn mime_types<'a>(groups: &Groups<'a>, group: &str) -> Vec<&'a str> {
    let mut out = Vec::new();
    for (key, value) in groups.get(group).map_or(&[][..], Vec::as_slice) {
        if *key != "MimeType" {
            continue;
        }
        for mime in value.split(';') {
            let mime = mime.trim();
            if !mime.is_empty() {
                out.push(mime);
            }
        }
    }
    out
}

/// Parse one desktop file into a manager, if it declares nxm handling.
///
/// **`Exec=` comes from `[Desktop Entry]` and nowhere else** - see
/// [`groups_of`] for what goes wrong otherwise.
///
/// **`MimeType` is read from any group.** AppImage-packaged apps put it in the
/// wrong one: the real HD2Arsenal 0.36.2 entry carries
/// `MimeType=x-scheme-handler/rsnl;x-scheme-handler/nxm;...` under
/// `[Desktop Action Uninstall]` and **not at all** under `[Desktop Entry]`, so a
/// strict group-reader does not see HD2Arsenal at all and the whole reason this
/// module exists evaporates. The asymmetry is safe in the one direction that
/// matters: the argv still comes from `[Desktop Entry]`, so a stray `MimeType`
/// under an action group can never make `AppManager uninstall` something we
/// would spawn.
pub fn parse_entry(text: &str, path: &Path) -> Option<Declared> {
    let groups = groups_of(text);

    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for group in groups.keys() {
        declared.extend(mime_types(&groups, group));
    }
    if !declared.contains(NXM_MIME) {
        return None;
    }

    let (exec, args) = crate::parse_exec_line(key(&groups, ENTRY_GROUP, "Exec")?)?;
    // `parse_exec_line` already folded `%u` and `%U` onto `%1`, so this is the
    // whole of "would this ever receive a link".
    if !args.iter().any(|arg| arg == "%1") {
        return None;
    }

    Some(Declared {
        name: key(&groups, ENTRY_GROUP, "Name")
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            }),
        entry: path.to_path_buf(),
        mime_types: declared.into_iter().map(str::to_string).collect(),
        manager: Manager {
            exec,
            args,
            pipe: None,
        },
    })
}

/// The directories an application directory lives in, in search order.
///
/// `$XDG_CONFIG_HOME/applications` is included because the desktop file
/// specification lists it, and because a user who hand-dropped an entry there
/// means it. Each source is read-only here; nothing in this crate writes to
/// any of them except its own published entry.
pub fn applications_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !dirs.contains(&dir) {
            dirs.push(dir);
        }
    };

    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        push(data_home.join("applications"));
    }
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|raw| std::env::split_paths(&raw).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            // The specification's default, which a machine with the variable
            // unset still searches.
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        });
    for dir in data_dirs {
        push(dir.join("applications"));
    }
    if let Some(home) = &home {
        push(home.join(".local").join("share").join("applications"));
    }
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        push(config_home.join("applications"));
    }
    if let Some(home) = &home {
        push(home.join(".config").join("applications"));
    }
    dirs
}

/// The per-user executable directory, `$XDG_BIN_HOME` or `~/.local/bin`.
///
/// **This is a resolution aid, never a detection source.** What is in here says
/// a program runs; it says nothing about whether the program takes an nxm link,
/// and a manager that never receives the link eats every download silently. It
/// is worth knowing because installer-managed apps drop a symlink here for
/// every app they own - on the machine this was written for,
/// `hd2arsenal -> ~/Applications/HD2Arsenal` sits beside 40 others - so it is
/// the usual answer to "where did the name in that `Exec=` line go".
pub fn local_bin() -> Option<PathBuf> {
    std::env::var_os("XDG_BIN_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin")))
}

/// True when `exec` is this very program.
///
/// `install` publishes `nxmproxy.desktop` with `MimeType=x-scheme-handler/nxm`
/// **before** it runs detection, so without this the router finds itself. That
/// is not a cosmetic bug: on a machine with exactly one real mod manager the
/// two candidates are ambiguous, and the flagship out-of-the-box install that
/// used to work would refuse every time.
///
/// nxmproxy is the router, not a destination. A link delivered to it comes back
/// to the process already handling it.
fn is_this_program(exec: &str) -> bool {
    let Ok(ours) = std::env::current_exe() else {
        return false;
    };
    let Some(theirs) = crate::resolve_exec(exec) else {
        return false;
    };
    // Canonicalised, because the published `Exec=` and `current_exe()` reach
    // the same file by different routes on an installed copy.
    let same = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    same(&ours) == same(&theirs)
}

/// True when `entry` is the desktop file [`crate::handle_install`] publishes.
///
/// **By name, because by path it cannot work.** `install` writes
/// `current_exe()`, so an entry written by a *different build* of nxmproxy -
/// an installed 0.2.0 sitting next to a freshly built one - names a different
/// file while being exactly as much the router as we are. Verified against a
/// real machine, where that is the difference between "3 managers found" and
/// "2 managers found, one of them the router".
///
/// The router is not its own destination on any machine.
fn is_our_own_entry(entry: &Path) -> bool {
    entry
        .file_stem()
        .is_some_and(|stem| stem == env!("CARGO_PKG_NAME"))
}

/// Every application on this machine that declares it takes nxm links.
///
/// Sorted, and deduplicated by invocation, so two directories describing the
/// same program (`~/.local/share` and `/usr/share` both carrying a packaged
/// copy) are one candidate rather than an ambiguity invented out of one app.
pub fn declared() -> Vec<Declared> {
    let mut found: Vec<Declared> = Vec::new();
    for dir in applications_dirs() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|e| e == "desktop"))
            .collect();
        // Directory order is filesystem order and varies between machines;
        // this output becomes a message a user reads and a test asserts on.
        files.sort();

        for path in files {
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(candidate) = parse_entry(&text, &path) else {
                continue;
            };
            if is_this_program(&candidate.manager.exec) || is_our_own_entry(&candidate.entry) {
                continue;
            }
            if !found.iter().any(|seen| seen.manager == candidate.manager) {
                found.push(candidate);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The entry exactly as HD2Arsenal 0.36.2 installed it, unmodified.
    ///
    /// Not tidied. The quoted `Exec=`, the `Categories=Utility;` that rules out
    /// category matching, the `MimeType` in the wrong group and the duplicated
    /// `x-scheme-handler/nxm` are all the bugs this module exists for, and a
    /// fixture that tidied them away would test a file nobody has.
    const HD2ARSENAL: &str = r#"[Desktop Entry]
Name=HD2Arsenal
Exec="/home/petrica/Applications/HD2Arsenal" --no-sandbox %U
Type=Application
Icon=hd2arsenal
StartupWMClass=HD2Arsenal
X-AppImage-Version=0.36.2
Comment=Helldivers 2 Companion Desktop Application, Mod Manager, Mod Manifest Builder, Lobby Browser, and more!
Categories=Utility;
Actions=Uninstall;

[Desktop Action Uninstall]
Name=Move to Trash
Exec=/home/petrica/Applications/AppManager uninstall "/home/petrica/Applications/HD2Arsenal"
Icon=user-trash
MimeType=x-scheme-handler/rsnl;x-scheme-handler/nxm;application/x-hd2a-project;x-scheme-handler/nxm;
"#;

    fn arsenal() -> Option<Declared> {
        parse_entry(HD2ARSENAL, Path::new("hd2arsenal.desktop"))
    }

    #[test]
    fn the_real_arsenal_entry_is_detected() {
        let found = arsenal().expect("HD2Arsenal declares x-scheme-handler/nxm");
        assert_eq!(found.name, "HD2Arsenal");
        assert_eq!(found.manager.exec, "/home/petrica/Applications/HD2Arsenal");
        assert_eq!(found.manager.args, vec!["--no-sandbox", "%1"]);
        assert!(found.manager.takes_url(), "it must receive the link");
    }

    #[test]
    fn a_quoted_exec_splits_into_one_argument_per_token() {
        // Whitespace-splitting this line yields `"/home/petrica/Applications/HD2Arsenal"`
        // as argv[0] - quotes attached, so the file does not exist and the
        // manager silently never receives a link.
        let found = arsenal().unwrap();
        assert_eq!(
            found.manager.exec, "/home/petrica/Applications/HD2Arsenal",
            "the quotes must be stripped, or this path resolves to nothing"
        );
        assert!(
            !found.manager.exec.contains('"'),
            "a quote survived into the executable: {:?}",
            found.manager.exec
        );
        assert_eq!(found.manager.args.len(), 2, "{:?}", found.manager.args);
        assert_eq!(found.manager.args[0], "--no-sandbox");
    }

    #[test]
    fn the_uninstall_action_exec_is_not_the_applications_exec() {
        // The action group's Exec is `AppManager uninstall "<the app>"`. Reading
        // Exec by scanning keys instead of by group yields that, and delivering
        // a link to it asks a package manager to uninstall the mod manager.
        let found = arsenal().unwrap();
        assert!(
            !found.manager.exec.contains("AppManager"),
            "picked the [Desktop Action Uninstall] Exec: {:?}",
            found.manager.exec
        );
        assert!(
            !found.manager.args.iter().any(|a| a == "uninstall"),
            "the uninstall verb became an argument: {:?}",
            found.manager.args
        );
    }

    #[test]
    fn a_duplicated_mimetype_key_is_one_handler() {
        let found = arsenal().unwrap();
        assert_eq!(
            found.mime_types.iter().filter(|m| *m == NXM_MIME).count(),
            1,
            "one scheme declared twice is one handler: {:?}",
            found.mime_types
        );
        // The neighbours survive: deduplicating is not truncating the list.
        assert!(found
            .mime_types
            .contains(&"x-scheme-handler/rsnl".to_string()));
        assert!(found
            .mime_types
            .contains(&"application/x-hd2a-project".to_string()));
    }

    #[test]
    fn all_three_url_field_codes_are_accepted() {
        for code in ["%u", "%U", "%1"] {
            let text =
                format!("[Desktop Entry]\nName=m\nExec=/bin/m {code}\nMimeType={NXM_MIME};\n");
            let found = parse_entry(&text, Path::new("m.desktop"))
                .unwrap_or_else(|| panic!("{code} was not accepted as a url field"));
            assert_eq!(found.manager.args, vec!["%1"], "{code} maps to %1");
        }
    }

    #[test]
    fn an_entry_with_no_url_field_is_not_a_manager_we_can_use() {
        // It declares nxm handling but takes nothing. Registering it would
        // start a manager for every download and hand it nothing.
        let text = format!("[Desktop Entry]\nName=m\nExec=/bin/m --quiet\nMimeType={NXM_MIME};\n");
        assert_eq!(parse_entry(&text, Path::new("m.desktop")), None);
    }

    #[test]
    fn an_entry_that_does_not_declare_nxm_is_ignored() {
        // `Categories=Utility;` is what HD2Arsenal ships. Matching on category
        // would take this file, a text editor, and a calculator.
        let text = "[Desktop Entry]\nName=Text Editor\nExec=/bin/editor %U\n\
                    Categories=Utility;Network;\n";
        assert_eq!(parse_entry(text, Path::new("editor.desktop")), None);
    }

    #[test]
    fn a_file_with_no_exec_line_is_refused_rather_than_guessed() {
        let text = format!("[Desktop Entry]\nName=m\nMimeType={NXM_MIME};\n");
        assert_eq!(parse_entry(&text, Path::new("m.desktop")), None);
    }

    #[test]
    fn an_entry_with_no_name_falls_back_to_the_file_stem() {
        // The fixture the existing detection tests write has no `Name=` at all,
        // so this is the shape a test and a hand-written entry take.
        let text = format!("[Desktop Entry]\nExec=/bin/m %U\nMimeType={NXM_MIME};\n");
        let found = parse_entry(&text, Path::new("gamemodmanager-nxm.desktop")).unwrap();
        assert_eq!(found.name, "gamemodmanager-nxm");
    }

    #[test]
    fn scanning_is_read_only_over_the_application_directories() {
        // Precondition for the tests above being meaningful: they parse text,
        // they do not depend on this machine having any desktop installed.
        assert!(applications_dirs()
            .iter()
            .all(|d| d.ends_with("applications")));
        for dir in applications_dirs() {
            assert!(
                !dir.as_os_str().is_empty(),
                "an empty directory path would scan the working directory"
            );
        }
    }
}
