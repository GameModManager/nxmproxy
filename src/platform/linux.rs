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
use crate::platform::Claim;

const SCHEME: &str = "nxm";
const DESKTOP_FILE: &str = "nxmproxy.desktop";
const MIME_TYPE: &str = "x-scheme-handler/nxm";
const GROUP: &str = "Default Applications";

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
    let handler = current_handler()?;
    let claim = if handler.as_deref() == Some(DESKTOP_FILE) {
        Claim::verified()
    } else {
        Claim::needs_user(format!(
            "{DESKTOP_FILE} is registered, but {} still says {:?} handles {MIME_TYPE}. \
             Open Settings > Applications > Default Applications and pick NXM Proxy there.",
            mimeapps_path()?.display(),
            handler.unwrap_or_else(|| "(nothing)".to_string())
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

/// The desktop file ids listed for `key` under `[Default Applications]`.
pub fn default_ids(text: &str, key: &str) -> Option<Vec<String>> {
    const GROUP_LOWER: &str = "default applications";
    let mut section = String::new();
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).trim().to_ascii_lowercase();
            continue;
        }
        if section != GROUP_LOWER {
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
///
/// The specification's lookup order is the user's config first, then the
/// config dirs, then the data dirs, and **the first file that names a handler
/// wins**: an entry in `~/.config/mimeapps.list` overrides one in
/// `/usr/share/applications/mimeapps.list`. That is the whole reason writing to
/// one file is enough to claim a scheme.
///
/// This used to take the *last* hit instead, which is what a tool that writes
/// to three files at once wants and a tool that writes to one does not: a
/// system-wide entry for another manager would have made `test` - and the
/// install verification - report a handler that the desktop does not use.
pub fn current_handler() -> Result<Option<String>, String> {
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

    let texts: Vec<String> = files
        .iter()
        .filter_map(|file| std::fs::read_to_string(file).ok())
        .collect();
    Ok(effective_handler(&texts).map(|list| list.join(";")))
}

/// The first file that names a handler decides, per the specification's
/// precedence. Pure, so the ordering rule is testable without a filesystem.
///
/// The key is the **MIME type**, not the bare scheme. A `mimeapps.list` is
/// keyed by MIME type, so `nxm=...` is a different key from
/// `x-scheme-handler/nxm=...` and never matches what `xdg-mime default` wrote.
/// Getting this wrong makes a successful install verify against a key nothing
/// ever sets, and report "not the handler" straight after claiming the scheme.
pub fn effective_handler(texts: &[String]) -> Option<Vec<String>> {
    texts.iter().find_map(|text| default_ids(text, MIME_TYPE))
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
        let mine = format!("[Default Applications]\n{MIME_TYPE}={DESKTOP_FILE};");
        let system = "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;";
        assert_eq!(
            effective_handler(&[mine.clone(), system.to_string()]).as_deref(),
            Some(&[DESKTOP_FILE.to_string()][..]),
            "the first file that names a handler decides"
        );
        assert_eq!(
            effective_handler(&[system.to_string(), mine.clone()]).as_deref(),
            Some(&["vortex.desktop".to_string()][..]),
            "and it is the ORDER that decides: a system-wide file listed first wins, \
             which is why `current_handler` must put the user's own file first"
        );
        assert_eq!(effective_handler(&[]), None);
        // A file that has never heard of the scheme is not a claim on it.
        let unrelated = "[Default Applications]\nx-scheme-handler/http=firefox.desktop;";
        assert_eq!(effective_handler(&[unrelated.to_string()]), None);
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
