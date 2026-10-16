// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Configuration file: one TOML file, two maps.
//!
//! Two deliberate differences from upstream (TanninOne/nxmproxy):
//!
//! * A manager is stored as an **argv vector** (`exec` + `args`), not as a
//!   command *string*. Upstream substituted the url into a string and split
//!   the result, so a url containing a quote changed the arguments.
//! * Loading is **per item**. One bad entry is dropped with a warning and the
//!   rest survive. Upstream deserialised all-or-nothing and then `.expect()`ed,
//!   so a single bad byte panicked the handler.
//!
//! File format (see README for the per-platform location):
//!
//! ```toml
//! [games]
//! skyrimse = "vortex"
//! _ = "vortex"
//!
//! [managers.vortex]
//! exec = "/usr/bin/vortex"
//! args = []
//! pipe = "vortex_download"
//! ```
//!
//! A third difference from upstream: a manager can be **remembered without being
//! chosen**. `discovered` names the ids that were found on this machine by
//! `nxmproxy discover` rather than typed by the user. They are present and
//! usable, and they route nothing until an `assign` says so - see
//! [`crate::route::resolve`].

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File name inside the config directory.
pub const CONFIG_FILE: &str = "config.toml";

/// The reserved domain key meaning "every game with no explicit assignment".
pub const CATCH_ALL: &str = "_";

/// The field codes that mean "the link goes here".
///
/// `%1` is nxmproxy's own token. `%u` and `%U` are the desktop entry
/// specification's codes for the same thing, and they are what an Electron,
/// AppImage or Flatpak entry writes, so a manager registered with the argument
/// vector copied out of its own desktop entry still receives the link. All
/// three are accepted; nothing else about the vector changes.
const URL_FIELDS: [&str; 3] = ["%1", "%u", "%U"];

/// True when `arg` asks for the link, by any of the three spellings.
fn asks_for_url(arg: &str) -> bool {
    URL_FIELDS.iter().any(|field| arg.contains(field))
}

/// Put `url` where a field code asked for it, inside one argument.
///
/// **One pass over the argument.** The url arrives from a browser and is
/// untrusted, so a url that itself contains `%u` must not be re-scanned for
/// field codes and substituted into a second time.
pub fn substitute_url(arg: &str, url: &str) -> String {
    let mut out = String::with_capacity(arg.len());
    let mut rest = arg;
    while let Some(at) = rest.find('%') {
        let (head, tail) = rest.split_at(at);
        out.push_str(head);
        // `get` rather than slicing: a `%` followed by a multi-byte character
        // is not a field code, and must not be a panic.
        if tail.get(..2).is_some_and(|f| URL_FIELDS.contains(&f)) {
            out.push_str(url);
            rest = &tail[2..];
        } else {
            out.push('%');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

/// One mod manager: an executable plus a fixed argument vector.
///
/// `%1`, `%u` or `%U` inside any element of `args` is replaced by the nxm url
/// at dispatch time. See [`crate::route::spawn_args`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manager {
    /// Executable path, or a bare command name resolved through `PATH`.
    pub exec: String,
    /// Fixed arguments, passed verbatim apart from the link substitution.
    #[serde(default)]
    pub args: Vec<String>,
    /// Optional listening address tried before spawning.
    ///
    /// A bare pipe name on Windows, which becomes `\\.\pipe\{name}`; the full
    /// path of a **Unix domain socket** everywhere else. The address is the
    /// whole discovery mechanism - there is nothing else to look up - and any
    /// failure to deliver falls back to spawning. See [`crate::pipe`] and
    /// README "Delivering to an already-running manager", which is the
    /// versioned contract for implementing a listener.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pipe: Option<String>,
}

impl Manager {
    /// True when the url will actually reach the manager.
    ///
    /// Shares the field codes with [`substitute_url`], so this can never say
    /// "no link" about a manager that is about to be given one - which is the
    /// disagreement a GUI reads `takes_url` to warn the user about.
    pub fn takes_url(&self) -> bool {
        self.args.iter().any(|a| asks_for_url(a))
    }
}

/// The whole configuration. `BTreeMap`, not `HashMap`, so `list --json` emits
/// the same bytes on every run and the GUI's output does not jitter.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// nexusmods.com domain -> manager id. Key [`CATCH_ALL`] is the fallback.
    #[serde(default)]
    pub games: BTreeMap<String, String>,
    /// Manager id -> manager.
    #[serde(default)]
    pub managers: BTreeMap<String, Manager>,
    /// Manager ids that discovery found on this machine and the user did not
    /// choose.
    ///
    /// This is the difference between *knowing* a manager exists and *having
    /// picked* it. `register` never adds to it and only `discover` does, so it
    /// cannot be reached by accident, and [`crate::route::resolve`] never lets
    /// one of these own a game by default. It is a set of ids rather than a
    /// flag on [`Manager`] so that one new field cannot break the struct
    /// literal every existing call site writes.
    ///
    /// Omitted from the file when empty, so a user who never runs `discover`
    /// sees no trace of it.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub discovered: BTreeSet<String>,
}

/// Outcome of loading the config file.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub config: Config,
    /// False when a file existed but could not be parsed. Writes are refused
    /// while this is false, so one bad byte cannot silently wipe every
    /// assignment the user has.
    pub writable: bool,
    /// Human readable problems, surfaced by `list` and written to the log.
    pub warnings: Vec<String>,
}

impl Config {
    /// Load from `dir`, creating nothing. A missing file is an empty config,
    /// which is a valid configuration: see [`crate::route::resolve`] step 3
    /// for why a user with exactly one manager needs nothing configured.
    pub fn load(dir: &Path) -> Loaded {
        let path = dir.join(CONFIG_FILE);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded {
                    config: Config::default(),
                    writable: true,
                    warnings: Vec::new(),
                }
            }
            Err(e) => {
                return Loaded {
                    config: Config::default(),
                    writable: true,
                    warnings: vec![format!("could not read {}: {}", path.display(), e)],
                }
            }
        };

        match toml::from_str::<Config>(&text) {
            Ok(mut parsed) => {
                let warnings = parsed.validate(&path);
                Loaded {
                    config: parsed,
                    writable: true,
                    warnings,
                }
            }
            Err(e) => {
                // Whole-file parse failure. Degrade to empty, loudly, and make
                // the file read-only for this run so the next write does not
                // overwrite whatever the user needs to repair.
                let where_ = offset_to_line_col(&text, e.span().map(|s| s.start));
                let mut warnings = vec![format!(
                    "{} is not valid TOML{}: {}\n  using an empty configuration; \
                     nothing was written back. Fix or remove the file to continue.",
                    path.display(),
                    where_,
                    e.message()
                )];
                warnings.extend(unparsed_item_notes(&text, &path));
                Loaded {
                    config: Config::default(),
                    writable: false,
                    warnings,
                }
            }
        }
    }

    /// Drop individually invalid entries, collecting a warning for each.
    /// Returns the surviving config via `self`.
    fn validate(&mut self, path: &Path) -> Vec<String> {
        let mut warnings = Vec::new();

        self.games.retain(|domain, manager_id| {
            let ok = !domain.trim().is_empty() && self.managers.contains_key(manager_id);
            if !ok {
                warnings.push(format!(
                    "{}: ignoring assignment {:?} -> {:?}: {}",
                    path.display(),
                    domain,
                    manager_id,
                    if self.managers.contains_key(manager_id) {
                        "empty domain"
                    } else {
                        "no such mod manager is registered"
                    }
                ));
            }
            ok
        });

        // Normalise domains so `assign SkyrimSE skyrimse` cannot create a
        // second unreachable entry for the same game.
        let mut normalised = BTreeMap::new();
        for (domain, manager_id) in std::mem::take(&mut self.games) {
            normalised.insert(domain.trim().to_ascii_lowercase(), manager_id);
        }
        self.games = normalised;

        self.managers.retain(|id, manager| {
            let ok = !manager.exec.trim().is_empty();
            if !ok {
                warnings.push(format!(
                    "{}: ignoring mod manager {:?}: no executable given",
                    path.display(),
                    id
                ));
            }
            ok
        });

        // A manager that did not survive validation is not remembered as
        // discovered either. A stale id here would be harmless today and would
        // silently downgrade a manager the user later registers by hand into
        // one that routes nothing.
        self.discovered.retain(|id| self.managers.contains_key(id));

        warnings
    }

    /// Write atomically: temp file plus rename, as upstream did. The rename is
    /// the atomic part, so a concurrent reader sees either the old file or the
    /// new one and never a half written one.
    pub fn write(&self, dir: &Path) -> Result<(), String> {
        let body =
            toml::to_string_pretty(self).map_err(|e| format!("could not serialise config: {e}"))?;
        let final_path = dir.join(CONFIG_FILE);
        let tmp_path = dir.join(format!("{CONFIG_FILE}.tmp"));

        fs::write(&tmp_path, body.as_bytes())
            .map_err(|e| format!("could not write {}: {}", tmp_path.display(), e))?;
        fs::rename(&tmp_path, &final_path)
            .map_err(|e| format!("could not replace {}: {}", final_path.display(), e))?;
        Ok(())
    }

    /// Register a manager, keeping any pipe it already had. Upstream kept
    /// pipes in a second map, so re-registering could not clobber one; with the
    /// pipe folded into [`Manager`] that has to be preserved explicitly.
    pub fn register_manager(&mut self, id: &str, manager: Manager) -> Result<(), String> {
        if id.trim().is_empty() {
            return Err("a mod manager needs a non-empty id".to_string());
        }
        if manager.exec.trim().is_empty() {
            return Err("a mod manager needs a non-empty executable".to_string());
        }
        // Registering by hand is the user choosing. A manager that discovery
        // found and the user then registered is no longer merely found, and
        // leaving it flagged would keep it out of routing for ever - found on a
        // machine where the user had already answered the question by hand.
        self.discovered.remove(id);
        let mut manager = manager;
        if let Some(existing) = self.managers.get(id) {
            manager.pipe = manager.pipe.or_else(|| existing.pipe.clone());
        }
        self.managers.insert(id.to_string(), manager);
        Ok(())
    }

    /// Remove a manager and every assignment that pointed at it, so no
    /// dangling reference survives. Upstream's `deregister` removed the manager
    /// but left `games` entries pointing at a manager that no longer existed.
    pub fn unregister_manager(&mut self, id: &str) -> Result<bool, String> {
        if self.managers.remove(id).is_none() {
            return Ok(false);
        }
        self.discovered.remove(id);
        self.games.retain(|_, assigned| assigned != id);
        Ok(true)
    }

    /// Remember a manager discovery found, **assigned to nothing**.
    ///
    /// Two rules, and both of them exist because the alternative is a user
    /// losing something:
    ///
    /// * **Never clobber.** An id that is already known is left exactly as it
    ///   is, including its `discovered` flag, its pipe and its assignments. A
    ///   re-scan is therefore safe to run at any time, and running it cannot
    ///   undo a `register` or an `assign`.
    /// * **Never remove anything.** A manager whose application was uninstalled
    ///   stays here and is reported unavailable by `list`; forgetting it
    ///   because a file went missing is how a list silently loses entries, and
    ///   the user would have to find and retype the whole thing.
    ///
    /// Returns whether anything was added.
    pub fn discover_manager(&mut self, id: &str, manager: Manager) -> Result<bool, String> {
        if self.managers.contains_key(id) {
            return Ok(false);
        }
        self.register_manager(id, manager)?;
        self.discovered.insert(id.to_string());
        Ok(true)
    }

    /// Assign a domain to a manager. `_` assigns the catch-all.
    pub fn assign(&mut self, manager_id: &str, domain: &str) -> Result<(), String> {
        if !self.managers.contains_key(manager_id) {
            return Err(format!(
                "no mod manager named {manager_id:?} is registered.\n  \
                 nxmproxy register {manager_id} <exec> [<args>...]"
            ));
        }
        let domain = domain.trim().to_ascii_lowercase();
        if domain.is_empty() {
            return Err("a game domain cannot be empty".to_string());
        }
        self.games.insert(domain, manager_id.to_string());
        Ok(())
    }

    /// Give a manager an address to try before spawning it.
    pub fn set_pipe(&mut self, manager_id: &str, pipe: &str) -> Result<(), String> {
        match self.managers.get_mut(manager_id) {
            Some(manager) => {
                manager.pipe = Some(pipe.to_string());
                Ok(())
            }
            None => Err(format!(
                "no mod manager named {:?} is registered",
                manager_id
            )),
        }
    }
}

/// `"line 3, column 12"` for a byte offset, so a corrupt-config warning points
/// at the offending byte instead of just saying "failed".
fn offset_to_line_col(text: &str, offset: Option<usize>) -> String {
    match offset {
        Some(offset) if offset <= text.len() => {
            let upto = &text[..offset];
            let line = upto.matches('\n').count() + 1;
            let column = upto
                .rsplit('\n')
                .next()
                .map(|l| l.chars().count())
                .unwrap_or(0)
                + 1;
            format!(" at line {line}, column {column}")
        }
        _ => String::new(),
    }
}

/// Names the sections present in a file we could not parse, so the warning
/// says which parts survived instead of implying the whole file is lost.
fn unparsed_item_notes(text: &str, path: &Path) -> Vec<String> {
    let mut found = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        for section in ["[managers", "[games"] {
            if line.starts_with(section) {
                let name = line
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string();
                if !found.contains(&name) {
                    found.push(name);
                }
            }
        }
    }
    if found.is_empty() {
        return Vec::new();
    }
    vec![format!(
        "{}: sections present in the unreadable file: {}",
        path.display(),
        found.join(", ")
    )]
}

/// Per-platform config directory, following the standard convention:
/// `$XDG_CONFIG_HOME/nxmproxy` (or `~/.config/nxmproxy`),
/// `~/Library/Application Support/nxmproxy`, `%APPDATA%\nxmproxy`.
///
/// `NXMPROXY_CONFIG_DIR` overrides it. That exists so tests and sandboxed
/// runs cannot touch the user's real configuration.
pub fn config_dir() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("NXMPROXY_CONFIG_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let base = directories::BaseDirs::new()
        .ok_or_else(|| "could not determine the user configuration directory".to_string())?;
    Ok(base.config_dir().join("nxmproxy"))
}

/// Create the config directory if it is missing.
pub fn ensure_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("could not create {}: {}", dir.display(), e))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("nxmproxy-test-{}-{}", std::process::id(), tag));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn empty_config_loads_clean() {
        let dir = tmp("empty-load");
        let loaded = Config::load(&dir);
        assert!(loaded.config.games.is_empty());
        assert!(loaded.config.managers.is_empty());
        assert!(loaded.writable);
        assert!(
            loaded.warnings.is_empty(),
            "a missing file must not warn: {:?}",
            loaded.warnings
        );
    }

    #[test]
    fn a_corrupt_byte_degrades_to_empty_and_refuses_writes() {
        let dir = tmp("corrupt");
        fs::write(dir.join(CONFIG_FILE), b"[games\nthis is not = = toml").unwrap();
        let loaded = Config::load(&dir);
        assert!(
            loaded.config.games.is_empty(),
            "a bad byte must not yield assignments"
        );
        assert!(
            !loaded.writable,
            "must not overwrite a file the user still needs to repair"
        );
        assert!(
            loaded.warnings.iter().any(|w| w.contains("not valid TOML")),
            "expected a parse warning, got {:?}",
            loaded.warnings
        );
        // The file itself is untouched: nothing was wiped.
        assert!(dir.join(CONFIG_FILE).exists());
    }

    #[test]
    fn one_bad_assignment_is_dropped_and_the_rest_survive() {
        let dir = tmp("per-item");
        fs::write(
            dir.join(CONFIG_FILE),
            r#"
[managers.good]
exec = "/bin/true"

[games]
skyrimse = "good"
solved = "ghost"
"#,
        )
        .unwrap();
        let loaded = Config::load(&dir);
        assert_eq!(
            loaded.config.games.get("skyrimse").map(String::as_str),
            Some("good")
        );
        assert!(
            !loaded.config.games.contains_key("solved"),
            "dangling assignment must be dropped"
        );
        assert_eq!(loaded.config.managers.len(), 1);
        assert!(loaded.writable, "a repairable entry is not a corrupt file");
        assert!(loaded.warnings.iter().any(|w| w.contains("ghost")));
    }

    #[test]
    fn a_manager_without_an_executable_is_dropped() {
        let dir = tmp("no-exec");
        fs::write(
            dir.join(CONFIG_FILE),
            "[managers.broken]\nexec = \"\"\n\n[managers.fine]\nexec = \"/bin/true\"\n",
        )
        .unwrap();
        let loaded = Config::load(&dir);
        assert!(!loaded.config.managers.contains_key("broken"));
        assert!(loaded.config.managers.contains_key("fine"));
    }

    #[test]
    fn domains_are_normalised_on_load() {
        let dir = tmp("normalise");
        fs::write(
            dir.join(CONFIG_FILE),
            "[managers.v]\nexec = \"/bin/true\"\n\n[games]\n  SkyrimSE  = \"v\"\n",
        )
        .unwrap();
        let loaded = Config::load(&dir);
        assert!(
            loaded.config.games.contains_key("skyrimse"),
            "got {:?}",
            loaded.config.games
        );
    }

    #[test]
    fn write_is_atomic_and_round_trips() {
        let dir = tmp("atomic");
        let mut cfg = Config::default();
        cfg.register_manager(
            "vortex",
            Manager {
                exec: "/usr/bin/vortex".into(),
                args: vec!["%1".into()],
                pipe: Some("vortex_download".into()),
            },
        )
        .unwrap();
        cfg.assign("vortex", "skyrimse").unwrap();
        cfg.write(&dir).unwrap();

        let loaded = Config::load(&dir);
        assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
        assert_eq!(
            loaded.config.managers["vortex"].pipe.as_deref(),
            Some("vortex_download")
        );
        assert_eq!(loaded.config.games["skyrimse"], "vortex");
        // temp file must not survive a successful write
        assert!(!dir.join(format!("{CONFIG_FILE}.tmp")).exists());
    }

    #[test]
    fn registering_an_existing_manager_keeps_its_pipe() {
        let mut cfg = Config::default();
        cfg.register_manager(
            "v",
            Manager {
                exec: "/a".into(),
                args: vec![],
                pipe: None,
            },
        )
        .unwrap();
        cfg.set_pipe("v", "thepipe").unwrap();
        cfg.register_manager(
            "v",
            Manager {
                exec: "/b".into(),
                args: vec![],
                pipe: None,
            },
        )
        .unwrap();
        assert_eq!(cfg.managers["v"].pipe.as_deref(), Some("thepipe"));
        assert_eq!(cfg.managers["v"].exec, "/b");
    }

    #[test]
    fn unregister_removes_assignments_that_pointed_at_it() {
        let mut cfg = Config::default();
        cfg.register_manager(
            "v",
            Manager {
                exec: "/a".into(),
                args: vec![],
                pipe: None,
            },
        )
        .unwrap();
        cfg.register_manager(
            "m",
            Manager {
                exec: "/b".into(),
                args: vec![],
                pipe: None,
            },
        )
        .unwrap();
        cfg.assign("v", "skyrimse").unwrap();
        cfg.assign("m", "fo76").unwrap();
        assert!(cfg.unregister_manager("v").unwrap());
        assert!(!cfg.managers.contains_key("v"));
        assert!(
            !cfg.games.contains_key("skyrimse"),
            "assignment must not dangle"
        );
        assert_eq!(cfg.games["fo76"], "m", "unrelated assignments must survive");
        assert!(
            !cfg.unregister_manager("v").unwrap(),
            "second removal reports nothing removed"
        );
    }

    #[test]
    fn assign_refuses_an_unregistered_manager() {
        let mut cfg = Config::default();
        let err = cfg.assign("ghost", "skyrimse").unwrap_err();
        assert!(
            err.contains("register"),
            "message must be actionable: {err}"
        );
    }

    #[test]
    fn a_discovered_manager_is_remembered_without_being_chosen() {
        let mut cfg = Config::default();
        assert!(cfg
            .discover_manager(
                "hd2arsenal",
                Manager {
                    exec: "/a".into(),
                    args: vec!["%1".into()],
                    pipe: None
                }
            )
            .unwrap());
        assert!(cfg.discovered.contains("hd2arsenal"));
        assert!(
            cfg.games.is_empty(),
            "discovering a manager is not assigning it: {:?}",
            cfg.games
        );

        // Already known, by any route: nothing is touched, so `discover` is
        // safe to run again after a `register` or an `assign`.
        assert!(!cfg
            .discover_manager(
                "hd2arsenal",
                Manager {
                    exec: "/different".into(),
                    args: vec![],
                    pipe: None
                }
            )
            .unwrap());
        assert_eq!(cfg.managers["hd2arsenal"].exec, "/a");
    }

    /// A manager that did not survive validation must not stay in `discovered`.
    /// A stale id there is silent: the user re-registers the manager by hand
    /// under the same name, and it routes nothing, with nothing saying why.
    #[test]
    fn a_stale_discovered_id_is_dropped_on_load() {
        let dir = tmp("stale-discovered");
        // `discovered` must come **before** any table header. Written after one
        // it becomes a key of that table, is ignored as unknown, and the
        // assertion below passes against an empty set that was never populated -
        // which is exactly what happened the first time this was written.
        fs::write(
            dir.join(CONFIG_FILE),
            "discovered = [\"ghost\"]\n\n[managers.real]\nexec = \"/bin/true\"\n",
        )
        .unwrap();
        let loaded = Config::load(&dir);
        assert!(
            loaded.warnings.is_empty(),
            "the fixture must be a valid config, not an unreadable one: {:?}",
            loaded.warnings
        );
        assert!(
            loaded.config.managers.contains_key("real"),
            "precondition: a real manager was loaded"
        );
        assert!(
            loaded.config.discovered.is_empty(),
            "an id with no manager survived: {:?}",
            loaded.config.discovered
        );
    }

    #[test]
    fn register_manager_refuses_an_empty_exec() {
        let mut cfg = Config::default();
        assert!(cfg
            .register_manager(
                "x",
                Manager {
                    exec: " ".into(),
                    args: vec![],
                    pipe: None
                }
            )
            .is_err());
        assert!(cfg
            .register_manager(
                " ",
                Manager {
                    exec: "/bin/true".into(),
                    args: vec![],
                    pipe: None
                }
            )
            .is_err());
    }
}
