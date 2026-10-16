// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Windows: register through a per-user, application-specific ProgID.
//!
//! ## Deliberate divergence from upstream
//!
//! Upstream (`TanninOne/nxmproxy`, `src/win32.rs` and `src/main.rs:114-135`)
//! did two things this fork refuses to do:
//!
//! * It wrote `HKEY_CLASSES_ROOT\nxm\shell\open\command`. `HKCR` is the merged
//!   machine-plus-user view, so that is an overwrite of whatever Vortex or MO2
//!   had registered, affecting every user on the machine. Upstream expected it
//!   to fail and caught `ERROR_ACCESS_DENIED`, then **re-launched itself
//!   elevated** with the `runas` verb.
//! * It wrote the bare scheme key `nxm` rather than a ProgID of its own. There
//!   was then nothing for the Windows chooser to choose between, and
//!   `deregister` never removed the key, so only a manual delete un-installed
//!   it.
//!
//! What we do instead: a ProgID of our own under **`HKEY_CURRENT_USER`**,
//! plus `SupportedTypes` and a `RegisteredApplications` entry so the app shows
//! up in Default Apps and the shell can offer a choice. It needs no elevation
//! (`HKCU` is always writable by its owner), it does not disturb other users,
//! and it does not touch the `nxm` key itself. `uninstall` removes exactly what
//! `install` created and nothing else.
//!
//! ## Why `reg.exe` instead of the Win32 API
//!
//! The `bindings` crate (the `windows` 0.17 build macro) is what made this
//! project Windows-only: its build script emits code for Windows targets only
//! and `cargo check` dies on Linux and macOS. Driving `reg.exe` removes two
//! dependencies and the whole build-script problem, and leaves the risky part,
//! the exact key names and quoting, as plain argument vectors that are
//! compiled and unit tested on every platform, because this module contains no
//! FFI and therefore builds anywhere.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use crate::logger;
use crate::platform::Claim;

/// Application-specific ProgID. Ours, so we never collide with another
/// manager's registration.
pub const PROG_ID: &str = "GameModManager.NxmProxy";
const SCHEME: &str = "nxm";

/// The id this platform registers nxmproxy under, which is what the shell's
/// scheme association names when nxmproxy is the handler. See
/// [`crate::platform::Status`] for why `test` needs to compare it.
pub const OWN_ID: &str = PROG_ID;
const CLASSES: &str = r"HKCU\Software\Classes";
const REGISTERED_APPLICATIONS: &str =
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\RegisteredApplications";
const REGISTERED_APPLICATION_NAME: &str = "GameModManager NXM Proxy";
const FRIENDLY_TYPE: &str = "URL:NXM Protocol";

/// One registry operation, built before anything is executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegOp {
    Add {
        key: String,
        value: Option<String>,
        kind: &'static str,
        data: String,
    },
    DeleteKey {
        key: String,
    },
    DeleteValue {
        key: String,
        value: String,
    },
}

/// The command line the shell runs to hand us one link.
pub fn handler_command_line(exe: &Path) -> String {
    format!("\"{}\" url \"%1\"", exe.to_string_lossy())
}

/// The registry key whose value is the effective handler for the scheme.
///
/// A [`Declaration::source`] here, so the chain `test` prints names a place the
/// user can open in `regedit` rather than a filename that is not one.
pub fn handler_key() -> String {
    format!(r"{CLASSES}\{SCHEME}\shell\open\command")
}

/// The program part of a handler command line, i.e. its first token.
///
/// Windows stores the whole command under the bare scheme key, so "is this
/// ours" is a comparison of paths and not the desktop-id comparison the other
/// two platforms do. Pure, and the reason [`owns`] can say which of the two it
/// is doing.
pub fn program_of(command: &str) -> &str {
    let trimmed = command.trim();
    match trimmed.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(rest),
        None => trimmed.split_whitespace().next().unwrap_or(trimmed),
    }
}

/// Whether the effective handler is this very program.
///
/// Compared against [`std::env::current_exe`] rather than against
/// [`OWN_ID`], because the value under the bare `nxm` key is a command line and
/// a ProgID is not in it. The two can differ in any case: `install` publishes the
/// ProgID, and the shell only runs the command once the user has chosen a
/// default app, which no program may choose for them.
pub fn owns(resolution: &crate::platform::Resolution) -> bool {
    let Some(command) = resolution.first_id() else {
        return false;
    };
    let Ok(ours) = std::env::current_exe() else {
        return false;
    };
    let same = |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    same(Path::new(program_of(command))) == same(&ours)
}

/// Everything `install` does, as data.
///
/// Kept separate from execution so a test can assert the security-relevant
/// properties without a Windows machine: every key is under `HKCU`, and the
/// bare `nxm` scheme key is never among them.
pub fn install_operations(exe: &Path) -> Vec<RegOp> {
    let prog = format!(r"{CLASSES}\{PROG_ID}");
    let capabilities = format!("Software\\Classes\\{PROG_ID}\\Capabilities");
    vec![
        RegOp::Add {
            key: prog.clone(),
            value: None,
            kind: "REG_SZ",
            data: FRIENDLY_TYPE.to_string(),
        },
        // The empty "URL Protocol" value is what marks this ProgID as a URL
        // scheme handler to the shell.
        RegOp::Add {
            key: prog.clone(),
            value: Some("URL Protocol".to_string()),
            kind: "REG_SZ",
            data: String::new(),
        },
        RegOp::Add {
            key: format!(r"{prog}\DefaultIcon"),
            value: None,
            kind: "REG_SZ",
            data: format!("{},0", exe.to_string_lossy()),
        },
        RegOp::Add {
            key: format!(r"{prog}\shell\open\command"),
            value: None,
            kind: "REG_SZ",
            data: handler_command_line(exe),
        },
        RegOp::Add {
            key: format!("{prog}\\FriendlyTypeName"),
            value: None,
            kind: "REG_SZ",
            data: "NXM Proxy".to_string(),
        },
        // SupportedTypes + RegisteredApplications + Capabilities are the
        // documented shape that puts an app in Default Apps and lets the user
        // choose, instead of silently taking the scheme.
        RegOp::Add {
            key: format!("{prog}\\SupportedTypes"),
            value: Some(SCHEME.to_string()),
            kind: "REG_SZ",
            data: FRIENDLY_TYPE.to_string(),
        },
        RegOp::Add {
            key: format!("{prog}\\Capabilities"),
            value: Some("ApplicationName".to_string()),
            kind: "REG_SZ",
            data: "NXM Proxy".to_string(),
        },
        RegOp::Add {
            key: format!(r"{CLASSES}\{PROG_ID}\Capabilities\URLAssociations"),
            value: Some(SCHEME.to_string()),
            kind: "REG_SZ",
            data: PROG_ID.to_string(),
        },
        RegOp::Add {
            key: REGISTERED_APPLICATIONS.to_string(),
            value: Some(REGISTERED_APPLICATION_NAME.to_string()),
            kind: "REG_SZ",
            data: capabilities,
        },
    ]
}

/// Exactly the inverse. Note what is *not* here: `HKCU\Software\Classes\nxm`
/// and `HKCR` of any kind. Those may belong to another manager.
pub fn uninstall_operations() -> Vec<RegOp> {
    vec![
        RegOp::DeleteValue {
            key: REGISTERED_APPLICATIONS.to_string(),
            value: REGISTERED_APPLICATION_NAME.to_string(),
        },
        RegOp::DeleteKey {
            key: format!(r"{CLASSES}\{PROG_ID}"),
        },
    ]
}

fn reg_args(op: &RegOp) -> Vec<OsString> {
    match op {
        RegOp::Add {
            key,
            value,
            kind,
            data,
        } => {
            let mut args = vec![OsString::from("add"), OsString::from(key.as_str())];
            match value {
                Some(name) => {
                    args.push(OsString::from("/v"));
                    args.push(OsString::from(name.as_str()));
                }
                None => args.push(OsString::from("/ve")),
            }
            args.push(OsString::from("/t"));
            args.push(OsString::from(*kind));
            args.push(OsString::from("/d"));
            args.push(OsString::from(data.as_str()));
            args.push(OsString::from("/f"));
            args
        }
        RegOp::DeleteKey { key } => {
            vec![
                OsString::from("delete"),
                OsString::from(key.as_str()),
                OsString::from("/f"),
            ]
        }
        RegOp::DeleteValue { key, value } => vec![
            OsString::from("delete"),
            OsString::from(key.as_str()),
            OsString::from("/v"),
            OsString::from(value.as_str()),
            OsString::from("/f"),
        ],
    }
}

/// Render an argv for a log line and an error message.
fn render(args: &[OsString]) -> String {
    args.iter()
        .map(|a| a.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

fn exec_reg(op: &RegOp) -> Result<(), String> {
    let args = reg_args(op);
    logger::info(format!("reg {}", render(&args)));
    let output = Command::new("reg")
        .args(&args)
        .output()
        .map_err(|e| format!("could not run reg.exe ({e}); is this Windows?"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "reg {} failed: {}",
            render(&args),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| format!("could not determine this executable's path: {e}"))?;
    for op in install_operations(&exe) {
        exec_reg(&op)?;
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    for op in uninstall_operations() {
        exec_reg(&op)?;
    }
    Ok(())
}

/// The settings page for this exact application, opened for the user to click.
///
/// `ms-settings:defaultapps` is Microsoft's documented URI for the Default Apps
/// page, and the `registeredAppUser` parameter - added in Windows 11 21H2 - jumps
/// straight to one app's page. **User, not Machine**, because the app is
/// registered under `HKCU`; a machine-scoped lookup would not find it.
///
/// Built by a pure function so the exact string is testable on Linux, where no
/// test can otherwise see it.
pub fn default_apps_uri() -> String {
    // `registeredAppUser` takes a URI-escaped name. The registered name has a
    // space in it, so escaping is not optional here.
    format!(
        "ms-settings:defaultapps?registeredAppUser={}",
        REGISTERED_APPLICATION_NAME.replace(' ', "%20")
    )
}

/// Windows will not let any program assign itself a default app.
///
/// The effective default lives in
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\FileExts\...` or
/// `...\App Paths`, behind `UserChoice`, whose `Hash` is a keyed digest Windows
/// recomputes on every write and validates on every read. Writing it by hand
/// produces a hash Windows rejects, and `reg add` against the key fails
/// outright. This is a deliberate platform policy - documented as "apps cannot
/// change default apps without user interaction in system UI" - not an
/// obstacle to route around, so nothing here attempts it.
///
/// What `install` can do is publish a correct ProgID and a correct
/// `RegisteredApplications` entry (which [`install`] does, and which the tests
/// assert on every platform) so the app *appears* in Default Apps, and then
/// open that page. The user finishes in one click.
///
/// The result is therefore never [`Claim::verified`]. There is no read-back to
/// prove it and no program on this machine is permitted to make it true.
pub fn claim() -> Result<Claim, String> {
    let uri = default_apps_uri();
    logger::info(format!("opening {uri}"));

    // `explorer.exe` resolves the `ms-settings:` protocol for us. The
    // alternative the docs show, `ShellExecuteExW`, would mean an FFI block
    // here, and the reason this file drives `reg.exe` rather than the Win32 API
    // is precisely to keep FFI out of a file that compiles on three platforms.
    let opened = Command::new("explorer").arg(&uri).spawn();

    let note = match &opened {
        Ok(_) => format!("Opened Windows Settings at {uri}."),
        Err(e) => format!("Could not open Windows Settings ({e}). Open it yourself at {uri}."),
    };

    Ok(Claim::needs_user(format!(
        "{note}\n  \
         nxmproxy is registered as URL:NXM Protocol and is listed in Default Apps, but \
         Windows does not let any program choose the default app for you.\n  \
         1. Press Win, type \"Default Apps\" and press Enter.\n  \
         2. Search for \"NXM Proxy\" and click it.\n  \
         3. Under \"Set defaults\", click \"Set default\" for .nxm links.\n  \
         Until you do, nxm:// links still open with whatever handles them now."
    )))
}

/// Give the scheme back.
///
/// The ProgID and the `RegisteredApplications` value are removed by
/// [`uninstall`], which is the part that can actually be undone. The *default*
/// choice cannot: Windows keeps `UserChoice` pointing at a ProgID that no longer
/// exists and falls back to its own prompt, and no program may change that
/// without user interaction. So the scheme comes back to the chooser, and the
/// user is told so rather than being told the reverse.
pub fn unclaim() -> Result<Claim, String> {
    Ok(Claim::needs_user(
        "Windows will not let nxmproxy take the nxm:// default back, so nxm:// links fall back \
         to whatever Windows offers now.\n  \
         To point them at another manager: Settings > Apps > Default apps > search for it, \
         then Set default.",
    ))
}

/// Read the *effective* handler for the scheme.
///
/// Read only. `HKCR` here is the merged machine-plus-user view, which is
/// exactly what you want when asking "what would the shell do" - it is only
/// ever wrong to write there, and we never do.
pub fn current_handler() -> Result<Option<String>, String> {
    let output = Command::new("reg")
        .args(["query", &handler_key(), "/ve"])
        .output()
        .map_err(|e| format!("could not run reg.exe ({e}); is this Windows?"))?;
    if !output.status.success() {
        // Nothing registered under the bare scheme key: exactly the polite
        // state, where Windows asks the user.
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .skip(1)
        .find_map(|l| l.split_once("REG_SZ"))
        .map(|(_, v)| v.trim().to_string()))
}

/// Read the machine's answer: every place Windows was told, and who won.
///
/// Windows keeps exactly one place - the bare `nxm` key - so this is a chain of
/// at most one. It is still a [`Resolution`] rather than a bare name, because
/// the question a user is asking is "where does this answer come from", and
/// here the answer is a registry key they can open.
pub fn resolve() -> crate::platform::Resolution {
    let command = current_handler().unwrap_or(None);
    let declarations: Vec<crate::platform::Declaration> = command
        .map(|command| {
            vec![crate::platform::Declaration {
                source: handler_key(),
                section: "scheme handler".to_string(),
                // One element: a command line is one handler, not a ranked list.
                ids: vec![command],
            }]
        })
        .unwrap_or_default();
    let rule = if declarations.is_empty() {
        "no application is registered for nxm://".to_string()
    } else {
        format!("the {} key is the effective handler", handler_key())
    };
    crate::platform::Resolution {
        declarations,
        rule,
        // `reg.exe` answered; an empty list means nothing is registered under
        // the scheme key, which is a state and not an absence of knowledge.
        readable: true,
    }
}

/// The key whose presence means "nxmproxy is registered".
///
/// Pure, so a test on any platform can assert *what we look up* without a
/// Windows machine. This is the key that must be asked about.
pub fn registration_key() -> String {
    format!(r"{CLASSES}\{PROG_ID}\shell\open\command")
}

/// True when nxmproxy's ProgID is published, i.e. nxmproxy is one of the
/// applications Windows may offer for `nxm://`.
///
/// This deliberately does **not** mean "nxmproxy is the default handler". We
/// never write the bare `nxm` key, so by design we never take the scheme by
/// force, and querying the default handler here would report "not installed"
/// forever, even straight after a successful `install`. Registered means
/// eligible; `current_handler` is what reports who actually handles it.
pub fn is_registered() -> Result<bool, String> {
    let output = Command::new("reg")
        .args(["query", &registration_key(), "/ve"])
        .output()
        .map_err(|e| format!("could not run reg.exe ({e}); is this Windows?"))?;
    Ok(output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys_of(ops: &[RegOp]) -> Vec<&str> {
        ops.iter()
            .map(|op| match op {
                RegOp::Add { key, .. }
                | RegOp::DeleteKey { key }
                | RegOp::DeleteValue { key, .. } => key.as_str(),
            })
            .collect()
    }

    #[test]
    fn every_key_is_per_user() {
        let exe = Path::new(r"C:\Tools\nxmproxy.exe");
        for key in keys_of(&install_operations(exe)) {
            assert!(
                key.starts_with(r"HKCU\"),
                "not per-user, would touch other users: {key}"
            );
        }
        for key in keys_of(&uninstall_operations()) {
            assert!(key.starts_with(r"HKCU\"), "not per-user: {key}");
        }
    }

    #[test]
    fn the_bare_nxm_scheme_key_is_never_touched() {
        // Writing HKCU\Software\Classes\nxm (let alone HKCR) is how you
        // stomp on Vortex or MO2. Neither install nor uninstall may do it.
        for key in keys_of(&install_operations(Path::new("x.exe"))) {
            assert_ne!(key, r"HKCU\Software\Classes\nxm");
        }
        for key in keys_of(&uninstall_operations()) {
            assert_ne!(key, r"HKCU\Software\Classes\nxm");
        }
        for key in keys_of(&install_operations(Path::new("x.exe"))) {
            assert!(
                !key.starts_with("HKCR"),
                "HKCR must never be written: {key}"
            );
        }
    }

    #[test]
    fn there_is_no_elevation_anywhere() {
        // Upstream re-launched itself with the "runas" verb on
        // ERROR_ACCESS_DENIED. HKCU needs no elevation, so neither should we.
        // Scans production source, see `crate::platform::production_code`.
        let code = crate::platform::production_code(include_str!("windows.rs"));
        for forbidden in ["ShellExecuteExW", "spawn_elevated", "ERROR_ACCESS_DENIED"] {
            assert!(
                !code.contains(forbidden),
                "self elevation appears in production code: {forbidden}"
            );
        }
    }

    #[test]
    fn the_progid_is_ours_not_the_scheme() {
        let ops = install_operations(Path::new("x.exe"));
        let keys = keys_of(&ops);
        assert!(keys.iter().any(|k| k.ends_with(PROG_ID)));
        assert!(keys
            .iter()
            .all(|k| k.contains(PROG_ID) || k.contains("RegisteredApplications")));
    }

    #[test]
    fn install_registers_the_scheme_so_the_chooser_can_offer_it() {
        let ops = install_operations(Path::new("x.exe"));
        assert!(
            ops.contains(&RegOp::Add {
                key: format!(r"{CLASSES}\{PROG_ID}"),
                value: Some("URL Protocol".to_string()),
                kind: "REG_SZ",
                data: String::new(),
            }),
            "the empty URL Protocol value marks this as a scheme handler"
        );
        assert!(ops.iter().any(|op| matches!(op,
            RegOp::Add { key, value: Some(v), .. } if key.contains("SupportedTypes") && v == "nxm")));
        assert!(ops.iter().any(|op| matches!(op,
            RegOp::Add { key, value: Some(v), .. } if key.contains("URLAssociations") && v == "nxm")));
        assert!(ops.iter().any(|op| matches!(op,
            RegOp::Add { key, .. } if key.contains("RegisteredApplications"))));
    }

    #[test]
    fn uninstall_removes_exactly_what_install_created() {
        let exe = Path::new(r"C:\Tools\nxmproxy.exe");
        let created: Vec<String> = install_operations(exe)
            .iter()
            .map(|op| match op {
                RegOp::Add { key, value, .. } => {
                    format!("{key}\u{1}{}", value.clone().unwrap_or_default())
                }
                _ => String::new(),
            })
            .filter(|s| !s.is_empty())
            .collect();

        let removed: Vec<String> = uninstall_operations()
            .iter()
            .map(|op| match op {
                RegOp::DeleteKey { key } => format!("{key}\u{1}"),
                RegOp::DeleteValue { key, value } => format!("{key}\u{1}{value}"),
                _ => String::new(),
            })
            .collect();

        for created in &created {
            let (key, value) = created.split_once('\u{1}').unwrap();
            // Coverage means "the value is deleted, or the key sits under a key
            // we delete". `reg delete` on a key removes its subkeys too, which
            // is how the DefaultIcon and Capabilities subtrees get cleaned up
            // by the single DeleteKey of our ProgID.
            let covered = removed.iter().any(|r| {
                let (rk, rv) = r.split_once('\u{1}').unwrap();
                if rv.is_empty() {
                    rk == key || key.starts_with(&format!("{rk}\\"))
                } else {
                    rk == key && rv == value
                }
            });
            assert!(covered, "uninstall leaves {created} behind");
        }
        assert_eq!(
            removed.len(),
            2,
            "uninstall must not go deleting unrelated keys"
        );
    }

    #[test]
    fn registration_is_asked_about_our_progid_not_the_bare_scheme() {
        // The bug this asserts against: asking reg.exe about HKCR\nxm, the
        // default handler, to decide whether we are *registered*. We never
        // write that key on purpose, so `test` reported "not installed"
        // forever, and the install/uninstall round trip failed on Windows.
        let key = registration_key();
        assert!(key.contains(PROG_ID), "{key}");
        assert!(key.starts_with(r"HKCU\"), "{key}");
        assert_ne!(key, r"HKCR\nxm\shell\open\command");
        assert!(
            !key.ends_with(r"\nxm"),
            "must not be the bare scheme: {key}"
        );
    }

    #[test]
    fn the_handler_command_line_quotes_a_path_with_spaces() {
        assert_eq!(
            handler_command_line(Path::new(r"C:\Program Files\GMM\nxmproxy.exe")),
            r#""C:\Program Files\GMM\nxmproxy.exe" url "%1""#
        );
    }

    #[test]
    fn reg_argv_keeps_the_command_line_as_one_value() {
        let op = RegOp::Add {
            key: format!(r"{CLASSES}\{PROG_ID}\shell\open\command"),
            value: None,
            kind: "REG_SZ",
            data: handler_command_line(Path::new(r"C:\A B\nxmproxy.exe")),
        };
        let args = reg_args(&op);
        let rendered: Vec<String> = args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            rendered,
            vec![
                "add",
                r"HKCU\Software\Classes\GameModManager.NxmProxy\shell\open\command",
                "/ve",
                "/t",
                "REG_SZ",
                "/d",
                r#""C:\A B\nxmproxy.exe" url "%1""#,
                "/f",
            ]
        );
    }

    #[test]
    fn the_settings_uri_names_this_app_and_escapes_its_spaces() {
        // `ms-settings:defaultapps` is Microsoft's documented URI for the page.
        let uri = default_apps_uri();
        assert!(
            uri.starts_with("ms-settings:defaultapps"),
            "wrong settings page: {uri}"
        );
        // The parameter is scoped to *User* because the app is registered under
        // HKCU. `registeredAppMachine` would not find a per-user registration.
        assert!(
            uri.contains("registeredAppUser="),
            "a per-user app must use the User parameter: {uri}"
        );
        assert!(
            !uri.contains("registeredAppMachine="),
            "we are not machine-scoped: {uri}"
        );
        // An unescaped space truncates the parameter at the first space and
        // lands Windows on the default apps page with no app selected, which is
        // exactly the one-click flow this is supposed to save.
        assert!(!uri.contains(' '), "the URI must be escaped: {uri}");
        assert!(uri.contains("%20"), "expected escaped spaces in: {uri}");
        // And it names the app we actually registered under RegisteredApplications.
        let escaped = REGISTERED_APPLICATION_NAME.replace(' ', "%20");
        assert!(
            uri.ends_with(&escaped),
            "must point at our own registration: {uri} vs {escaped}"
        );
    }

    #[test]
    fn windows_never_reports_a_claim_it_could_not_make() {
        // The load-bearing assertion for this whole platform.
        //
        // `HKCU\...\UserChoice` is hash-protected and no app may write it, so
        // `claim` cannot finish the job. If it ever returned `verified()`, a
        // script would read exit 0 over an install where nxm:// links still
        // open with another manager - a broken install, automated. This runs on
        // every platform because the decision is data, not FFI.
        let claim = claim().expect("claim must not error");
        assert_eq!(
            claim.exit_code(),
            crate::platform::EXIT_NEEDS_USER,
            "Windows cannot choose the default app, so it must not report success"
        );
        assert!(
            !claim.instruction.trim().is_empty(),
            "the user must be told exactly what to click"
        );
        // And the instruction has to be actionable, not a shrug.
        assert!(
            claim.instruction.contains("Set default"),
            "expected the concrete click path: {}",
            claim.instruction
        );
    }

    #[test]
    fn nothing_writes_the_user_choice_hash() {
        // The forbidden route, stated as a test. If anyone "fixes" Windows
        // claiming by writing UserChoice, this is the test that says why not:
        // the value is a keyed digest Windows validates, so a hand-written one
        // is rejected and `reg add` fails. Scans production source, see
        // `crate::platform::production_code`.
        let code = crate::platform::production_code(include_str!("windows.rs"));
        for forbidden in ["UserChoice", "FileExts", "App Paths"] {
            assert!(
                !code.contains(forbidden),
                "production code touches {forbidden}, the hash-protected default-app key"
            );
        }
    }

    #[test]
    fn uninstall_is_honest_that_windows_keeps_the_choice() {
        // Symmetric with claim(): it must not claim to have undone something
        // the OS will not let it undo.
        let un = unclaim().expect("unclaim must not error");
        assert_eq!(un.exit_code(), crate::platform::EXIT_NEEDS_USER);
        assert!(!un.instruction.trim().is_empty());
    }
}
