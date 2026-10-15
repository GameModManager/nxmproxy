// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Linux: register by publishing an XDG desktop entry, and nothing else.
//!
//! The whole mechanism is one file:
//!
//! ```text
//! ~/.local/share/applications/nxmproxy.desktop
//!     MimeType=x-scheme-handler/nxm;
//! ```
//!
//! That line is what puts nxmproxy in the list the desktop shows when an
//! `nxm://` link is opened. It registers; it does not decide.
//!
//! **What this deliberately does not do**, because doing it is the greed this
//! tool exists to fix: it never runs `xdg-mime default`, never writes a
//! `[Default Applications]` group into any `mimeapps.list`, and never
//! force-propagates into another file. GMM's own Linux registration does all
//! three, which is why a user with two mod managers finds one of them buried.
//! Here, opening an nxm link with no default configured makes the desktop ask.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::logger;

const SCHEME: &str = "nxm";
const DESKTOP_FILE: &str = "nxmproxy.desktop";

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

/// The `[Default Applications]` entry for a scheme in one `mimeapps.list`.
///
/// Pure, and read-only: used by `test` to report the truth. It is never used
/// to write anything.
pub fn default_in_mimeapps(text: &str, scheme: &str) -> Option<String> {
    let mut section = String::new();
    let mut found = None;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.trim_matches(['[', ']']).trim().to_ascii_lowercase();
            continue;
        }
        if section != "default applications" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(scheme) {
            // mimeapps.list values are a semicolon-terminated list of desktop
            // file ids. The trailing `;` is syntax, not part of the id.
            found = Some(value.trim().trim_end_matches(';').trim().to_string());
        }
    }
    found
}

/// The effective default handler, read from the `mimeapps.list` chain.
///
/// The specification's lookup order is config dirs then data dirs, and a later
/// file overrides an earlier one, so the last hit wins. Returns `None` when
/// nothing claims the scheme, which is the case this tool is happy to be in:
/// the desktop then asks the user.
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

    let mut result = None;
    for file in files {
        if let Ok(text) = std::fs::read_to_string(&file) {
            if let Some(found) = default_in_mimeapps(&text, SCHEME) {
                result = Some(found);
            }
        }
    }
    Ok(result)
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
nxm=nxmproxy.desktop;

[Default Applications]
nxm=vortex.desktop;
x-scheme-handler/nxm=other.desktop;
";
        assert_eq!(
            default_in_mimeapps(text, "nxm").as_deref(),
            Some("vortex.desktop")
        );
        // the same key elsewhere in the file must not leak in
        assert_eq!(
            default_in_mimeapps(text, "x-scheme-handler/nxm").as_deref(),
            Some("other.desktop")
        );
        assert_eq!(default_in_mimeapps(text, "http"), None);
    }

    #[test]
    fn mimeapps_parsing_lets_a_later_group_override_an_earlier_one() {
        let text =
            "[Default Applications]\nnxm=a.desktop;\n\n[Default Applications]\nnxm=b.desktop;\n";
        assert_eq!(
            default_in_mimeapps(text, "nxm").as_deref(),
            Some("b.desktop")
        );
    }
}
