// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! The routing decision: which mod manager owns an nxm link, and with which
//! argument vector. Pure logic, no IO, no process handling.
//!
//! Upstream's decision was `config.resolve(game).expect("Failed to find
//! manager for game")`. An unknown domain was therefore a **panic**: a crash
//! dialog in the user's face, because they clicked a link for a game that is
//! not in the routing table. An unknown nxm link is ordinary traffic. Here it
//! is a value, [`Route::Unhandled`], with a reason the caller can print.

use std::process::{Command, Stdio};

use crate::config::{Config, Manager, CATCH_ALL};
use crate::logger;
use crate::pipe;

/// Why a domain has no manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Nothing is registered at all.
    NoManagers,
    /// More than one manager and no assignment for this domain. Guessing here
    /// is a misroute, so we do not.
    Ambiguous(Vec<String>),
}

impl Reason {
    /// A message that tells the user what to type next.
    pub fn message(&self, domain: &str) -> String {
        match self {
            Reason::NoManagers => format!(
                "no mod managers are registered, so there is nothing to forward the nxm link for \
                 {domain:?} to.\n  register one:  nxmproxy register <name> <exec> [<args>...]"
            ),
            Reason::Ambiguous(ids) => format!(
                "no mod manager is assigned to the nxm domain {domain:?}, and {} managers are \
                 registered ({}) so guessing would deliver the link to the wrong one.\n  assign one:  \
                 nxmproxy assign <name> {domain}\n  or send every unassigned game to one of them:  \
                 nxmproxy assign <name> _",
                ids.len(),
                ids.join(", ")
            ),
        }
    }
}

/// The outcome of routing one link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// A manager owns this domain.
    Manager {
        domain: String,
        manager_id: String,
        manager: Manager,
    },
    /// Nothing owns it. Not an error condition in the process sense, but the
    /// link is not delivered, so callers exit non-zero.
    Unhandled { domain: String, reason: Reason },
}

impl Route {
    /// The domain this route was made for, either way.
    pub fn domain(&self) -> &str {
        match self {
            Route::Manager { domain, .. } => domain,
            Route::Unhandled { domain, .. } => domain,
        }
    }

    /// True when a manager owns this link. A helper for tests: the callers that
    /// act on a route match on [`Route::Manager`] instead, because they also
    /// need to name the manager.
    #[cfg(test)]
    pub fn is_handled(&self) -> bool {
        matches!(self, Route::Manager { .. })
    }
}

/// Decide the manager for `domain`, in this order:
///
/// 1. an exact assignment for the domain;
/// 2. the `_` catch-all;
/// 3. **the only registered manager**, if there is exactly one.
///
/// Step 3 is why an empty configuration works out of the box: a user with one
/// mod manager and no config at all still gets their links, which is what
/// upstream could not do. It deliberately does not fire when two managers are
/// registered, because with two there is no single obvious owner and a wrong
/// guess is a link in someone else's mod queue.
pub fn resolve(config: &Config, domain: &str) -> Route {
    let domain = domain.trim().to_ascii_lowercase();

    let by_assignment = config
        .games
        .get(&domain)
        .or_else(|| config.games.get(CATCH_ALL))
        .and_then(|id| config.managers.get(id).map(|manager| (id, manager)));

    let (manager_id, manager) = match by_assignment {
        Some(found) => found,
        None if config.managers.len() == 1 => {
            // Safe: exactly one manager, so there is nothing to choose between.
            let (id, manager) = config.managers.iter().next().unwrap();
            (id, manager)
        }
        None if config.managers.is_empty() => {
            return Route::Unhandled {
                domain,
                reason: Reason::NoManagers,
            }
        }
        None => {
            return Route::Unhandled {
                domain,
                reason: Reason::Ambiguous(config.managers.keys().cloned().collect()),
            }
        }
    };

    Route::Manager {
        domain,
        manager_id: manager_id.clone(),
        manager: manager.clone(),
    }
}

/// Split the host out of an `nxm://` url.
///
/// Returns `Err` for anything that is not an nxm url with a host. The url
/// arrives from the browser or the OS, so it is untrusted input and is
/// validated here rather than `expect`ed, as upstream did.
pub fn domain_of(url: &str) -> Result<String, String> {
    let parsed = url::Url::parse(url).map_err(|e| format!("not a url: {url:?} ({e})"))?;
    if parsed.scheme() != "nxm" {
        return Err(format!("not an nxm url: {url:?}"));
    }
    parsed
        .host_str()
        .filter(|h| !h.is_empty())
        .map(|h| h.to_ascii_lowercase())
        .ok_or_else(|| format!("nxm url has no game domain: {url:?}"))
}

/// Substitute the url into the manager's argument vector.
///
/// The field code is replaced **inside one element**, never by splitting a
/// string. Upstream did `command.replace("%1", url)` and then split the
/// resulting string with the Win32 parser, so a url containing a double quote
/// produced a different and wrong argument vector. Holding the command as a
/// vector and touching a single element cannot corrupt it.
///
/// `%u` and `%U` mean the same thing as `%1`; see [`config::substitute_url`].
pub fn spawn_args(manager: &Manager, url: &str) -> (String, Vec<String>) {
    let args = manager
        .args
        .iter()
        .map(|arg| crate::config::substitute_url(arg, url))
        .collect();
    (manager.exec.clone(), args)
}

/// Deliver the link.
///
/// Tries the manager's named pipe first if it has one (a running manager
/// handles it faster than a cold start), then falls back to spawning.
///
/// The child is spawned with all three standard streams nulled and is **never
/// waited on**: the OS launched us for one uri, and we return as soon as it
/// exists. Nothing here keeps a process resident.
pub fn deliver(route: &Route, url: &str) -> Result<(), String> {
    let (manager_id, manager) = match route {
        Route::Unhandled { .. } => return Err("refusing to deliver an unhandled link".to_string()),
        Route::Manager {
            manager_id,
            manager,
            ..
        } => (manager_id, manager),
    };

    if let Some(pipe_name) = manager.pipe.as_deref() {
        logger::info(format!("trying pipe {pipe_name} for manager {manager_id}"));
        if pipe::try_write_pipe(pipe_name, url) {
            logger::info(format!("delivered over pipe {pipe_name}"));
            return Ok(());
        }
        // Not necessarily a problem: the manager is probably not running yet.
        logger::info(format!(
            "pipe {pipe_name} write failed, falling back to spawning"
        ));
    }

    let (exec, args) = spawn_args(manager, url);
    logger::info(format!("spawning manager {manager_id}: {exec} {args:?}"));

    Command::new(&exec)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ()) // deliberately dropped: we do not wait, and must not.
        .map_err(|e| format!("could not start mod manager {manager_id:?} ({exec}): {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mgr(exec: &str, args: &[&str]) -> Manager {
        Manager {
            exec: exec.to_string(),
            args: args.iter().map(|s| s.to_string()).collect(),
            pipe: None,
        }
    }

    fn with_one_manager() -> Config {
        let mut c = Config::default();
        c.register_manager("vortex", mgr("/usr/bin/vortex", &["%1"]))
            .unwrap();
        c
    }

    #[test]
    fn an_assigned_domain_routes_to_its_manager() {
        let mut config = with_one_manager();
        config.assign("vortex", "skyrimse").unwrap();
        let route = resolve(&config, "skyrimse");
        assert!(route.is_handled());
        match &route {
            Route::Manager { manager_id, .. } => assert_eq!(manager_id, "vortex"),
            other => panic!("expected a manager, got {other:?}"),
        }
    }

    #[test]
    fn an_unrouted_domain_is_a_value_not_a_panic() {
        // Upstream: `config.resolve(game).expect("Failed to find manager for
        // game")`. Clicking a link for an unconfigured game therefore produced
        // a panic and a crash dialog, when an nxm link nobody has configured is
        // ordinary traffic. Both ways of being unroutable must come back as
        // data, with a message that names the domain and the next command.
        let none = resolve(&Config::default(), "a-game-nobody-configured");
        let mut config = with_one_manager();
        config
            .register_manager("mo2", mgr("/usr/bin/mo2", &["%1"]))
            .unwrap();
        let ambiguous = resolve(&config, "a-game-nobody-configured");

        for route in [&none, &ambiguous] {
            match route {
                Route::Unhandled { domain, reason } => {
                    assert_eq!(domain, "a-game-nobody-configured");
                    let message = reason.message(domain);
                    assert!(message.contains("a-game-nobody-configured"), "{message}");
                    assert!(
                        message.contains("nxmproxy register ")
                            || message.contains("nxmproxy assign"),
                        "the message must say what to do next: {message}"
                    );
                }
                other => panic!("an unrouted domain must not be delivered: {other:?}"),
            }
        }
    }

    #[test]
    fn a_known_domain_with_two_managers_and_no_assignment_is_ambiguous_not_guessed() {
        let mut config = with_one_manager();
        config
            .register_manager("mo2", mgr("/usr/bin/mo2", &["%1"]))
            .unwrap();
        match resolve(&config, "helldivers2") {
            Route::Unhandled {
                reason: Reason::Ambiguous(ids),
                ..
            } => {
                assert_eq!(ids, vec!["mo2".to_string(), "vortex".to_string()]);
            }
            other => panic!("must not guess between two managers: {other:?}"),
        }
    }

    #[test]
    fn an_empty_config_with_one_manager_still_delivers() {
        // Upstream could not do this: it resolved through games/_ only.
        let config = with_one_manager();
        assert!(config.games.is_empty(), "precondition: nothing assigned");
        assert!(resolve(&config, "anything").is_handled());
    }

    #[test]
    fn an_empty_config_with_no_managers_reports_how_to_register_one() {
        match resolve(&Config::default(), "skyrimse") {
            Route::Unhandled {
                reason: Reason::NoManagers,
                ..
            } => {}
            other => panic!("{other:?}"),
        }
        let message = Reason::NoManagers.message("skyrimse");
        assert!(message.contains("register"), "{message}");
    }

    #[test]
    fn the_catch_all_covers_domains_with_no_assignment() {
        let mut config = with_one_manager();
        config.assign("vortex", CATCH_ALL).unwrap();
        config
            .register_manager("mo2", mgr("/usr/bin/mo2", &["%1"]))
            .unwrap();
        let route = resolve(&config, "helldivers2");
        match &route {
            Route::Manager { manager_id, .. } => assert_eq!(manager_id, "vortex"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_exact_assignment_beats_the_catch_all() {
        let mut config = with_one_manager();
        config.assign("vortex", CATCH_ALL).unwrap();
        config
            .register_manager("arsenal", mgr("/usr/bin/arsenal", &["%1"]))
            .unwrap();
        config.assign("arsenal", "helldivers2").unwrap();
        match &resolve(&config, "helldivers2") {
            Route::Manager { manager_id, .. } => assert_eq!(manager_id, "arsenal"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn domain_matching_is_case_insensitive() {
        let mut config = with_one_manager();
        config.assign("vortex", "skyrimse").unwrap();
        assert!(resolve(&config, "SkyrimSE").is_handled());
    }

    #[test]
    fn a_url_containing_a_quote_survives_substitution_intact() {
        // Upstream: command.replace("%1", url) then CommandLineToArgvW on the
        // result, so this url silently became several arguments.
        let url = r#"nxm://somegame/mods/1/files/2?a="b"&c='d' e"#;
        let manager = mgr("C:\\Program Files\\vortex.exe", &["--flag", "%1"]);
        let (exec, args) = spawn_args(&manager, url);
        assert_eq!(exec, "C:\\Program Files\\vortex.exe");
        assert_eq!(
            args.len(),
            2,
            "the url must stay exactly one argument: {args:?}"
        );
        assert_eq!(args[0], "--flag");
        assert_eq!(args[1], url, "the url must arrive byte for byte");
    }

    #[test]
    fn substitution_touches_every_element_that_asks_for_it() {
        let manager = mgr("m", &["%1", "--also", "%1"]);
        let (_, args) = spawn_args(&manager, "nxm://g/mods/1");
        assert_eq!(args, vec!["nxm://g/mods/1", "--also", "nxm://g/mods/1"]);
    }

    #[test]
    fn the_desktop_entry_spellings_of_the_field_code_all_substitute() {
        // `%u` and `%U` are what an Electron, AppImage or Flatpak entry
        // writes, so they are what a user copies into `register`. Before this,
        // only `%1` substituted: the manager started, and the link went
        // nowhere, silently.
        for code in ["%1", "%u", "%U"] {
            let (_, args) = spawn_args(&mgr("m", &[code]), "nxm://g/mods/1");
            assert_eq!(args, vec!["nxm://g/mods/1"], "{code} did not substitute");
            // And the manager is not reported as one that would never see a
            // link: `takes_url` is the contract a GUI warns the user with.
            assert!(mgr("m", &[code]).takes_url(), "{code} reads as no link");
        }
        // Mid-string too, the form `npm run start -- %U` style entries use.
        let (_, args) = spawn_args(&mgr("m", &["--open=%U"]), "nxm://g/mods/1");
        assert_eq!(args, vec!["--open=nxm://g/mods/1"]);
        // A bare `%` that is not a field code is left alone rather than
        // swallowed: `--progress 50%` is not a link.
        let (_, args) = spawn_args(&mgr("m", &["%"]), "nxm://g/mods/1");
        assert_eq!(args, vec!["%"]);
        assert!(!mgr("m", &["50%"]).takes_url());
    }

    #[test]
    fn substitution_is_one_pass_so_a_url_containing_a_field_code_survives() {
        // The url is untrusted input from a browser. Replacing `%u` after
        // `%1` was already expanded would substitute into the text the url
        // itself brought with it, and hand the manager a different url.
        let url = "nxm://g/mods/1/files/2?name=50%user";
        let (_, args) = spawn_args(&mgr("m", &["%U"]), url);
        assert_eq!(args, vec![url], "the url must arrive byte for byte");
        let (_, args) = spawn_args(&mgr("m", &["%1"]), url);
        assert_eq!(args, vec![url]);
    }

    #[test]
    fn domain_of_accepts_real_links_and_rejects_the_rest() {
        assert_eq!(
            domain_of("nxm://skyrimse/mods/1/files/2").unwrap(),
            "skyrimse"
        );
        assert_eq!(domain_of("nxm://SomeGame/mods/1").unwrap(), "somegame");
        for bad in [
            "https://www.nexusmods.com/skyrimse/mods/1",
            "nxm:///mods/1",
            "not a url at all",
            "",
        ] {
            let err = domain_of(bad).unwrap_err();
            assert!(!err.is_empty(), "every rejection needs a message");
        }
    }

    #[test]
    fn takes_url_reports_whether_the_manager_will_ever_see_a_url() {
        assert!(mgr("m", &["%1"]).takes_url());
        assert!(!mgr("m", &[]).takes_url());
    }

    #[cfg(unix)]
    #[test]
    fn deliver_returns_without_waiting_for_the_child() {
        // The design property the whole thing rests on: the OS launches us per
        // uri and we go away again. If anyone changes `.spawn()` to
        // `.status()`, this blocks for the full sleep and then fails.
        let mut config = Config::default();
        config
            .register_manager("slow", mgr("sleep", &["30"]))
            .unwrap();
        let route = resolve(&config, "skyrimse");

        let started = std::time::Instant::now();
        deliver(&route, "nxm://skyrimse/mods/1/files/2").unwrap();
        let elapsed = started.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(5),
            "deliver waited {elapsed:?} on the child"
        );
    }
}
