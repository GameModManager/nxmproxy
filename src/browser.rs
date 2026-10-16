// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! The last thing between a clicked link and nxmproxy: the browser.
//!
//! `test` answers "who does the OS say handles `nxm://`". It cannot answer
//! "who opens it when you click Download on a web page", and on Linux those are
//! frequently not the same application. Firefox keeps its own database of
//! per-scheme handlers and consults it **before** the OS:
//!
//! ```text
//! GetProtocolHandlerInfo(scheme):
//!     GetProtocolHandlerInfoFromOS(...)       // what mimeapps.list says
//!     handlerSvc.Exists(...) -> FillHandlerInfo(...) and RETURN
//!     SetProtocolHandlerDefaults(...)         // only if Firefox has no entry
//! ```
//!
//! (`uriloader/exthandler/nsExternalHelperAppService.cpp`.) So once `nxm` has an
//! entry in a profile's `handlers.json`, no change to `mimeapps.list` will ever
//! reach that profile again and a link clicked in Firefox arrives nowhere.
//! That is the state `xdg-mime` cannot fix and `install` cannot verify, because
//! both of them only ever speak to the OS.
//!
//! ## What this module may and may not do
//!
//! **Read only.** A browser's profile is its own store, in its own format,
//! under its own lifecycle, and nothing here writes to it or to any other
//! application's configuration. The best that can be done is name the file the
//! user has to open and say what is in it; the edit is theirs.
//!
//! ## Why only Gecko
//!
//! Because a Gecko browser is the only one of the three families that keeps an
//! **application** per scheme.
//!
//! * **Firefox and its forks** - `handlers.json` plus the
//!   `network.protocol-handler.*` preferences in `prefs.js`. Read below.
//! * **Chromium** - has no such list. The only thing it persists is
//!   `protocol_handler.allowed_origin_protocol_pairs` in the profile
//!   `Preferences`, which records that a *site* may launch a *scheme* without a
//!   prompt and names no application at all
//!   (`chrome/browser/external_protocol/external_protocol_handler.cc`,
//!   `chrome/common/pref_names.h`). The launch itself always goes back out to
//!   the OS - `org.freedesktop.portal.OpenURI` when the portal is running,
//!   `xdg-open` when it is not (`chrome/browser/platform_util_linux.cc`) - so a
//!   Chromium-family browser cannot be the reason `xdg-mime` was ignored, and
//!   rummaging through its profile to "check" would only manufacture a finding
//!   that is not there.
//! * **WebKitGTK** - delegates to GIO and keeps no list of its own.
//!
//! Every Gecko browser is covered by scanning `$HOME/.var/app/*` rather than by
//! listing application ids, so a distribution or a fork this file has never
//! heard of is still found.

use std::path::{Path, PathBuf};

/// The scheme, as a browser writes it in its own configuration.
const SCHEME: &str = "nxm";

/// One browser that decides `nxm://` for itself instead of asking the OS.
///
/// A finding, never an edit. See the module docs for the read-only boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Override {
    /// Which browser, by the name a user would search for.
    pub browser: String,
    /// What it does instead, in its own vocabulary.
    pub says: String,
    /// The file to open to undo it.
    pub source: PathBuf,
}

/// Every Gecko browser on this machine that has its own answer for `nxm://`.
///
/// `home` is a parameter rather than read from the environment so the caller
/// decides whose home that is. It is the same `$HOME` the desktop resolves
/// against, and passing it explicitly is what lets a sandboxed test point at
/// its own tree and see nothing.
pub fn overrides(home: &Path) -> Vec<Override> {
    let mut found = Vec::new();
    for (browser, root) in roots(home) {
        let Ok(profiles) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = profiles
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        // Stable order, so two profiles disagreeing produce the same report on
        // every run rather than whichever the filesystem listed first.
        entries.sort();
        for profile in entries {
            if let Some(over) = override_in(&browser, &profile) {
                found.push(over);
            }
        }
    }
    found
}

/// The one thing this module is for: a browser, a finding, a file.
///
/// A file that is absent, empty or unparseable is not a finding: it is a
/// browser that has never been told anything, which is the ordinary state of a
/// profile that has not opened a link yet.
fn override_in(browser: &str, profile: &Path) -> Option<Override> {
    let handlers = profile.join("handlers.json");
    let prefs = profile.join("prefs.js");
    let handlers_text = std::fs::read_to_string(&handlers).unwrap_or_default();
    let prefs_text = std::fs::read_to_string(&prefs).unwrap_or_default();

    let (says, file) = gecko_says(&handlers_text, &prefs_text)?;
    Some(Override {
        browser: browser.to_string(),
        says,
        source: profile.join(file),
    })
}

/// Every directory a Gecko profile may live in, as `(browser name, root)`.
///
/// Three layouts, all of them real: a build installed for the user, a Flatpak
/// whose home is relocated, and a snap whose common data is shared.
fn roots(home: &Path) -> Vec<(String, PathBuf)> {
    // A root that does not exist costs one failed `read_dir` in `overrides`,
    // so these are not filtered here.
    let mut out = vec![("Firefox".to_string(), home.join(".mozilla").join("firefox"))];
    // ponytail: profiles are found by globbing `<root>/*` rather than by reading
    // `profiles.ini`, so a profile whose `IsRelative=0` puts it outside its own
    // root is missed. Read `profiles.ini` and follow `Path=` when one machine
    // turns out to have one.
    if let Ok(apps) = std::fs::read_dir(home.join(".var").join("app")) {
        for app in apps.filter_map(Result::ok) {
            let path = app.path();
            let profile_root = path.join(".mozilla").join("firefox");
            if profile_root.is_dir() {
                let id = path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "Firefox".to_string());
                out.push((id, profile_root));
            }
        }
    }
    out.push((
        "Firefox (snap)".to_string(),
        home.join("snap")
            .join("firefox")
            .join("common")
            .join(".mozilla")
            .join("firefox"),
    ));
    out
}

/// What a Gecko profile's own two files say should handle `nxm`, and which file
/// said it. `None` for the ordinary case.
///
/// Pure, so every shape is testable with no browser installed. `None` covers
/// both "this profile has an `nxm` entry with `useSystemDefault`" and "this
/// profile has no `nxm` entry at all", because both mean Firefox will go to
/// the OS - and a finding printed for either would be a claim about a machine
/// that is not broken.
///
/// **The order is a reporting choice, not Firefox's precedence.** A profile with
/// more than one of these set gets one of them, and the one naming an
/// *application* wins: "which application is deciding" is the question this
/// exists to answer, and an application is the answer. `expose` and `external`
/// come last because they explain a link that reaches nothing at all rather than
/// one that reaches somewhere else.
pub fn gecko_says(handlers: &str, prefs: &str) -> Option<(String, &'static str)> {
    // `schemes.nxm.action == "useHelperApp"` is Firefox keeping its own choice
    // and not asking again. Navigated key by key rather than deserialised into a
    // struct because Mozilla calls this "the internal format of handlers.json",
    // and a fixed struct is a claim about a file that is allowed to change.
    let remembered = serde_json::from_str::<serde_json::Value>(handlers).ok();
    let entry = remembered
        .as_ref()
        .and_then(|json| json.get("schemes"))
        .and_then(|json| json.get(SCHEME));
    if entry.and_then(|e| e.get("action")).and_then(|a| a.as_str()) == Some("useHelperApp") {
        let path = entry
            .and_then(|e| e.get("handlers"))
            .and_then(|h| h.get(0))
            .and_then(|h| h.get("path"))
            .and_then(|p| p.as_str());
        // A remembered action with no readable path is still an override: the
        // profile is set to use a helper application and the OS is out of the
        // loop, whatever the name of it is.
        let says = path.unwrap_or("(a helper application Firefox chose)");
        return Some((says.to_string(), "handlers.json"));
    }

    let app = format!("network.protocol-handler.app.{SCHEME}");
    if let Some(value) = pref(prefs, &app) {
        return Some((format!("{app} = {value:?}"), "prefs.js"));
    }

    let expose = format!("network.protocol-handler.expose.{SCHEME}");
    if pref_is_false(prefs, &expose) {
        return Some((
            format!("{expose} is false, so nxm:// is treated as a web address"),
            "prefs.js",
        ));
    }
    let external = format!("network.protocol-handler.external.{SCHEME}");
    if pref_is_false(prefs, &external) {
        return Some((
            format!("{external} is false, so Firefox launches nothing at all"),
            "prefs.js",
        ));
    }

    None
}

/// One `user_pref("name", ...)` out of a `prefs.js`, by name.
///
/// **Both value shapes are legal and both matter here.** Firefox writes a string
/// quoted (`user_pref("k", "/path");`) and a boolean or number bare
/// (`user_pref("k", false);`), and reading only the quoted form silently misses
/// every `network.protocol-handler.expose.<scheme>` - which is a boolean, and is
/// therefore the whole of the "Firefox never launches it" finding.
///
/// The closing `");` is matched at the **end** of the line rather than at the
/// first one, so a quoted value containing that sequence still reads whole.
/// Values are returned exactly as written: a user reading this line needs the
/// bytes that are on disk, and un-escaping them would be a second parser to get
/// wrong.
pub fn pref(text: &str, name: &str) -> Option<String> {
    let prefix = format!("user_pref(\"{name}\", ");
    let line = text
        .lines()
        .find(|line| line.trim_start().starts_with(&prefix))?;
    let value = line.trim_start().strip_prefix(&prefix)?;
    value
        .strip_prefix('"')
        .and_then(|quoted| quoted.strip_suffix("\");"))
        .or_else(|| value.strip_suffix(");"))
        .map(str::to_string)
}

/// One boolean `user_pref`, read as a bool or not at all.
///
/// Only the literal `false` counts. A pref written as a number, or one Firefox
/// has never written, must not be reported as the user having switched
/// something off.
fn pref_is_false(text: &str, name: &str) -> bool {
    pref(text, name).as_deref() == Some("false")
}

#[cfg(test)]
mod tests {
    use super::*;

    const HANDLERS: &str = r#"{
      "defaultHandlersVersion": 1,
      "mimeTypes": {},
      "extensions": {},
      "schemes": {
        "nxm": {
          "action": "useHelperApp",
          "ask": false,
          "handlers": [{"name": "Vortex", "path": "/usr/bin/vortex"}]
        }
      }
    }"#;

    /// A scratch profile tree, gone when it goes out of scope.
    struct Home {
        root: PathBuf,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "nxmproxy-browser-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            Home { root }
        }

        fn profile(&self, relative: &str) -> PathBuf {
            let path = self.root.join(relative);
            std::fs::create_dir_all(&path).unwrap();
            path
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// A profile that has never seen an `nxm` link, which is most of them.
    ///
    /// The false green this module could introduce is a finding printed on a
    /// machine that is not broken, so the states that must stay silent are
    /// asserted as loudly as the one that must speak.
    #[test]
    fn a_profile_that_asked_nothing_has_no_finding() {
        assert_eq!(gecko_says("", ""), None, "a profile that never saw a link");
        assert_eq!(
            gecko_says(r#"{"schemes":{},"mimeTypes":{}}"#, ""),
            None,
            "handlers.json exists and names no scheme"
        );
        assert_eq!(
            gecko_says(r#"{"schemes":{"nxm":{"action":"useSystemDefault"}}}"#, ""),
            None,
            "useSystemDefault is the OS being used, which is the opposite of an override"
        );
        assert_eq!(
            gecko_says("not json at all", "user_pref(\"a\", 1);"),
            None,
            "an unparseable file is an unread one, never a finding"
        );
    }

    /// The reported failure: nxmproxy owns `mimeapps.list`, and Firefox does
    /// not consult it because this profile says so.
    #[test]
    fn a_remembered_helper_application_is_reported_with_its_path() {
        assert_eq!(
            gecko_says(HANDLERS, ""),
            Some(("/usr/bin/vortex".to_string(), "handlers.json")),
            "the application, not the file - 'which application is deciding' is the question"
        );
    }

    /// A remembered action whose path cannot be read is still an override.
    ///
    /// Reporting nothing here is the false negative of the false green: the
    /// profile is set to use a helper application, so the OS is out of the
    /// loop, and a silent `test` over that machine draws a working install over
    /// a broken one.
    #[test]
    fn a_remembered_action_with_no_readable_path_is_still_an_override() {
        assert_eq!(
            gecko_says(r#"{"schemes":{"nxm":{"action":"useHelperApp"}}}"#, ""),
            Some((
                "(a helper application Firefox chose)".to_string(),
                "handlers.json"
            ))
        );
    }

    /// The three `prefs.js` shapes that are findings, and the ones that are not.
    #[test]
    fn the_preferences_are_read_the_way_firefox_writes_them() {
        assert_eq!(
            gecko_says(
                "",
                "user_pref(\"browser.zoom.factor\", 0);\n\
                 user_pref(\"network.protocol-handler.app.nxm\", \"/opt/Vortex/Vortex\");\n"
            ),
            Some((
                "network.protocol-handler.app.nxm = \"/opt/Vortex/Vortex\"".to_string(),
                "prefs.js"
            )),
            "a pref for another scheme must not leak in, and a non-string pref must not make \
             the file unreadable"
        );
        assert_eq!(
            gecko_says(
                "",
                "user_pref(\"network.protocol-handler.expose.nxm\", false);\n"
            ),
            Some((
                "network.protocol-handler.expose.nxm is false, so nxm:// is treated as a web \
                 address"
                    .to_string(),
                "prefs.js"
            ))
        );
        assert_eq!(
            gecko_says(
                "",
                "user_pref(\"network.protocol-handler.external.nxm\", false);\n"
            ),
            Some((
                "network.protocol-handler.external.nxm is false, so Firefox launches nothing \
                 at all"
                    .to_string(),
                "prefs.js"
            ))
        );
        // True, and false for another scheme: neither is an override. Reporting
        // `true` as one would name a browser that is working.
        assert_eq!(
            gecko_says(
                "",
                "user_pref(\"network.protocol-handler.external.nxm\", true);\n"
            ),
            None
        );
        assert_eq!(
            gecko_says(
                "",
                "user_pref(\"network.protocol-handler.expose.magnet\", false);\n"
            ),
            None
        );
    }

    /// An application outranks a boolean: the reporting order [`gecko_says`]
    /// documents, asserted so it cannot be reshuffled by accident.
    #[test]
    fn an_application_is_reported_before_a_boolean_that_also_applies() {
        let prefs = "user_pref(\"network.protocol-handler.expose.nxm\", false);\n";
        assert_eq!(
            gecko_says(HANDLERS, prefs),
            Some(("/usr/bin/vortex".to_string(), "handlers.json")),
            "the question is which application gets the link; expose=false explains why it \
             gets nothing, which is a different answer to a different question"
        );
    }

    /// [`overrides`] over a real tree: the part the pure function above cannot
    /// reach. That a profile is found, that the finding names the file the user
    /// has to open, and that a Flatpak browser's relocated home is searched.
    #[test]
    fn a_flatpak_profile_is_found_and_names_the_file_it_came_from() {
        let home = Home::new("flatpak");
        let profile =
            home.profile(".var/app/org.mozilla.firefox/.mozilla/firefox/8x2q.default-release");
        std::fs::write(profile.join("handlers.json"), HANDLERS).unwrap();

        assert_eq!(
            overrides(&home.root),
            vec![Override {
                browser: "org.mozilla.firefox".to_string(),
                says: "/usr/bin/vortex".to_string(),
                source: profile.join("handlers.json"),
            }],
            "a Flatpak browser's relocated home must be searched"
        );
    }

    /// The same, for the other two layouts, and the silence in between.
    #[test]
    fn a_native_profile_is_found_and_an_empty_one_is_not() {
        let home = Home::new("native");
        let profile = home.profile(".mozilla/firefox/p1.default");
        assert!(
            overrides(&home.root).is_empty(),
            "a profile that has never opened a link is not a finding"
        );

        std::fs::write(
            profile.join("prefs.js"),
            "user_pref(\"network.protocol-handler.app.nxm\", \"/usr/bin/vortex\");\n",
        )
        .unwrap();
        assert_eq!(
            overrides(&home.root),
            vec![Override {
                browser: "Firefox".to_string(),
                says: "network.protocol-handler.app.nxm = \"/usr/bin/vortex\"".to_string(),
                source: profile.join("prefs.js"),
            }],
            "a prefs.js finding must name prefs.js, not the other file in the same directory"
        );
    }
}
