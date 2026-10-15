// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Linux: publish an XDG desktop entry, then claim the scheme with `xdg-mime`.
//!
//! Two steps, both reversible, neither touching anything that is not ours:
//!
//! ```text
//! ~/.local/share/applications/nxmproxy.desktop
//!     MimeType=x-scheme-handler/nxm;
//! ~/.config/mimeapps.list
//!     [Default Applications]
//!     x-scheme-handler/nxm=nxmproxy.desktop;
//! ```
//!
//! The first file makes nxmproxy *eligible*. The second line makes it the one
//! the desktop opens. `xdg-mime default` writes that line and nothing else:
//! one file, one group, one key.
//!
//! **What this deliberately does not do**, because it is the greed GMM's own
//! Linux registration commits: it does not `set_mimeapps_defaults` across the
//! `~/.config`, `applications` and `/etc/xdg` files at once. That buries every
//! other manager's claim and overrides entries other tools wrote, and it is
//! unreversible without hand editing. Exactly one line, in exactly one file,
//! goes away again in `uninstall`.
//!
//! ## Why the edit is done here and not by `xdg-mime`
//!
//! Writing goes through `xdg-mime`, so the file is the one every other tool on
//! the system writes and the format is the tool's, not ours. **Removing** does
//! not: `xdg-mime` has no verb for it, so [`remove_default_ids`] rewrites the
//! one key, and only that key. That is why the editor below is a pure function
//! over text: "which line disappears" is a decision worth testing without a
//! desktop anywhere near it.

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
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("share")))
        .ok_or_else(|| "could not determine the XDG data directory".to_string())
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

/// Ask `xdg-mime` to make nxmproxy the default for `nxm://`.
///
/// The one call, and only that call. It writes
/// `$XDG_CONFIG_HOME/mimeapps.list` and touches no other file, which is what
/// makes this claim reversible; see the module docs for the pattern this
/// refuses to use.
pub fn claim() -> Result<Claim, String> {
    // `xdg-mime default` exits 0 and writes **nothing** when the config
    // directory does not exist, which it does not on a fresh account. Verified
    // against xdg-utils 1.1.3: no error, no file, exit 0. Create it first, or
    // `install` claims a scheme it never claimed.
    let dir = mimeapps_path()?;
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {}", parent.display(), e))?;
    }

    let outcome = Command::new("xdg-mime")
        .args(["default", DESKTOP_FILE, MIME_TYPE])
        .output();

    let fallback_note = match outcome {
        Ok(output) if output.status.success() => None,
        Ok(output) => {
            return Err(format!(
                "xdg-mime default failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
        Err(e) => {
            // No xdg-mime (a headless container, or xdg-utils not installed).
            // The claim is one line in one file; do it directly rather than
            // failing an install that would otherwise be complete.
            logger::info(format!(
                "xdg-mime not available ({e}); writing the entry directly"
            ));
            upsert_default(MIME_TYPE, &[DESKTOP_FILE])?;
            Some(format!(
                "xdg-mime was not on PATH ({e}), so the entry was written to {} directly. \
                 Install xdg-utils to use the desktop's own tool instead.",
                mimeapps_path()?.display()
            ))
        }
    };

    // Only report success if the file now says so. `xdg-mime` exits 0 when it
    // wrote the line, but the effective handler is a property of the whole
    // mimeapps chain, so read it back rather than trusting the exit code.
    //
    // Read back through [`resolve`] and judged by [`owns`], the same two steps
    // `test` and `list` take. Comparing the joined value against `DESKTOP_FILE`
    // instead - which is what this used to do - reports a *failed* claim on a
    // file that says `nxmproxy.desktop;vortex.desktop`, because nxmproxy is
    // first in that list and is therefore the handler. A claim refused over a
    // fallback we ourselves appended is the false negative of the false green
    // this module exists to end.
    let resolution = resolve();
    let claim = if owns(&resolution) {
        Claim::verified()
    } else {
        Claim::needs_user(format!(
            "{DESKTOP_FILE} is registered, but {} still says {} handles {MIME_TYPE}. \
             Open Settings > Applications > Default Applications and pick NXM Proxy there.",
            mimeapps_path()?.display(),
            resolution
                .handler()
                .unwrap_or_else(|| "(nothing)".to_string())
        ))
    };

    if let Some(note) = fallback_note {
        println!("note: {note}");
    }
    Ok(claim)
}

/// Give `nxm://` up. Removes our id from the `[Default Applications]` line and
/// leaves every other id on that line, and every other line, alone.
///
/// Not delegated to `xdg-mime`, which cannot do it. Windows and macOS are the
/// other half of this story and cannot do it either; see their `unclaim`.
pub fn unclaim() -> Result<Claim, String> {
    let path = mimeapps_path()?;
    let Ok(text) = std::fs::read_to_string(&path) else {
        // Nothing written means nothing to undo.
        return Ok(Claim::verified());
    };

    let remaining: Vec<String> = default_ids(&text, MIME_TYPE)
        .map(|list| list.into_iter().filter(|id| id != DESKTOP_FILE).collect())
        .unwrap_or_default();
    let wanted: Vec<&str> = remaining.iter().map(String::as_str).collect();
    let updated = set_default_ids(&text, MIME_TYPE, &wanted);
    if updated != text {
        std::fs::write(&path, &updated)
            .map_err(|e| format!("could not write {}: {}", path.display(), e))?;
        logger::info(format!("removed nxmproxy.desktop from {}", path.display()));
    }

    Ok(Claim::needs_user(
        "nxm:// is unclaimed. Set it back with: nxmproxy install\n  \
         or pick your previous manager in Settings > Applications > Default Applications.",
    ))
}

/// The `mimeapps.list` this user owns, which is the one `xdg-mime` writes.
pub fn mimeapps_path() -> Result<PathBuf, String> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute());
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (config_home, home) {
        (Some(dir), _) => Ok(dir.join("mimeapps.list")),
        (None, Some(home)) => Ok(home.join(".config").join("mimeapps.list")),
        (None, None) => Err("could not determine the XDG config directory".to_string()),
    }
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

/// Set or remove one `[Default Applications]` key. Pure: the text in, the text
/// out, so the "we touch one line" claim is testable without a desktop.
///
/// Every other line is reproduced byte for byte, including lines in other
/// groups and other keys in this one. An empty `ids` removes the key entirely
/// rather than leaving `key=;` behind, which a desktop reads as "claims
/// nothing" and which is not the same as making no claim.
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
    }

    let mut result = out.join("\n");
    if !result.is_empty() && !result.ends_with('\n') {
        result.push('\n');
    }
    result
}

/// Set one key directly. Used only when `xdg-mime` is absent; see [`claim`].
fn upsert_default(key: &str, ids: &[&str]) -> Result<(), String> {
    let path = mimeapps_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("could not create {}: {}", parent.display(), e))?;
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    std::fs::write(&path, set_default_ids(&text, key, ids))
        .map_err(|e| format!("could not write {}: {}", path.display(), e))
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
/// The specification's lookup order is the user's config first, then the config
/// dirs, then the data dirs, and **the first file that names a handler wins**:
/// an entry in `~/.config/mimeapps.list` overrides one in
/// `/usr/share/applications/mimeapps.list`. That is the whole reason writing to
/// one file is enough to claim a scheme.
///
/// This used to take the *last* hit instead, which is what a tool that writes to
/// three files at once wants and a tool that writes to one does not: a
/// system-wide entry for another manager would have made `test` - and the
/// install verification - report a handler that the desktop does not use.
pub fn mimeapps_files() -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        files.push(config_home.join("mimeapps.list"));
    }
    for dir in std::env::var_os("XDG_CONFIG_DIRS")
        .map(|d| std::env::split_paths(&d).collect::<Vec<_>>())
        .unwrap_or_else(|| vec![PathBuf::from("/etc/xdg")])
    {
        files.push(dir.join("mimeapps.list"));
    }
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        files.push(data_home.join("applications").join("mimeapps.list"));
    }
    for dir in std::env::var_os("XDG_DATA_DIRS")
        .map(|d| std::env::split_paths(&d).collect::<Vec<_>>())
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share"),
                PathBuf::from("/usr/share"),
            ]
        })
    {
        files.push(dir.join("applications").join("mimeapps.list"));
    }
    files
}

/// Read the machine's answer: every place the desktop was told, and who won.
///
/// The only impure part of the ordering. [`chain`] holds the rule and is
/// exercised without touching a filesystem.
pub fn resolve() -> Resolution {
    let files: Vec<(PathBuf, String)> = mimeapps_files()
        .into_iter()
        .filter_map(|file| std::fs::read_to_string(&file).ok().map(|text| (file, text)))
        .collect();
    chain(&files)
}

/// Rank the declarations and say why the winner won. Pure.
///
/// `files` arrives in precedence order and each file's declarations arrive in
/// [`associations`] order, so the first one is the answer and no search over
/// the rest is ever performed - not even for our own name.
pub fn chain(files: &[(PathBuf, String)]) -> Resolution {
    let declarations: Vec<Declaration> = files
        .iter()
        .flat_map(|(path, text)| {
            associations(text, MIME_TYPE)
                .into_iter()
                .map(move |(section, ids)| Declaration {
                    source: path.display().to_string(),
                    section: section.to_string(),
                    ids,
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
}
