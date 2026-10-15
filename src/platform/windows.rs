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

/// Application-specific ProgID. Ours, so we never collide with another
/// manager's registration.
pub const PROG_ID: &str = "GameModManager.NxmProxy";
const SCHEME: &str = "nxm";
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

/// Read the *effective* handler for the scheme.
///
/// Read only. `HKCR` here is the merged machine-plus-user view, which is
/// exactly what you want when asking "what would the shell do" - it is only
/// ever wrong to write there, and we never do.
pub fn current_handler() -> Result<Option<String>, String> {
    let output = Command::new("reg")
        .args(["query", r"HKCR\nxm\shell\open\command", "/ve"])
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

pub fn is_registered() -> Result<bool, String> {
    let Some(command) = current_handler()? else {
        return Ok(false);
    };
    let this_exe = std::env::current_exe().ok();
    Ok(match this_exe {
        Some(exe) => command.contains(&exe.to_string_lossy().to_string()),
        None => command.to_ascii_lowercase().contains("nxmproxy"),
    })
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
}
