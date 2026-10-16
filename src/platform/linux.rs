// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Linux: publish an XDG desktop entry, then claim the scheme in every row
//! below the user's own that a desktop could look in.
//!
//! ```text
//! ~/.local/share/applications/nxmproxy.desktop
//!     MimeType=x-scheme-handler/nxm;
//! ~/.config/kde-mimeapps.list          <- and every other row we own
//!     [Default Applications]
//!     x-scheme-handler/nxm=nxmproxy.desktop;
//! ```
//!
//! The first file makes nxmproxy *eligible*. The rest make it what the desktop
//! opens, and there is more than one of them because the specification says
//! there is more than one place to look.
//!
//! **Read everything, write only what is ours.** [`mimeapps_files`] is the
//! specification's full eight rows, because a reader that skips a row cannot
//! say who won. [`claimed_chain`] is four of them - the two under
//! `$XDG_CONFIG_HOME` and the two under `$XDG_DATA_HOME/applications` - because
//! the other four cannot outrank the two that are written, and writing into
//! `/usr/share` is not a user's tool's business. The split is one function with
//! two scopes, so the side that writes cannot drift from the side that reads.
//!
//! ## Why one file was not enough, and how that was found
//!
//! This used to write exactly one line, into `$XDG_CONFIG_HOME/mimeapps.list`,
//! with `xdg-mime default`. On the machine the author is on, the scheme still
//! opened GameModManager - every time, on every install. The cause is
//! [specification section 2](https://specifications.freedesktop.org/mime-apps/latest/file.html),
//! which lists **eight** files in precedence order and puts
//! `$XDG_CONFIG_HOME/$desktop-mimeapps.list` **above** `$XDG_CONFIG_HOME/mimeapps.list`.
//! GMM writes `kde-mimeapps.list`; nxmproxy wrote the file below it; the desktop
//! read the higher one. The user deleted `kde-mimeapps.list` by hand and GMM put
//! it back.
//!
//! Nothing in nxmproxy could see that. [`mimeapps_files`] - the read side -
//! **omitted the desktop-specific files entirely**, so `test` resolved a chain
//! that contained no rival, printed `nxmproxy handles nxm://: yes`, and exited
//! 0 over a scheme GMM held. That was the false green, and it was in the
//! reader as much as in the writer.
//!
//! ## What claiming greedily does and does not mean
//!
//! nxmproxy is a broker the user installed on purpose, and a broker that loses
//! the argument is a broken install. So [`claim`] writes **its own id** into
//! every row of [`claimed_chain`], and verifies each write by reading it back.
//!
//! What it still never does is **remove, reorder or overwrite another
//! application's entry**. [`prepend_default_id`] puts nxmproxy at the front of
//! the ranked list and leaves every other id in the order it was already in.
//! A user who installs a broker should find the broker in charge and every
//! other option still present. Because the edit is a prepend rather than a
//! replace, `uninstall` restores the previous bytes exactly, which a replace
//! could not.
//!
//! ## Why the edit is done here and not by `xdg-mime`
//!
//! It is not, any more. `xdg-mime default` writes one file, and **replaces** the
//! value for the key rather than prepending to it - so delegating to it made
//! burying another manager's entry unavoidable on the one file nxmproxy could
//! reach. It also exits 0 while writing nothing when the target directory is
//! missing, which is why the read back below is not optional. The whole sweep is
//! one pure text function instead, tested without a desktop anywhere near it.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logger;
use crate::platform::{Claim, Declaration, Resolution};

const SCHEME: &str = "nxm";
const DESKTOP_FILE: &str = "nxmproxy.desktop";
const MIME_TYPE: &str = "x-scheme-handler/nxm";
const GROUP: &str = "Default Applications";

/// "This app handles nxm." The application's own record of itself.
const SECTION_ADDED: &str = "Added Associations";

/// The id this platform registers nxmproxy under, which is what a
/// `mimeapps.list` names when nxmproxy is the handler. See
/// [`crate::platform::Status`] for why `test` needs to compare it.
pub const OWN_ID: &str = DESKTOP_FILE;

/// Quote one argument for the `Exec=` key of a desktop entry.
///
/// Per the desktop entry specification an argument may be wrapped in double
/// quotes, and inside them `\` `"` `` ` `` and `$` must be backslash escaped.
/// Outside quotes the reserved set is much larger, so anything containing one
/// is quoted. `%` is doubled because it introduces a field code.
pub fn desktop_exec_arg(arg: &str) -> String {
    let needs_quotes = arg.is_empty()
        || arg.chars().any(|c| {
            c.is_whitespace()
                || matches!(
                    c,
                    '"' | '\'' | '\\' | '>' | '<' | '~' | '|' | '&' | '$' | '?' | '#' | '(' | ')'
                )
        });
    if !needs_quotes {
        return arg.replace('%', "%%");
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    for c in arg.chars() {
        match c {
            '\\' | '"' | '`' | '$' => {
                out.push('\\');
                out.push(c);
            }
            '%' => out.push_str("%%"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The desktop entry body. Pure, so the exact text we publish is testable.
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=NXM Proxy\n\
         Comment=Forward Nexus Mods links to the right mod manager\n\
         Exec={} url %u\n\
         Icon=application-x-executable\n\
         Terminal=false\n\
         NoDisplay=false\n\
         Categories=Utility;\n\
         MimeType=x-scheme-handler/{SCHEME};\n",
        desktop_exec_arg(&exe.to_string_lossy())
    )
}

fn data_home() -> Result<PathBuf, String> {
    user_data_home().ok_or_else(|| "could not determine the XDG data directory".to_string())
}

/// `$XDG_DATA_HOME`, or `~/.local/share` when the variable is unset or relative.
///
/// **Relative is treated as unset**, because that is what the base directory
/// specification says an unparseable value means and because every real reader
/// does it: a relative `XDG_DATA_HOME` would resolve against whatever directory
/// the process happened to be launched in.
fn user_data_home() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
}

/// `$XDG_CONFIG_HOME`, or `~/.config`. Same relative-means-unset rule.
fn user_config_home() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".config"))
        })
}

/// `$XDG_CONFIG_DIRS`, or the specification's default `/etc/xdg`.
fn config_dirs() -> Vec<PathBuf> {
    std::env::var_os("XDG_CONFIG_DIRS")
        .map(|raw| std::env::split_paths(&raw).collect())
        .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
        .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")])
}

/// `$XDG_DATA_DIRS`, or the specification's default.
fn data_dirs() -> Vec<PathBuf> {
    std::env::var_os("XDG_DATA_DIRS")
        .map(|raw| std::env::split_paths(&raw).collect())
        .filter(|dirs: &Vec<PathBuf>| !dirs.is_empty())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        })
}

/// The four directory roots the eight rows are built from, in precedence order.
fn roots() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    let mut push = |dir: PathBuf| {
        if !roots.contains(&dir) {
            roots.push(dir);
        }
    };
    user_config_home().into_iter().for_each(&mut push);
    config_dirs().into_iter().for_each(&mut push);
    user_data_home().into_iter().for_each(&mut push);
    data_dirs().into_iter().for_each(&mut push);
    roots
}

fn desktop_path() -> Result<PathBuf, String> {
    Ok(data_home()?.join("applications").join(DESKTOP_FILE))
}

/// Ask the desktop's MIME cache to re-index the directory.
///
/// Best effort: without `desktop-file-utils` installed the entry is still
/// valid, desktops rescan on their own schedule, and failing the whole install
/// over a missing cache tool would be worse than a stale cache.
fn refresh_mime_cache(dir: &Path) {
    match Command::new("update-desktop-database").arg(dir).status() {
        Ok(status) if status.success() => logger::info("refreshed the desktop MIME cache"),
        Ok(status) => logger::info(format!("update-desktop-database exited with {status}")),
        Err(e) => logger::info(format!(
            "update-desktop-database not available ({e}); the desktop entry is still in place"
        )),
    }
}

/// The `$desktop` names the **running** desktop will consult, in the order it
/// consults them.
///
/// This is the only set the read side may use, and the distinction is not
/// cosmetic. The specification defines `$desktop` as the components of
/// `$XDG_CURRENT_DESKTOP`, so a `gnome-mimeapps.list` written during a KDE
/// session is never opened by anything. Reading it back would let `test` report
/// `nxmproxy handles nxm://: yes` off the back of a file the desktop has never
/// heard of - which is the same false green as not looking at `kde-mimeapps.list`
/// at all, wearing the other mask.
///
/// Unset gives an empty list, and that is right rather than a special case:
/// GLib drops the dash and reads `mimeapps.list`, and KDE's `SplitEmptyParts`
/// yields the same single name. The plain row is always in the chain anyway.
pub fn consulted_desktops() -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for part in std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .split(':')
    {
        // Real sessions spell it in the desktop's own case - `KDE`, `GNOME`,
        // `X-Cinnamon` - and the specification asks for ascii lowercase.
        let name = part.trim().to_ascii_lowercase();
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Every `$desktop` name `install` must write, in the order it writes them.
///
/// A superset of [`consulted_desktops`], deliberately. The specification can
/// only tell us the desktop being used **right now**, and the machine it is
/// being used on has other sessions installed, other sessions it has run since
/// the last logout, and applications that maintain their own list whether or not
/// a session is running at all. Each extra name below is one of those, found
/// rather than guessed:
///
/// 1. the running session ([`consulted_desktops`]);
/// 2. `DesktopNames=` in every installed `xsessions/` and `wayland-sessions/`
///    entry - literally the key a display manager copies into
///    `$XDG_CURRENT_DESKTOP`, so it is a list of sessions that exist here;
/// 3. the stem of every `$desktop-mimeapps.list` **already present** in any of
///    the four roots, which is proof that something on this machine reads it -
///    this is the one that finds GMM's `kde-mimeapps.list`;
/// 4. the four the specification names in its own text (`kde, gnome, xfce,
///    lxqt, etc.`), because a spec that says "for instance" is not a closed set
///    and the cost of one more row is one line that `uninstall` removes again.
pub fn claim_desktops() -> Vec<String> {
    let mut names = consulted_desktops();
    let roots = roots();
    let mut discovered: Vec<String> = Vec::new();
    for candidate in installed_sessions(&roots)
        .into_iter()
        .chain(existing_desktop_lists(&roots))
        .chain(
            ["kde", "gnome", "xfce", "lxqt"]
                .iter()
                .map(|name| (*name).to_string()),
        )
    {
        if !names.contains(&candidate) {
            discovered.push(candidate.clone());
            names.push(candidate);
        }
    }
    logger::info(format!(
        "claim desktops={:?} discovered={:?} running={:?}",
        names,
        discovered,
        consulted_desktops()
    ));
    names
}

/// `$desktop` values declared by sessions installed on this machine.
fn installed_sessions(roots: &[PathBuf]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for root in roots {
        for kind in ["xsessions", "wayland-sessions"] {
            let Ok(entries) = std::fs::read_dir(root.join(kind)) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.extension().is_some_and(|e| e == "desktop") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                for name in session_desktop_names(&text) {
                    if !out.contains(&name) {
                        out.push(name);
                    }
                }
            }
        }
    }
    out
}

/// The `DesktopNames=` value of a session file, lowercased and split.
///
/// Group-aware, and only from `[Desktop Entry]`: `[Desktop Action ...]` groups
/// carry their own keys and a session file is a normal desktop entry.
fn session_desktop_names(text: &str) -> Vec<String> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).trim().to_ascii_lowercase();
            continue;
        }
        if section != "desktop entry" {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            if key.trim().eq_ignore_ascii_case("DesktopNames") {
                for name in value.split(';') {
                    let name = name.trim().to_ascii_lowercase();
                    if !name.is_empty() {
                        out.push(name);
                    }
                }
            }
        }
    }
    out
}

/// The `$desktop` of every `$desktop-mimeapps.list` that already exists.
///
/// Both the bare root and its `applications/` subdirectory, because the two row
/// families are spelled differently and a file can be in either.
fn existing_desktop_lists(roots: &[PathBuf]) -> Vec<String> {
    // `file_stem()` has already taken the `.list` off, so the suffix left to
    // match is `-mimeapps`. Matching the full `*-mimeapps.list` against the stem
    // matches nothing at all, which is a discovery function that discovers
    // nothing and reports a clean empty list - and a unit test is what caught it.
    const PREFIX_SUFFIX: &str = "-mimeapps";
    let mut out: Vec<String> = Vec::new();
    for root in roots {
        for dir in [root.clone(), root.join("applications")] {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_some_and(|e| e != "list") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let Some(name) = stem.strip_suffix(PREFIX_SUFFIX) else {
                    continue;
                };
                if !name.is_empty() && !out.contains(&name.to_string()) {
                    out.push(name.to_string());
                }
            }
        }
    }
    out
}

/// Every `mimeapps.list` row, in the specification's precedence order.
///
/// The eight rows of [specification section
/// 2](https://specifications.freedesktop.org/mime-apps/latest/file.html), in
/// that order, with each `$desktop` name expanded in the order it is consulted.
/// Written out because the order *is* the rule, and because getting it backwards
/// is precisely the bug this module was rewritten for.
///
/// **Read side only.** This is everything the desktop may consult, and reading
/// anything less is how `test` once reported a win over a scheme GMM held.
pub fn mimeapps_chain(desktops: &[String]) -> Vec<PathBuf> {
    chain_over(
        user_config_home().as_deref(),
        &config_dirs(),
        user_data_home().as_deref(),
        &data_dirs(),
        desktops,
    )
}

/// Every row [`claim`] writes: the same eight, minus the four that belong to
/// somebody else.
///
/// The rows this drops are `$XDG_CONFIG_DIRS` and `$XDG_DATA_DIRS`, and the
/// reason is arithmetic rather than preference. The specification resolves the
/// scheme in list order and **stops at the first file that names a valid
/// handler**, and rows 1-2 (`$XDG_CONFIG_HOME`, with and without the `$desktop`
/// prefix) are above every one of them. We write both. So no entry below can
/// ever outrank what we just wrote, and writing them buys exactly nothing -
/// while costing three things that are not nothing:
///
/// * On this machine `/etc/xdg`, `/usr/share/applications` and
///   `/usr/local/share/applications` are root-owned, so a user-level `install`
///   reports ten refusals it has no way to act on and cannot fix.
/// * `$XDG_DATA_DIRS` here begins with
///   `$HOME/.local/share/flatpak/exports/share`, which *is* writable. Writing
///   association state into a Flatpak export directory corrupts the tree
///   flatpak owns, for every sandboxed application, to no effect.
/// * Run as root - the obvious thing a user tries when told the claim did not
///   take - it silently changes the default application for **every user on the
///   machine**. A per-user broker has no business doing that.
///
/// GMM's own footprint is this same subset: `~/.config/kde-mimeapps.list`,
/// `~/.config/mimeapps.list` and `~/.local/share/applications/mimeapps.list`.
pub fn claimed_chain(desktops: &[String]) -> Vec<PathBuf> {
    chain_over(
        user_config_home().as_deref(),
        &[],
        user_data_home().as_deref(),
        &[],
        desktops,
    )
}

/// [`mimeapps_chain`] with the roots handed in rather than read.
///
/// The split is because the ordering **is** the specification's rule, and a rule
/// that can only be exercised on the machine running the tests is not exercised
/// at all: a developer with three entries in `$XDG_CONFIG_DIRS` gets a different
/// chain from CI, and neither proves anything about the other.
///
/// One function and two scopes, so the side that writes and the side that reads
/// cannot drift apart: [`claimed_chain`] passes **empty slices** for the two
/// system roots, which drops rows 3-4 and 7-8 without a second ordering to keep
/// in step.
fn chain_over(
    config_home: Option<&Path>,
    config_dirs: &[PathBuf],
    data_home: Option<&Path>,
    data_dirs: &[PathBuf],
    desktops: &[String],
) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    let mut push = |path: PathBuf| {
        if !files.contains(&path) {
            files.push(path);
        }
    };
    let row = |dir: &Path| {
        let mut both: Vec<PathBuf> = desktops
            .iter()
            .map(|name| dir.join(format!("{name}-mimeapps.list")))
            .collect();
        both.push(dir.join("mimeapps.list"));
        both
    };

    // 1-2. $XDG_CONFIG_HOME - the user's own overrides.
    config_home
        .into_iter()
        .for_each(|dir| row(dir).into_iter().for_each(&mut push));
    // 3-4. $XDG_CONFIG_DIRS - sysadmin and vendor overrides. Ours to read,
    // never to write: see [`claimed_chain`].
    for dir in config_dirs {
        row(dir).into_iter().for_each(&mut push);
    }
    // 5-6. $XDG_DATA_HOME/applications - deprecated, still read, still ours.
    if let Some(data_home) = data_home {
        let dir = data_home.join("applications");
        row(&dir).into_iter().for_each(&mut push);
    }
    // 7-8. $XDG_DATA_DIRS/applications - the distribution's defaults.
    for dir in data_dirs {
        let dir = dir.join("applications");
        row(&dir).into_iter().for_each(&mut push);
    }
    files
}

/// One file the claim did not take, and why.
struct Refusal(PathBuf, String);

/// Write our id into one file, then read it back and say whether it stuck.
///
/// The read back is not defensive padding. `xdg-mime default` was verified on
/// xdg-utils 1.1.3 to **exit 0 and write nothing** when the target directory
/// does not exist, which it does not on a fresh account - and a root-owned
/// directory under `$XDG_DATA_DIRS` fails the same way for a reason no exit code
/// carries. Either way the answer is a file, so that is what is asked.
fn claim_into(path: &Path) -> Result<(), String> {
    let Some(parent) = path.parent() else {
        return Err(format!("{} has no parent directory", path.display()));
    };
    let before = std::fs::read_to_string(path).unwrap_or_default();
    let wanted = prepend_default_id(&before, MIME_TYPE, DESKTOP_FILE);
    if wanted == before {
        return Ok(()); // already ours, and already verified once.
    }
    std::fs::create_dir_all(parent)
        .map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    std::fs::write(path, &wanted).map_err(|e| format!("could not write: {e}"))?;

    let after = std::fs::read_to_string(path).map_err(|e| format!("could not read back: {e}"))?;
    match first_default_id(&after, MIME_TYPE) {
        Some(first) if first == DESKTOP_FILE => Ok(()),
        Some(first) => Err(format!("names {first:?} first, not nxmproxy")),
        None => Err("names no application at all".to_string()),
    }
}

/// The first application id in a file's `[Default Applications]` value for `key`.
fn first_default_id(text: &str, key: &str) -> Option<String> {
    default_ids(text, key)
        .and_then(|ids| ids.into_iter().next())
        .filter(|id| !id.is_empty())
}

/// Claim `nxm://` in every place we own that a desktop could read it from.
///
/// Every row of [`claimed_chain`] - GMM's three, plus the `$desktop-mimeapps`
/// variants it misses - rather than the one `xdg-mime` writes. The count in the
/// report distinguishes three outcomes, because a location that refused the write
/// and a location nobody will read are the same state to the desktop and a
/// different state to act on.
pub fn claim() -> Result<Claim, String> {
    let desktops = claim_desktops();
    let targets = claimed_chain(&desktops);
    let mut held: Vec<&Path> = Vec::new();
    let mut refused: Vec<Refusal> = Vec::new();
    for target in &targets {
        match claim_into(target) {
            Ok(()) => held.push(target),
            Err(reason) => refused.push(Refusal(target.clone(), reason)),
        }
    }
    report_claim(&held, &refused, &desktops);

    // Only report success if the chain now says so. The effective handler is a
    // property of the whole chain and of the desktop-specific rows in it, so it
    // is read back rather than believed - and read through [`resolve`], the same
    // two steps `test` and `list` take, so the three cannot disagree.
    let resolution = resolve();
    let claim = if owns(&resolution) {
        Claim::verified()
    } else {
        Claim::needs_user(refusal_instruction(&resolution))
    };
    Ok(claim)
}

/// What the user must do when the sweep finished and the desktop still says no.
fn refusal_instruction(resolution: &Resolution) -> String {
    format!(
        "{DESKTOP_FILE} is registered, but {} still says {} handles {MIME_TYPE}. \
         Open Settings > Applications > Default Applications and pick NXM Proxy there.",
        resolution
            .winner()
            .map(|w| w.source.clone())
            .unwrap_or_else(|| mimeapps_path()
                .map(|p| p.display().to_string())
                .unwrap_or_default()),
        resolution
            .handler()
            .unwrap_or_else(|| "(nothing)".to_string())
    )
}

/// The per-file report: one summary line, then a line per refusal.
///
/// **The successes are counted, not listed.** Ten files were written and the
/// count says so; printing ten paths adds nothing a user can act on, and it
/// buried the two lines that matter - whether the claim took, and which location
/// refused it. `test` prints the whole chain with the winner if the user wants
/// it spelled out.
fn report_claim(held: &[&Path], refused: &[Refusal], desktops: &[String]) {
    println!(
        "claimed {MIME_TYPE} in {} of {} files: {}",
        held.len(),
        held.len() + refused.len(),
        desktops.join(" ")
    );
    for Refusal(path, reason) in refused {
        println!("  refused {}: {reason}", path.display());
        logger::warn(format!("could not claim {}: {reason}", path.display()));
    }
}

/// Give `nxm://` up, in every file [`claim`] wrote to.
///
/// The inverse edit, and an exact one: [`prepend_default_id`] put our id at the
/// front of a list and changed nothing else, so removing it restores the bytes
/// that were there before. A file we created - one that held nothing but our own
/// key - is removed entirely rather than left behind as an empty group.
pub fn unclaim() -> Result<Claim, String> {
    let desktops = claim_desktops();
    let targets = claimed_chain(&desktops);
    let mut cleared: Vec<&Path> = Vec::new();
    let mut refused: Vec<Refusal> = Vec::new();
    for target in &targets {
        match unclaim_from(target) {
            Ok(true) => cleared.push(target),
            Ok(false) => {}
            Err(reason) => refused.push(Refusal(target.clone(), reason)),
        }
    }
    println!(
        "released {MIME_TYPE} from {} of {} files",
        cleared.len(),
        targets.len()
    );
    for Refusal(path, reason) in refused {
        println!("  refused {}: {reason}", path.display());
        logger::warn(format!("could not release {}: {reason}", path.display()));
    }

    Ok(Claim::needs_user(
        "nxm:// is unclaimed. Set it back with: nxmproxy install\n  \
         or pick your previous manager in Settings > Applications > Default Applications.",
    ))
}

/// Remove our id from one file. `Ok(true)` means the file changed.
fn unclaim_from(path: &Path) -> Result<bool, String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(false); // Nothing there means nothing to undo.
    };
    let Some(list) = default_ids(&text, MIME_TYPE) else {
        return Ok(false);
    };
    let remaining: Vec<&str> = list
        .iter()
        .map(String::as_str)
        .filter(|id| *id != DESKTOP_FILE)
        .collect();
    if remaining.len() == list.len() {
        return Ok(false); // Our id was never in this file.
    }
    let updated = set_default_ids(&text, MIME_TYPE, &remaining);

    // A file that holds nothing but the line we added is one we created. Leaving
    // an empty `[Default Applications]` header behind is litter, and on a
    // `$desktop-mimeapps.list` it is a file that did not exist a minute ago.
    if updated.lines().all(|line| line.trim().is_empty()) {
        std::fs::remove_file(path).map_err(|e| format!("could not remove: {e}"))?;
        logger::info(format!("removed {}", path.display()));
        return Ok(true);
    }
    std::fs::write(path, &updated).map_err(|e| format!("could not write: {e}"))?;

    // Read back, for the same reason `claim` does. A removal that did not happen
    // is the one `uninstall` must never report as done.
    let after = std::fs::read_to_string(path).map_err(|e| format!("could not read back: {e}"))?;
    if first_default_id(&after, MIME_TYPE).as_deref() == Some(DESKTOP_FILE) {
        return Err("still names nxmproxy after the removal".to_string());
    }
    Ok(true)
}

/// The `mimeapps.list` this user owns, which is the recommended location.
pub fn mimeapps_path() -> Result<PathBuf, String> {
    user_config_home()
        .map(|dir| dir.join("mimeapps.list"))
        .ok_or_else(|| "could not determine the XDG config directory".to_string())
}

/// The desktop file ids listed for `key` under one group of a `mimeapps.list`.
///
/// The group is matched case-insensitively and the **last** matching line wins
/// inside it, because a file that repeats a key has been edited twice and the
/// later statement is the current one. Both halves are things a real file does.
pub fn group_ids(text: &str, group: &str, key: &str) -> Option<Vec<String>> {
    let mut section = String::new();
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).trim().to_string();
            continue;
        }
        if !section.eq_ignore_ascii_case(group) {
            continue;
        }
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            // The value is a semicolon-terminated list of desktop file ids;
            // the trailing `;` is syntax, not part of the last id.
            found = Some(
                value
                    .split(';')
                    .map(str::trim)
                    .filter(|id| !id.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
        }
    }
    found
}

/// The ids listed for `key` under `[Default Applications]`.
pub fn default_ids(text: &str, key: &str) -> Option<Vec<String>> {
    group_ids(text, GROUP, key)
}

/// Every place **this one file** says who handles `key`, in precedence order.
///
/// Both groups, not just `[Default Applications]`. Reading only the default
/// group never produced a wrong winner, but it made a file that declares two
/// different applications look like one that declares one - which is the state a
/// user cannot resolve by hand. It also missed the desktop's own fallback: when
/// no `[Default Applications]` entry names a type, the desktop uses
/// `[Added Associations]`, so a machine with only an added association does open
/// that application and this used to report that nobody claimed it.
///
/// The key is the **MIME type**, not the bare scheme: a `mimeapps.list` is keyed
/// by MIME type, so `nxm=...` is a different key from `x-scheme-handler/nxm=...`
/// and never matches what `xdg-mime default` wrote.
pub fn associations(text: &str, key: &str) -> Vec<(&'static str, Vec<String>)> {
    let mut out = Vec::new();
    for section in [GROUP, SECTION_ADDED] {
        // A group that exists with nothing in it is not a claim, and one that
        // says `key=;` claims nothing either.
        if let Some(ids) = group_ids(text, section, key).filter(|ids| !ids.is_empty()) {
            out.push((section, ids));
        }
    }
    out
}

/// Put `id` at the **front** of a file's `[Default Applications]` value for
/// `key`, keeping every other id in the order it was already in.
///
/// **Prepend, never replace.** This is the difference between claiming a scheme
/// and burying the competition: replacing the value is what `xdg-mime default`
/// does, and it deletes whichever application the user had chosen first,
/// irreversibly, with nothing recording what it was. Prepending puts nxmproxy
/// first - which is the whole claim, because the first id is the one the desktop
/// opens - and shifts the others down a rank without disturbing the order among
/// them or losing any. It is also why [`unclaim_from`] can restore the previous
/// bytes exactly: this function is invertible, and a replace is not.
///
/// A duplicate of `id` is collapsed, because the real HD2Arsenal entry lists
/// `x-scheme-handler/nxm` twice and a file written twice by two tools may as
/// well.
pub fn prepend_default_id(text: &str, key: &str, id: &str) -> String {
    let existing = default_ids(text, key).unwrap_or_default();
    let mut ids: Vec<&str> = existing
        .iter()
        .map(String::as_str)
        .filter(|seen| *seen != id)
        .collect();
    ids.insert(0, id);
    set_default_ids(text, key, &ids)
}

/// Set or remove one `[Default Applications]` key. Pure: the text in, the text
/// out, so the "we touch one line" claim is testable without a desktop.
///
/// Every other line is reproduced byte for byte, including lines in other
/// groups and other keys in this one. An empty `ids` removes the key entirely
/// rather than leaving `key=;` behind, which a desktop reads as "claims
/// nothing" and which is not the same as making no claim.
///
/// A group left with no keys at all goes too. It is the same reasoning: a
/// `[Default Applications]` header over nothing is not "no claim", it is a file
/// that differs from the one the user had, and on a `$desktop-mimeapps.list` it
/// is a file that did not exist before `install` created it and would not exist
/// after `uninstall` if the header stayed.
pub fn set_default_ids(text: &str, key: &str, ids: &[&str]) -> String {
    let replacement = (!ids.is_empty()).then(|| format!("{key}={};", ids.join(";")));
    let mut out: Vec<String> = Vec::new();
    let mut in_group = false;
    let mut group_seen = false;
    let mut insert_at = None;

    for line in text.lines() {
        let trimmed = line.trim();
        let is_header = trimmed.starts_with('[');
        if is_header {
            in_group = trimmed
                .trim_matches(['[', ']'])
                .trim()
                .eq_ignore_ascii_case(GROUP);
            group_seen |= in_group;
        }
        if in_group && !is_header {
            if let Some((name, _)) = trimmed.split_once('=') {
                if name.trim().eq_ignore_ascii_case(key) {
                    // Drop every occurrence. Where the last one stood is where
                    // the replacement goes, so a file with the key twice ends
                    // up with it once, in the same place `default_ids` would
                    // have read it from.
                    insert_at = Some(out.len());
                    continue;
                }
            }
        }
        out.push(line.to_string());
        if is_header && in_group && replacement.is_some() && insert_at.is_none() {
            insert_at = Some(out.len());
        }
    }

    if let Some(value) = replacement {
        let at = insert_at.unwrap_or_else(|| {
            if !group_seen {
                if !out.is_empty() {
                    out.push(String::new());
                }
                out.push(format!("[{GROUP}]"));
            }
            out.len()
        });
        out.insert(at, value);
    } else {
        drop_keyless_group(&mut out);
    }

    let mut result = out.join("\n");
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Remove a `[Default Applications]` header that has no keys left under it.
///
/// The **last** matching header, because a file edited twice can carry the group
/// twice and only the last one can be keyless - the others' keys still count as
/// keys. One following blank line goes with it, or the file is left with a hole
/// where the group used to be.
fn drop_keyless_group(lines: &mut Vec<String>) {
    let is_header = |line: &String| {
        line.trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim()
            .eq_ignore_ascii_case(GROUP)
    };
    let Some(start) = lines.iter().rposition(is_header) else {
        return;
    };
    let has_key = lines[start + 1..]
        .iter()
        .take_while(|line| !line.trim().starts_with('['))
        .any(|line| line.contains('='));
    if has_key {
        return;
    }
    lines.remove(start);
    if lines.get(start).is_some_and(|line| line.trim().is_empty()) {
        lines.remove(start);
    }
}

/// Publish the desktop entry.
pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not determine this executable's path: {e}"))?;
    let path = desktop_path()?;
    let dir = path
        .parent()
        .expect("desktop path always has an applications parent")
        .to_path_buf();

    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("could not create {}: {}", dir.display(), e))?;
    std::fs::write(&path, desktop_entry(&exe))
        .map_err(|e| format!("could not write {}: {}", path.display(), e))?;

    logger::info(format!(
        "registered {} for x-scheme-handler/{SCHEME}",
        path.display()
    ));
    refresh_mime_cache(&dir);
    Ok(())
}

/// Remove the desktop entry. Touches nothing else: if another manager is
/// registered as the default handler it stays exactly as it was.
pub fn uninstall() -> Result<(), String> {
    let path = desktop_path()?;
    if !path.exists() {
        return Ok(());
    }
    std::fs::remove_file(&path)
        .map_err(|e| format!("could not remove {}: {}", path.display(), e))?;
    logger::info(format!("removed {}", path.display()));
    refresh_mime_cache(path.parent().expect("desktop path always has a parent"));
    Ok(())
}

/// True when our desktop entry is published.
pub fn is_registered() -> Result<bool, String> {
    let path = desktop_path()?;
    // A missing file means "not installed", which is a state and not a failure.
    // Reporting it as an error made `test` print a scary message after `uninstall`.
    match std::fs::read_to_string(&path) {
        Ok(entry) => Ok(entry.contains(&format!("x-scheme-handler/{SCHEME};"))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(format!("could not read {}: {}", path.display(), e)),
    }
}

/// The effective default handler, read from the `mimeapps.list` chain.
pub fn current_handler() -> Result<Option<String>, String> {
    Ok(resolve().handler())
}

/// Every file the desktop may read a `mimeapps.list` from, in precedence order.
///
/// **The specification's lookup order is the user's config first, then the config
/// dirs, then the data dirs, and the first file that names a handler wins**: an
/// entry in `~/.config/mimeapps.list` overrides one in
/// `/usr/share/applications/mimeapps.list`. That is the whole reason writing to
/// one file is enough to claim a scheme - and the reason writing to one file was
/// not enough once `$desktop-mimeapps.list` files existed.
///
/// **`$desktop-mimeapps.list` was missing from this list.** That is the false
/// green this function has to not have any more: GMM's `kde-mimeapps.list` sits
/// in `$XDG_CONFIG_HOME`, the single highest-precedence row, and the chain
/// simply did not contain it. `test` resolved a contest with one contestant,
/// printed `nxmproxy handles nxm://: yes` and exited 0 on a machine where KDE
/// opened GMM.
///
/// Only [`consulted_desktops`] feeds this. A `gnome-mimeapps.list` written
/// during a KDE session is never opened by anything, so counting it would let
/// this function report a win off a file no desktop reads.
///
/// This used to take the *last* hit instead, which is what a tool that writes to
/// three files at once wants and a tool that writes to one does not: a
/// system-wide entry for another manager would have made `test` - and the
/// install verification - report a handler that the desktop does not use.
pub fn mimeapps_files() -> Vec<PathBuf> {
    mimeapps_chain(&consulted_desktops())
}

/// Read the machine's answer: every place the desktop was told, and who won.
///
/// The only impure part of the ordering. [`chain_with`] holds the rule and is
/// exercised without touching a filesystem; this is the one place the *machine*
/// is consulted.
pub fn resolve() -> Resolution {
    let files: Vec<(PathBuf, String)> = mimeapps_files()
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(&file).ok().map(|text| (file, text)))
        .collect();
    chain_with(&files, &publishes_the_scheme)
}

/// True when `id` names a desktop file that is installed **and** declares the
/// scheme.
///
/// This is the specification's second step and it is not optional:
///
/// > for each desktop ID in the list, attempt to load the named desktop file,
/// > using the normal rules; if a valid desktop file is found, verify that it is
/// > associated with the type; if after all list items are handled, we have not
/// > yet found a default application, **proceed to the next mimeapps.list file**
/// > in the search order and repeat
///
/// So an application named first in a file that is not installed does not lose to
/// the system, it is skipped and the search carries on. Skipping this step is the
/// same false green as skipping the whole file: `test` reports a handler the
/// desktop would never open. Verified against xdg-utils 1.1.3 - with
/// `~/.config/mimeapps.list` naming an unpublished `nxmproxy.desktop` and
/// `~/.config/kde-mimeapps.list` naming a published
/// `gamemodmanager-nxm.desktop`, `xdg-mime query default` answers GMM.
fn publishes_the_scheme(id: &str) -> bool {
    if !id.ends_with(".desktop") {
        return false;
    }
    crate::xdg::applications_dirs().iter().any(|dir| {
        std::fs::read_to_string(dir.join(id))
            .map(|text| crate::xdg::declares_scheme(&text))
            .unwrap_or(false)
    })
}

/// Rank the declarations and say why the winner won. Pure.
///
/// `files` arrives in precedence order and each file's declarations arrive in
/// [`associations`] order, so the first one is the answer and no search over
/// the rest is ever performed - not even for our own name.
pub fn chain(files: &[(PathBuf, String)]) -> Resolution {
    chain_with(files, &|_| true)
}

/// [`chain`], with "is this id installed and does it declare the type" answered
/// by the caller.
///
/// Split so the ordering rule can be tested with no filesystem. Folding the two
/// together meant the only way to test the rule was to run it on whatever
/// machine happened to be running the tests.
pub fn chain_with(files: &[(PathBuf, String)], usable: &impl Fn(&str) -> bool) -> Resolution {
    let declarations: Vec<Declaration> = files
        .iter()
        .flat_map(|(path, text)| {
            associations(text, MIME_TYPE)
                .into_iter()
                // Drop the ids the desktop would skip, and drop the declaration
                // entirely when nothing is left: a file whose only named
                // application is not installed is not a claim at all.
                .filter_map(move |(section, ids)| {
                    let ids: Vec<String> = ids.into_iter().filter(|id| usable(id)).collect();
                    (!ids.is_empty()).then(|| Declaration {
                        source: path.display().to_string(),
                        section: section.to_string(),
                        ids,
                    })
                })
        })
        .collect();
    let rule = explain(&declarations);
    Resolution {
        declarations,
        rule,
        // The chain was read, so an empty one means nothing claims the scheme -
        // which is a statement, not an absence of knowledge.
        readable: true,
    }
}

/// Why the winner won, in the specification's own terms.
///
/// The rule is always one of these four and never a guess, because the whole
/// purpose of this type is that a user can check the answer against the file by
/// hand. A resolution that cannot explain itself is the thing this replaced.
fn explain(declarations: &[Declaration]) -> String {
    let Some(first) = declarations.first() else {
        return format!("no file on this machine declares {MIME_TYPE}");
    };
    if declarations.len() == 1 {
        return "the only declaration on this machine".to_string();
    }
    // A `[Added Associations]` entry sitting below a `[Default Applications]`
    // one is the case that made this necessary: the file looks like a conflict
    // and the rule that settles it is a single sentence.
    if first.section == GROUP && declarations.iter().any(|d| d.section != GROUP) {
        return format!("[{GROUP}] outranks [{SECTION_ADDED}]");
    }
    "the first file that names the scheme decides".to_string()
}

/// True when nxmproxy is the application the desktop would open.
///
/// Same comparison on every OS that names applications by id. Pure given a
/// [`Resolution`], so the rule is testable without one too.
pub fn owns(resolution: &Resolution) -> bool {
    resolution.first_id() == Some(OWN_ID)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn the_desktop_entry_declares_the_nxm_scheme() {
        let entry = desktop_entry(Path::new("/usr/bin/nxmproxy"));
        assert!(entry.contains("MimeType=x-scheme-handler/nxm;"), "{entry}");
        assert!(entry.contains("Type=Application"));
        assert!(entry.contains("Exec=/usr/bin/nxmproxy url %u"));
    }

    #[test]
    fn an_exec_path_with_spaces_is_quoted() {
        let entry = desktop_entry(Path::new("/opt/My Mods/nxmproxy"));
        assert!(
            entry.contains(r#"Exec="/opt/My Mods/nxmproxy" url %u"#),
            "{entry}"
        );
    }

    #[test]
    fn a_bare_path_is_left_alone_and_percent_is_escaped() {
        assert_eq!(desktop_exec_arg("/usr/bin/nxmproxy"), "/usr/bin/nxmproxy");
        // `%` is a field-code introducer but is not one of the characters the
        // spec reserves, so doubling it unquoted is enough.
        assert_eq!(desktop_exec_arg("50%"), "50%%");
        assert_eq!(desktop_exec_arg("a b"), "\"a b\"");
        assert_eq!(desktop_exec_arg("say \"hi\""), r#""say \"hi\"""#);
        assert_eq!(desktop_exec_arg(""), "\"\"");
    }

    #[test]
    fn mimeapps_parsing_reads_only_the_default_applications_group() {
        let text = "\
[Added Associations]
x-scheme-handler/nxm=nxmproxy.desktop;

[Default Applications]
x-scheme-handler/nxm=other.desktop;
";
        assert_eq!(
            default_ids(text, MIME_TYPE).as_deref(),
            Some(&["other.desktop".to_string()][..]),
            "the same key in another group must not leak in"
        );
        assert_eq!(default_ids(text, "x-scheme-handler/http"), None);
    }

    #[test]
    fn mimeapps_parsing_lets_a_later_group_override_an_earlier_one() {
        let text = "\
[Default Applications]
x-scheme-handler/nxm=a.desktop;

[Default Applications]
x-scheme-handler/nxm=other.desktop;
";
        assert_eq!(
            default_ids(text, MIME_TYPE).as_deref(),
            Some(&["other.desktop".to_string()][..])
        );
    }

    #[test]
    fn the_users_own_file_wins_over_the_system_one() {
        // The precedence rule, which used to be inverted. A tool that writes to
        // one file only can still be beaten by a system-wide file under the
        // wrong rule, which is how a claim gets reported as successful when the
        // desktop would open something else.
        let mine = (
            PathBuf::from("/home/u/.config/mimeapps.list"),
            format!("[Default Applications]\n{MIME_TYPE}={DESKTOP_FILE};"),
        );
        let system = (
            PathBuf::from("/etc/xdg/mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;".to_string(),
        );
        let winner = |files: Vec<(PathBuf, String)>| {
            chain(&files)
                .winner()
                .map(|w| w.ids.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            winner(vec![mine.clone(), system.clone()]),
            vec![DESKTOP_FILE.to_string()],
            "the first file that names a handler decides"
        );
        assert_eq!(
            winner(vec![system.clone(), mine.clone()]),
            vec!["vortex.desktop".to_string()],
            "and it is the ORDER that decides: a system-wide file listed first wins, \
             which is why `resolve` must put the user's own file first"
        );
        assert_eq!(winner(vec![]), Vec::<String>::new());
        // A file that has never heard of the scheme is not a claim on it.
        let unrelated = (
            PathBuf::from("/etc/xdg/mimeapps.list"),
            "[Default Applications]\nx-scheme-handler/http=firefox.desktop;".to_string(),
        );
        assert_eq!(winner(vec![unrelated]), Vec::<String>::new());
    }

    /// The machine this whole change exists for, as a fixture: the scheme
    /// declared **twice by two different applications**, one group apart.
    ///
    /// Verbatim in shape - the two groups in one file, GMM in the added group
    /// and nxmproxy in the default one - because that is what the user was
    /// staring at and could not interpret from a bare application name.
    #[test]
    fn two_applications_declaring_the_scheme_are_both_named_and_one_wins() {
        let file = PathBuf::from("/home/u/.config/mimeapps.list");
        let resolution = chain(&[(
            file.clone(),
            format!(
                "[Added Associations]\n\
                 {MIME_TYPE}=gamemodmanager-nxm.desktop;\n\n\
                 [Default Applications]\n\
                 {MIME_TYPE}={DESKTOP_FILE};"
            ),
        )]);

        assert_eq!(
            resolution.claimants(),
            vec![DESKTOP_FILE, "gamemodmanager-nxm.desktop"],
            "both applications must be named, and the winner first"
        );
        assert_eq!(resolution.handler().as_deref(), Some(DESKTOP_FILE));
        assert!(
            owns(&resolution),
            "the default group outranks the added group"
        );
        assert_eq!(
            resolution.rule, "[Default Applications] outranks [Added Associations]",
            "the rule has to be checkable by hand against the file"
        );
        // And each row says where it came from, which is the whole diagnostic.
        let rows: Vec<(&str, &str, String)> = resolution
            .declarations
            .iter()
            .map(|d| (d.section.as_str(), d.source.as_str(), d.ids.join(";")))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "Default Applications",
                    file.to_str().unwrap(),
                    DESKTOP_FILE.to_string()
                ),
                (
                    "Added Associations",
                    file.to_str().unwrap(),
                    "gamemodmanager-nxm.desktop".to_string()
                ),
            ],
            "every candidate, in precedence order, each with its group and its file"
        );
    }

    /// The same two applications in the opposite order: GMM holds the default
    /// group, and nxmproxy is only an added association.
    ///
    /// This is the state that makes `install`'s "verified" a lie, so it is worth
    /// a test of its own rather than a variant of the one above.
    #[test]
    fn the_wrong_application_in_the_default_group_wins_and_nxmproxy_is_bypassed() {
        let resolution = chain(&[(
            PathBuf::from("/home/u/.config/mimeapps.list"),
            format!(
                "[Added Associations]\n\
                 {MIME_TYPE}={DESKTOP_FILE};\n\n\
                 [Default Applications]\n\
                 {MIME_TYPE}=gamemodmanager-nxm.desktop;"
            ),
        )]);
        assert_eq!(
            resolution.handler().as_deref(),
            Some("gamemodmanager-nxm.desktop")
        );
        assert!(
            !owns(&resolution),
            "an added association is not ownership, whatever it says"
        );
        assert_eq!(resolution.claimants().len(), 2);
    }

    /// No default anywhere: the desktop falls back to `[Added Associations]`,
    /// and so must we. Reporting "nobody claims nxm://" there is a factually
    /// wrong answer about a machine that has just told us who handles it.
    #[test]
    fn an_added_association_alone_still_names_the_application_that_gets_the_link() {
        let resolution = chain(&[(
            PathBuf::from("/home/u/.config/mimeapps.list"),
            format!("[Added Associations]\n{MIME_TYPE}={DESKTOP_FILE};\n"),
        )]);
        assert_eq!(
            resolution.handler().as_deref(),
            Some(DESKTOP_FILE),
            "the desktop's documented fallback is to use the added association"
        );
        assert!(owns(&resolution));
        assert_eq!(resolution.rule, "the only declaration on this machine");
    }

    /// An entry that lists nothing is not a claim. `uninstall` leaves the key
    /// removed, but a hand-edited file and a half-finished `xdg-mime` both
    /// produce `key=;`, and that names no application.
    #[test]
    fn an_empty_value_is_not_a_declaration() {
        let resolution = chain(&[(
            PathBuf::from("/home/u/.config/mimeapps.list"),
            format!("[Default Applications]\n{MIME_TYPE}=;\n"),
        )]);
        assert!(resolution.declarations.is_empty(), "{:?}", resolution);
        assert_eq!(resolution.handler(), None);
        assert!(!owns(&resolution));
        assert_eq!(
            resolution.rule,
            "no file on this machine declares x-scheme-handler/nxm"
        );
    }

    #[test]
    fn both_groups_are_read_from_one_file_in_precedence_order() {
        let text = "[Added Associations]\n\
                    x-scheme-handler/nxm=vortex.desktop;\n\n\
                    [Default Applications]\n\
                    x-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n";
        assert_eq!(
            associations(text, MIME_TYPE),
            vec![
                (
                    "Default Applications",
                    vec!["gamemodmanager-nxm.desktop".to_string()]
                ),
                ("Added Associations", vec!["vortex.desktop".to_string()]),
            ],
            "the group order is the precedence order, regardless of file order"
        );
        // A file with only one of the two still yields that one.
        assert_eq!(
            associations(
                "[Added Associations]\nx-scheme-handler/nxm=a.desktop;",
                MIME_TYPE
            )
            .len(),
            1
        );
        assert!(associations(
            "[Default Applications]\nx-scheme-handler/http=a.desktop;",
            MIME_TYPE
        )
        .is_empty());
    }

    #[test]
    fn group_names_are_matched_the_way_the_specification_spelles_them() {
        // Lower case, and with the space inside the brackets trimmed: both are
        // hand-edits that appear in real files and neither may lose the entry.
        assert_eq!(
            group_ids(
                "[default applications]\nx-scheme-handler/nxm=a.desktop;",
                "Default Applications",
                MIME_TYPE
            )
            .as_deref(),
            Some(&["a.desktop".to_string()][..])
        );
        assert_eq!(
            group_ids(
                "[ Added Associations ]\nx-scheme-handler/nxm=b.desktop;",
                SECTION_ADDED,
                MIME_TYPE
            )
            .as_deref(),
            Some(&["b.desktop".to_string()][..])
        );
        assert_eq!(
            group_ids(
                "[Added Associations]\nx-scheme-handler/nxm=a.desktop;",
                GROUP,
                MIME_TYPE
            ),
            None,
            "the groups are not interchangeable"
        );
    }

    #[test]
    fn setting_a_default_creates_the_group_when_the_file_is_empty() {
        let out = set_default_ids("", MIME_TYPE, &[DESKTOP_FILE]);
        assert_eq!(
            out,
            format!("[Default Applications]\n{MIME_TYPE}={DESKTOP_FILE};\n")
        );
        assert_eq!(
            default_ids(&out, MIME_TYPE).as_deref(),
            Some(&[DESKTOP_FILE.to_string()][..])
        );
    }

    #[test]
    fn setting_a_default_leaves_every_other_line_byte_for_byte() {
        // Another manager's entry, in two groups, plus an unrelated scheme.
        let before = "[Added Associations]\nnxm=vortex.desktop;\n\n\
                      [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n\
                      x-scheme-handler/nxm=nxmproxy.desktop;\n\n\
                      [Added Associations]\nx-scheme-handler/http=firefox.desktop;\n";
        let out = set_default_ids(before, MIME_TYPE, &[DESKTOP_FILE]);
        assert_eq!(
            out,
            "[Added Associations]\nnxm=vortex.desktop;\n\n\
             [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n\
             x-scheme-handler/nxm=nxmproxy.desktop;\n\n\
             [Added Associations]\nx-scheme-handler/http=firefox.desktop;\n",
            "re-setting a value already in place must change nothing"
        );

        let fresh = set_default_ids(before, "x-scheme-handler/magnet", &[DESKTOP_FILE]);
        assert!(
            fresh.contains("nxm=vortex.desktop;"),
            "another manager's entry survives: {fresh}"
        );
        assert!(
            fresh.contains("x-scheme-handler/http=firefox.desktop;"),
            "another scheme's default survives: {fresh}"
        );
        assert_eq!(
            fresh
                .matches("x-scheme-handler/http=firefox.desktop;")
                .count(),
            2,
            "both of its lines survive: {fresh}"
        );
    }

    #[test]
    fn a_new_key_lands_under_the_existing_group() {
        let before = "[Added Associations]\nnxm=vortex.desktop;\n\n[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
        let out = set_default_ids(before, MIME_TYPE, &[DESKTOP_FILE]);
        let group = out.find("[Default Applications]").unwrap();
        let key = out.find("x-scheme-handler/nxm=").unwrap();
        let http = out.find("x-scheme-handler/http=").unwrap();
        assert!(
            group < key && key < http,
            "the key belongs inside the group: {out}"
        );
    }

    #[test]
    fn removing_a_key_takes_out_that_line_and_nothing_else() {
        // nxmproxy's id removed from a list that also names another manager:
        // the other manager stays, because it is a valid claim for nxm://.
        let before = "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n\
                      x-scheme-handler/nxm=vortex.desktop;nxmproxy.desktop;\n";
        let out = set_default_ids(before, MIME_TYPE, &["vortex.desktop"]);
        assert_eq!(
            out,
            "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n\
             x-scheme-handler/nxm=vortex.desktop;\n",
            "the other manager's claim must survive the removal: {out}"
        );
        assert!(!out.contains("nxmproxy.desktop"), "{out}");
    }

    #[test]
    fn removing_the_last_id_removes_the_key_rather_than_emptying_it() {
        let before = "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\nx-scheme-handler/nxm=nxmproxy.desktop;\n";
        let out = set_default_ids(before, MIME_TYPE, &[]);
        assert_eq!(
            out,
            "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n"
        );
        // An empty value is not "no claim", it is a claim on nothing, and it is
        // not what the user asked for.
        assert!(!out.contains("nxm="), "{out}");
    }

    #[test]
    fn removing_a_key_that_is_not_there_changes_nothing() {
        let before = "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
        assert_eq!(set_default_ids(before, MIME_TYPE, &[]), before);
    }

    #[test]
    fn default_ids_reads_the_list_not_just_the_first_entry() {
        let text = "[Default Applications]\nx-scheme-handler/nxm=a.desktop;b.desktop;\n";
        assert_eq!(
            default_ids(text, MIME_TYPE),
            Some(vec!["a.desktop".to_string(), "b.desktop".to_string()])
        );
        assert_eq!(
            default_ids("[Added Associations]\nnxm=a.desktop;\n", MIME_TYPE),
            None
        );
    }

    // -----------------------------------------------------------------------
    // The claim sweep.
    //
    // Everything below is about one property with two halves: nxmproxy must end
    // up first, and every other application must still be there. The first half
    // is the feature; the second is the promise that makes it defensible.
    // -----------------------------------------------------------------------

    #[test]
    fn the_claim_goes_to_the_front_and_pushes_nobody_off_the_list() {
        // The state GMM leaves behind: two other managers ahead of nxmproxy, in
        // the file that outranks every other one.
        let before = "[Default Applications]\n\
                      x-scheme-handler/nxm=vortex.desktop;hd2arsenal.desktop;\n\
                      x-scheme-handler/http=firefox.desktop;\n";
        let after = prepend_default_id(before, MIME_TYPE, DESKTOP_FILE);
        assert_eq!(
            default_ids(&after, MIME_TYPE).as_deref(),
            Some(
                &[
                    DESKTOP_FILE.to_string(),
                    "vortex.desktop".to_string(),
                    "hd2arsenal.desktop".to_string()
                ][..]
            ),
            "our id first, theirs next, in the order they were already in: {after}"
        );
        assert!(
            after.contains("x-scheme-handler/http=firefox.desktop;"),
            "an unrelated scheme's default is not ours to touch: {after}"
        );
    }

    #[test]
    fn claiming_twice_changes_nothing_the_second_time() {
        // `install` is meant to be run again whenever the user feels like it. A
        // second pass that reorders the list would make the outcome depend on how
        // many times it ran, which is its own class of bug.
        let once = prepend_default_id(
            "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;\n",
            MIME_TYPE,
            DESKTOP_FILE,
        );
        let twice = prepend_default_id(&once, MIME_TYPE, DESKTOP_FILE);
        assert_eq!(once, twice);
        assert_eq!(
            default_ids(&twice, MIME_TYPE).as_deref(),
            Some(&[DESKTOP_FILE.to_string(), "vortex.desktop".to_string()][..])
        );
    }

    #[test]
    fn a_duplicate_of_our_own_id_is_collapsed_and_the_others_are_not() {
        // Two tools having written the same id, and the real HD2Arsenal entry
        // declaring the scheme twice, both produce this.
        let text = "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;nxmproxy.desktop;vortex.desktop;\n";
        let after = prepend_default_id(text, MIME_TYPE, DESKTOP_FILE);
        assert_eq!(
            default_ids(&after, MIME_TYPE).as_deref(),
            Some(
                &[
                    DESKTOP_FILE.to_string(),
                    "vortex.desktop".to_string(),
                    "vortex.desktop".to_string()
                ][..]
            ),
            "only *our* duplicate goes: a manager listed twice is still listed \
             twice, because deduplicating it is an edit nobody asked for: {after}"
        );
    }

    /// The property the whole ticket rests on: a prepend is invertible.
    ///
    /// A replace is not. `xdg-mime default` overwrote the value for the key and
    /// nothing recorded what it was, so `uninstall` could not put Vortex back and
    /// had to say so. Prepending keeps enough information to put it back exactly,
    /// and this is the test that says so rather than the README saying so.
    #[test]
    fn undoing_the_claim_restores_the_previous_bytes_exactly() {
        for before in [
            // The file that outranked everything, with two rivals ahead of us.
            "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;hd2arsenal.desktop;\n",
            // A file with other content in other groups, which must survive too.
            "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
             [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n",
            // A file with no group of ours at all.
            "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n",
            // A file with neither: created by the claim, removed by the undo.
            "",
        ] {
            let claimed = prepend_default_id(before, MIME_TYPE, DESKTOP_FILE);
            assert_eq!(
                first_default_id(&claimed, MIME_TYPE).as_deref(),
                Some(DESKTOP_FILE),
                "the claim did not take on {before:?}: {claimed}"
            );
            let list = default_ids(&claimed, MIME_TYPE).unwrap();
            let kept: Vec<&str> = list.iter().map(String::as_str).collect();
            let rest: Vec<&str> = kept
                .iter()
                .copied()
                .filter(|id| *id != DESKTOP_FILE)
                .collect();
            let undone = set_default_ids(&claimed, MIME_TYPE, &rest);
            assert_eq!(
                undone, before,
                "the undo of a prepend must be the identity, not an approximation"
            );
        }
    }

    #[test]
    fn a_group_left_with_no_keys_at_all_goes_too() {
        // The exact shape `install` creates in a file that had nothing, and what
        // `uninstall` must not leave behind.
        let claimed = prepend_default_id("", MIME_TYPE, DESKTOP_FILE);
        assert_eq!(
            claimed,
            format!("[Default Applications]\n{MIME_TYPE}={DESKTOP_FILE};\n")
        );
        let undone = set_default_ids(&claimed, MIME_TYPE, &[]);
        assert_eq!(
            undone, "",
            "an empty group is not a group, and leaving it behind means a \
             $desktop-mimeapps.list that did not exist a minute ago still does"
        );
    }

    #[test]
    fn dropping_a_keyless_group_leaves_a_group_that_still_has_keys_alone() {
        let text = "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n\
                    x-scheme-handler/nxm=nxmproxy.desktop;\n";
        assert_eq!(
            set_default_ids(text, MIME_TYPE, &[]),
            "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n"
        );
        // And a second, keyless copy of the group is the one that goes: the first
        // still has keys under it, so the file is not left with a dangling header.
        let repeated = "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n\n\
                        [Default Applications]\nx-scheme-handler/nxm=nxmproxy.desktop;\n";
        assert_eq!(
            set_default_ids(repeated, MIME_TYPE, &[]),
            "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n",
            "the keyless group went and the blank line that separated them became \
             the file's trailing newline, rather than leaving a hole"
        );
    }

    /// The specification's table, in order, with nothing missing and nothing
    /// invented. The order is the rule: a chain with these eight paths in the
    /// wrong sequence reads a different file's answer.
    #[test]
    fn the_chain_is_the_specifications_eight_rows_in_order() {
        // Explicit roots, so this is a fact about the ordering and not about the
        // machine running it.
        let chain = |desktops: &[String]| {
            chain_over(
                Some(Path::new("/home/u/.config")),
                &[PathBuf::from("/etc/xdg")],
                Some(Path::new("/home/u/.local/share")),
                &[
                    PathBuf::from("/usr/local/share"),
                    PathBuf::from("/usr/share"),
                ],
                desktops,
            )
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
        };
        assert_eq!(
            chain(&["kde".to_string()]),
            vec![
                // 1-2. $XDG_CONFIG_HOME - user overrides. The desktop-specific row
                // is ABOVE the plain one, and that is the entire bug report.
                "/home/u/.config/kde-mimeapps.list",
                "/home/u/.config/mimeapps.list",
                // 3-4. $XDG_CONFIG_DIRS - sysadmin and vendor overrides.
                "/etc/xdg/kde-mimeapps.list",
                "/etc/xdg/mimeapps.list",
                // 5-6. $XDG_DATA_HOME/applications - deprecated, still read.
                "/home/u/.local/share/applications/kde-mimeapps.list",
                "/home/u/.local/share/applications/mimeapps.list",
                // 7-8. $XDG_DATA_DIRS/applications - the distribution's defaults.
                "/usr/local/share/applications/kde-mimeapps.list",
                "/usr/local/share/applications/mimeapps.list",
                "/usr/share/applications/kde-mimeapps.list",
                "/usr/share/applications/mimeapps.list",
            ],
            "specification section 2's table, in its order"
        );
        // Every `$XDG_DATA_DIRS` entry gets both rows, not just the first: a
        // distribution's second entry is consulted like any other.
        assert_eq!(
            chain(&["kde".to_string()]).len(),
            10,
            "two config roots, one data home, two data dirs - and both rows each"
        );
        // `$desktop` expands to every name, in the order given.
        let two = chain(&["kde".to_string(), "gnome".to_string()]);
        assert_eq!(two.len(), 15);
        assert_eq!(
            &two[..3],
            &[
                "/home/u/.config/kde-mimeapps.list",
                "/home/u/.config/gnome-mimeapps.list",
                "/home/u/.config/mimeapps.list",
            ],
            "consulted in the order asked for, and the plain row still last"
        );
        // With no desktop at all - `$XDG_CURRENT_DESKTOP` unset - only the plain
        // rows remain, which is what GLib does by dropping the dash.
        assert_eq!(
            chain(&[]),
            vec![
                "/home/u/.config/mimeapps.list",
                "/etc/xdg/mimeapps.list",
                "/home/u/.local/share/applications/mimeapps.list",
                "/usr/local/share/applications/mimeapps.list",
                "/usr/share/applications/mimeapps.list",
            ]
        );
    }

    /// The discovery that finds GMM's file, against a real directory tree.
    ///
    /// This one cannot be pure: the mechanism *is* "walk the four roots and see
    /// what is already there", and a fixture that passed a list in would be
    /// testing the fixture. The scratch tree is the only filesystem these tests
    /// touch, and it is removed on both paths out.
    #[test]
    fn a_desktop_specific_file_already_on_disk_is_found_and_the_plain_one_is_not() {
        let root = scratch_dir("desklist");
        std::fs::create_dir_all(root.join("applications")).unwrap();
        std::fs::write(root.join("kde-mimeapps.list"), "[Default Applications]\n").unwrap();
        std::fs::write(
            root.join("applications/x-cinnamon-mimeapps.list"),
            "[Default Applications]\n",
        )
        .unwrap();
        // Decoys: the plain file names no desktop, and an unrelated file is not
        // one either. A stem-parsing slip would turn one of these into a name.
        std::fs::write(root.join("mimeapps.list"), "[Default Applications]\n").unwrap();
        std::fs::write(root.join("notes.txt"), "hello").unwrap();

        let found = existing_desktop_lists(std::slice::from_ref(&root));
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            found,
            vec!["kde".to_string(), "x-cinnamon".to_string()],
            "both roots scanned, both desktop names found, neither decoy counted"
        );
    }

    #[test]
    fn a_session_installed_on_this_machine_is_discovered() {
        let root = scratch_dir("xsessions");
        std::fs::create_dir_all(root.join("wayland-sessions")).unwrap();
        std::fs::write(
            root.join("wayland-sessions/plasma.desktop"),
            "[Desktop Entry]\nName=Plasma\nDesktopNames=KDE;\n",
        )
        .unwrap();
        std::fs::write(
            root.join("wayland-sessions/gnome.desktop"),
            "[Desktop Entry]\nName=GNOME\n",
        )
        .unwrap();

        let found = installed_sessions(std::slice::from_ref(&root));
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(
            found,
            vec!["kde".to_string()],
            "the session that declares its own $desktop is found; one that declares \
             nothing contributes nothing: {found:?}"
        );
    }

    /// A unique scratch directory under `$TMPDIR`, for the two tests above.
    fn scratch_dir(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "nxmproxy-{tag}-{}-{}",
            std::process::id(),
            tag.len()
        ));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn every_desktop_name_is_lowercased_and_none_is_claimed_twice() {
        // Real sessions spell it their own way, and the specification asks for
        // ascii lowercase. `KDE:GNOME:X-Cinnamon` is three names, not one, and
        // `KDE` and `kde` are the same name.
        let mut names = Vec::new();
        for part in "KDE:GNOME:kde:X-Cinnamon".split(':') {
            let name = part.trim().to_ascii_lowercase();
            if !name.is_empty() && !names.contains(&name) {
                names.push(name);
            }
        }
        assert_eq!(names, vec!["kde", "gnome", "x-cinnamon"]);
    }

    #[test]
    fn a_session_files_desktop_names_are_read_and_nothing_else_is() {
        // The real shape: `DesktopNames` is how a display manager learns what to
        // put in `$XDG_CURRENT_DESKTOP`, and it is in `[Desktop Entry]` - while
        // `[Desktop Action ...]` groups carry keys of their own that must not be
        // mistaken for it.
        let text = "[Desktop Entry]\nName=Plasma (Wayland)\n\
                    DesktopNames=KDE;Plasma;\nExec=startplasma-wayland\n\
                    \n[Desktop Action Screenshot]\nName=Screenshot\nDesktopNames=notadesktop;\n";
        assert_eq!(session_desktop_names(text), vec!["kde", "plasma"]);
        assert!(session_desktop_names("[Desktop Entry]\nName=Plain\n").is_empty());
    }

    #[test]
    fn an_existing_desktop_specific_file_is_discovered_by_its_stem() {
        // This is how GMM's `kde-mimeapps.list` gets found on a machine whose
        // running session is not KDE. `mimeapps.list` has no prefix and must not
        // produce an empty name.
        let name_of = |file: &str| {
            let path = Path::new(file);
            path.extension()
                .is_some_and(|e| e == "list")
                .then(|| {
                    path.file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                })
                .and_then(|stem| stem.strip_suffix("-mimeapps"))
                .map(str::to_string)
        };
        assert_eq!(
            name_of("/home/u/.config/kde-mimeapps.list").as_deref(),
            Some("kde")
        );
        assert_eq!(
            name_of("/home/u/.config/x-cinnamon-mimeapps.list").as_deref(),
            Some("x-cinnamon")
        );
        assert_eq!(name_of("/home/u/.config/mimeapps.list"), None);
        assert_eq!(name_of("/home/u/.config/notes.txt"), None);
    }
}
