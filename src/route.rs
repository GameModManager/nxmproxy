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

use crate::config::{substitute_url, Config, Manager, CATCH_ALL};
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
    /// Every manager we know of was found by discovery, and the user has not
    /// assigned one. Guessing here is the same misroute as above, wearing a
    /// friendlier hat.
    Unassigned(Vec<String>),
}

impl Reason {
    /// The fact, plus the one command that changes it.
    ///
    /// One remedy line, not three: the `_` catch-all and the "what should
    /// happen instead" commentary are in `assign --help`, and a user reading
    /// this has a browser waiting on them.
    pub fn message(&self, domain: &str) -> String {
        match self {
            Reason::NoManagers => format!(
                "no mod manager is registered, so nothing can forward {domain:?}.\n  \
                 nxmproxy register <name> <exec> [<args>...]"
            ),
            Reason::Ambiguous(ids) => format!(
                "{domain:?} is assigned to none of the {} registered mod managers ({}).\n  \
                 nxmproxy assign <name> {domain}",
                ids.len(),
                ids.join(", ")
            ),
            Reason::Unassigned(ids) => format!(
                "the {} known mod manager(s) ({}) were all found by discovery and none is chosen, \
                 so {domain:?} is not forwarded.\n  nxmproxy assign <name> {domain}",
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
    /// The manager this link goes to, if any.
    ///
    /// So the log can name the manager next to the argv it was given, instead
    /// of the argv alone: "the link went somewhere" and "the link went to the
    /// thing you registered" are different claims.
    pub fn manager_id(&self) -> Option<&str> {
        match self {
            Route::Manager { manager_id, .. } => Some(manager_id),
            Route::Unhandled { .. } => None,
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
/// 3. **the only manager the user actually chose**, if there is exactly one.
///
/// Step 3 is why an empty configuration works out of the box: a user with one
/// mod manager and no config at all still gets their links, which is what
/// upstream could not do. It deliberately does not fire when two managers are
/// registered, because with two there is no single obvious owner and a wrong
/// guess is a link in someone else's mod queue.
///
/// **Step 3 counts only managers the user chose.** One that discovery merely
/// found is not a choice, and letting it win by default would be the exact
/// failure the rest of this function exists to prevent - discovered by the back
/// door, through a shortcut that reads as "nothing is configured, so anything
/// goes". An assignment to a discovered manager does route it; that is a user
/// typing `assign`, and it is the whole point of remembering the manager.
pub fn resolve(config: &Config, domain: &str) -> Route {
    let domain = domain.trim().to_ascii_lowercase();

    let by_assignment = config
        .games
        .get(&domain)
        .or_else(|| config.games.get(CATCH_ALL))
        .and_then(|id| config.managers.get(id).map(|manager| (id, manager)));

    let chosen: Vec<(&String, &Manager)> = config
        .managers
        .iter()
        .filter(|(id, _)| !config.discovered.contains(*id))
        .collect();

    let (manager_id, manager) = match by_assignment {
        Some(found) => found,
        None if chosen.len() == 1 => {
            // Safe: exactly one manager the user chose, so there is nothing to
            // choose between.
            chosen[0]
        }
        None if config.managers.is_empty() => {
            return Route::Unhandled {
                domain,
                reason: Reason::NoManagers,
            }
        }
        None if chosen.is_empty() => {
            return Route::Unhandled {
                domain,
                reason: Reason::Unassigned(config.managers.keys().cloned().collect()),
            }
        }
        None => {
            return Route::Unhandled {
                domain,
                reason: Reason::Ambiguous(chosen.into_iter().map(|(id, _)| id.clone()).collect()),
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
/// `%u` and `%U` mean the same thing as `%1`; see
/// [`crate::config::substitute_url`].
pub fn spawn_args(manager: &Manager, url: &str) -> (String, Vec<String>) {
    let args = manager
        .args
        .iter()
        .map(|arg| substitute_url(arg, url))
        .collect();
    (manager.exec.clone(), args)
}

/// Deliver the link.
///
/// Tries the manager's listening address first if it has one (a running
/// manager handles it faster than a cold start), then falls back to spawning.
///
/// The fallback is unconditional and is what makes the attempt safe to make on
/// every single link: the OS runs us once per link, so a failed delivery has
/// to be both cheap and ordinary, and it has to happen. Every failure of the
/// fast path is an error rather than a block, which is the property a FIFO
/// cannot offer - see [`crate::pipe`].
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
        logger::info(format!(
            "pipe manager={manager_id:?} pipe={pipe_name:?} result=trying"
        ));
        if pipe::try_write_pipe(pipe_name, url) {
            logger::info(format!(
                "pipe manager={manager_id:?} pipe={pipe_name:?} result=delivered"
            ));
            return Ok(());
        }
        // Not necessarily a problem: the manager is probably not running yet.
        logger::info(format!(
            "pipe manager={manager_id:?} pipe={pipe_name:?} result=unavailable action=spawning"
        ));
    }

    let (exec, args) = spawn_args(manager, url);

    // THE dispatch record. Manager, exact argv, and whether the process
    // actually started - or the OS's own words on why it did not.
    //
    // The failure this exists for is silent: a link reaches nxmproxy, the
    // manager is missing or not executable, `spawn` returns an error, we exit
    // 1 into a terminal nobody is watching, and the user sees a browser click
    // that did nothing. Everything else about a working link is invisible too.
    // `argv` is Rust's debug form so it stays on one line and a url containing
    // a space or a quote cannot break the record into two.
    let record = format!("spawn manager={manager_id:?} exec={exec:?} argv={args:?}");
    match Command::new(&exec)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(_) => {
            logger::info(format!("{record} result=started"));
            Ok(())
        }
        Err(e) => {
            logger::error(format!("{record} result=failed err={e:?}"));
            // Deliberately dropped on success: we do not wait, and must not.
            Err(format!(
                "could not start mod manager {manager_id:?} ({exec}): {e}"
            ))
        }
    }
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

    /// The message an unrouted domain would actually print, so the assertion is
    /// on what the user sees rather than on the enum alone.
    fn ids_message(config: &Config, domain: &str) -> String {
        match resolve(config, domain) {
            Route::Unhandled { reason, .. } => reason.message(domain),
            Route::Manager { .. } => String::new(),
        }
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

    /// The whole point of remembering a discovered manager: knowing one exists
    /// is not choosing one.
    ///
    /// Rule 3 says one registered manager owns every unassigned domain, and a
    /// discovered manager **is** registered. Without the `discovered` exclusion
    /// this file would auto-route every link on the machine into an application
    /// the user never picked, through the one rule that was written to be safe
    /// when a user had exactly one manager and nothing to choose from.
    #[test]
    fn a_discovered_manager_never_owns_a_game_until_it_is_assigned() {
        let mut config = Config::default();
        assert!(
            config
                .discover_manager("hd2arsenal", mgr("/usr/bin/hd2arsenal", &["%1"]))
                .unwrap(),
            "precondition: it was remembered"
        );

        match resolve(&config, "helldivers2") {
            Route::Unhandled {
                reason: Reason::Unassigned(ids),
                ..
            } => {
                assert_eq!(ids, vec!["hd2arsenal".to_string()]);
                let message = ids_message(&config, "helldivers2");
                assert!(message.contains("assign"), "{message}");
                assert!(
                    message.contains("found by discovery"),
                    "the message must say why it refuses: {message}"
                );
            }
            other => panic!("discovery auto-assigned a game: {other:?}"),
        }

        // Assign it and it routes, which is what makes remembering one useful:
        // the user picks, and then it is live.
        config.assign("hd2arsenal", "helldivers2").unwrap();
        assert!(resolve(&config, "helldivers2").is_handled());
    }

    /// A discovered manager is also invisible to the ambiguity count, in both
    /// directions: it cannot turn a working one-manager install into a refusal,
    /// and it cannot join the list of choices a user is asked to pick between.
    ///
    /// **Two configurations, not one, because they reach different lines.**
    /// With a single manager the user chose, `resolve` returns from the
    /// `chosen.len() == 1` arm and never builds an `Ambiguous` list at all - so
    /// a test written only in that shape proves nothing about that list. The
    /// first assertion was written that way and its mutation control came back
    /// **green**: the line it was meant to defend does not execute. The second
    /// configuration is the one that reaches the list.
    #[test]
    fn a_discovered_manager_is_never_one_of_the_choices_offered() {
        // One the user chose, one merely found: the chosen one still routes.
        let mut config = with_one_manager();
        config
            .discover_manager("hd2arsenal", mgr("/usr/bin/hd2arsenal", &["%1"]))
            .unwrap();
        match resolve(&config, "skyrimse") {
            Route::Manager { manager_id, .. } => assert_eq!(manager_id, "vortex"),
            other => panic!("the one manager the user chose must still route: {other:?}"),
        }

        // Two the user chose, one merely found: genuinely ambiguous, and the
        // refusal must name only the two the user registered. Naming
        // `hd2arsenal` there would ask the user to choose between applications
        // they never picked, which is the question discovery exists to avoid
        // asking rather than to ask badly.
        let mut two = Config::default();
        two.register_manager("vortex", mgr("/usr/bin/vortex", &["%1"]))
            .unwrap();
        two.register_manager("mo2", mgr("/usr/bin/mo2", &["%1"]))
            .unwrap();
        two.discover_manager("hd2arsenal", mgr("/usr/bin/hd2arsenal", &["%1"]))
            .unwrap();

        match resolve(&two, "skyrimse") {
            Route::Unhandled {
                reason: Reason::Ambiguous(ids),
                ..
            } => assert_eq!(
                ids,
                vec!["mo2".to_string(), "vortex".to_string()],
                "the refusal offered a manager the user never chose"
            ),
            other => panic!("two chosen managers must still be ambiguous: {other:?}"),
        }
    }

    /// Unregistering forgets both halves. A stale id left in `discovered` would
    /// silently downgrade a manager the user re-registers by hand into one that
    /// routes nothing.
    #[test]
    fn a_removed_manager_is_no_longer_remembered_as_discovered() {
        let mut config = Config::default();
        config
            .discover_manager("hd2arsenal", mgr("/usr/bin/hd2arsenal", &["%1"]))
            .unwrap();
        assert!(config.unregister_manager("hd2arsenal").unwrap());
        assert!(config.discovered.is_empty(), "{:?}", config.discovered);
        config
            .register_manager("hd2arsenal", mgr("/usr/bin/hd2arsenal", &["%1"]))
            .unwrap();
        match resolve(&config, "helldivers2") {
            Route::Manager { manager_id, .. } => assert_eq!(manager_id, "hd2arsenal"),
            other => panic!("a hand-registered manager must route: {other:?}"),
        }
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
