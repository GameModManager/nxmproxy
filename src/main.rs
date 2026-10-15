//! nxmproxy - forward `nxm://` links to the mod manager assigned for that game.
//!
//! The OS launches this binary once per link. It resolves the link to a mod
//! manager, spawns it with the link as one argument, and exits without waiting.
//! There is no daemon, no resident process and no service: the scheme
//! registration is the only thing that persists.
//!
//! Provenance: a fork of https://github.com/TanninOne/nxmproxy (GPL-3.0),
//! MO2's author's original Windows-only router. See README "Divergences from
//! upstream" for what was kept, what was fixed, and what was removed.

mod config;
mod defaults;
mod logger;
mod pipe;
mod platform;
mod route;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use config::{Config, Manager};
use platform::Status;
use route::Route;

/// Bumped when the `list --json` shape changes, so a GUI can refuse a format
/// it does not understand instead of misreading it.
const LIST_SCHEMA_VERSION: u32 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "nxmproxy",
    version,
    about = "Forward Nexus Mods nxm:// links to the mod manager assigned for that game.",
    long_about = "Forward Nexus Mods nxm:// links to the mod manager assigned for that game.\n\n\
                  Register with `nxmproxy install` and then assign games with\n\
                  `nxmproxy assign <manager> <domain>`. Run `nxmproxy list` to see the routing table."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Forward an nxm:// link to the mod manager assigned for its game.
    Url {
        /// The nxm:// link to forward.
        url: String,
    },

    /// Register nxmproxy as a handler for nxm:// links.
    ///
    /// This makes nxmproxy eligible. It does not make it the default: no
    /// default association is written on any platform, so the OS still lets
    /// you choose and other mod managers keep their claim.
    Install,

    /// Remove the nxm:// registration. Only what `install` created is removed.
    Uninstall,

    /// Report whether nxmproxy is registered, and who the OS says handles nxm://.
    Test,

    /// Show the routing table. `--json` is the machine-readable contract.
    List {
        /// Emit JSON instead of a human-readable table.
        #[arg(long)]
        json: bool,
    },

    /// Register a mod manager so games can be assigned to it.
    #[command(visible_alias = "register")]
    RegisterManager {
        /// Name to refer to this manager by.
        name: String,
        /// The executable, a path or a bare command name found on PATH.
        exec: String,
        /// Arguments after `--`. Put `%1` where the link should go.
        #[arg(last = true)]
        args: Vec<String>,
    },

    /// Remove a mod manager and every game assignment pointing at it.
    #[command(visible_alias = "deregister")]
    UnregisterManager {
        /// Name of the manager.
        name: String,
    },

    /// Assign a game to a mod manager.
    Assign {
        /// Name of a registered manager.
        manager: String,
        /// The nexusmods.com domain, or `_` for every game with no assignment.
        game: String,
    },

    /// Give a manager a named pipe to try first, before spawning it.
    ///
    /// Only useful when the manager is already running and listening; if the
    /// write fails nxmproxy falls back to spawning the command.
    Pipe {
        /// Name of a registered manager.
        manager: String,
        /// Pipe name only, not a path. Windows named pipe path.
        pipe: String,
    },
}

/// Find the executable for a manager: a path, or a bare name on `PATH`.
///
/// For the GUI, which needs to show a broken entry rather than let the user
/// discover it when a link silently fails.
fn resolve_exec(exec: &str) -> Option<PathBuf> {
    let as_path = PathBuf::from(exec);
    if as_path.components().count() > 1 {
        return as_path.is_file().then_some(as_path);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(exec))
            .find(|candidate| candidate.is_file())
    })
}

#[derive(serde::Serialize)]
struct JsonManager<'a> {
    id: &'a str,
    exec: &'a str,
    args: &'a [String],
    pipe: Option<&'a str>,
    /// False when the manager's arguments contain no `%1`, so it would be
    /// launched and given nothing.
    takes_url: bool,
    /// False when the executable cannot be found on this machine.
    exec_exists: bool,
}

#[derive(serde::Serialize)]
struct JsonAssignment<'a> {
    domain: &'a str,
    manager: &'a str,
}

#[derive(serde::Serialize)]
struct JsonKnownManager {
    id: &'static str,
    name: &'static str,
    exec: &'static str,
    args: &'static [&'static str],
    note: &'static str,
}

#[derive(serde::Serialize)]
struct JsonList<'a> {
    version: u32,
    config_path: String,
    /// False when the config could not be parsed. The GUI must not offer to
    /// save in that state: nxmproxy is refusing to write so a single bad byte
    /// cannot wipe the user's assignments.
    config_writable: bool,
    managers: Vec<JsonManager<'a>>,
    assignments: Vec<JsonAssignment<'a>>,
    known_managers: &'static [JsonKnownManager],
    warnings: &'a [String],
}

/// The curated list, pre-rendered. `OnceLock` because it is built once and
/// serialised on every `list --json`.
fn known_managers_json() -> &'static [JsonKnownManager] {
    use std::sync::OnceLock;
    static CACHE: OnceLock<Vec<JsonKnownManager>> = OnceLock::new();
    CACHE.get_or_init(|| {
        defaults::KNOWN
            .iter()
            .map(|k| JsonKnownManager {
                id: k.id,
                name: k.name,
                exec: k.exec,
                args: k.args,
                note: k.note,
            })
            .collect()
    })
}

fn list_json(config: &Config, loaded: &config::Loaded, dir: &Path) -> String {
    let payload = JsonList {
        version: LIST_SCHEMA_VERSION,
        config_path: dir.join(config::CONFIG_FILE).to_string_lossy().into_owned(),
        config_writable: loaded.writable,
        managers: config
            .managers
            .iter()
            .map(|(id, manager)| JsonManager {
                id,
                exec: &manager.exec,
                args: &manager.args,
                pipe: manager.pipe.as_deref(),
                takes_url: manager.takes_url(),
                exec_exists: resolve_exec(&manager.exec).is_some(),
            })
            .collect(),
        assignments: config
            .games
            .iter()
            .map(|(domain, manager)| JsonAssignment { domain, manager })
            .collect(),
        known_managers: known_managers_json(),
        warnings: &loaded.warnings,
    };
    // BTreeMaps plus declared field order make this byte stable across runs.
    serde_json::to_string_pretty(&payload).expect("the list payload is all owned, printable data")
}

fn print_list(config: &Config, loaded: &config::Loaded, dir: &Path) {
    println!("config: {}", dir.join(config::CONFIG_FILE).display());
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }

    if config.managers.is_empty() {
        println!("\nno mod managers registered.");
        println!("  nxmproxy register-manager vortex /path/to/vortex.exe -- %1");
        println!("  see `nxmproxy list --json` for a curated list of known managers");
        return;
    }

    println!("\nmod managers:");
    for (id, manager) in &config.managers {
        let mut notes: Vec<String> = Vec::new();
        if !manager.takes_url() {
            notes.push("no %1 in its arguments, it would receive no link".to_string());
        }
        if resolve_exec(&manager.exec).is_none() {
            notes.push("executable not found on this machine".to_string());
        }
        if let Some(pipe) = &manager.pipe {
            notes.push(format!("pipe {pipe}"));
        }
        let suffix = if notes.is_empty() {
            String::new()
        } else {
            format!("  ({})", notes.join("; "))
        };
        println!(
            "  {id}: {} {}{suffix}",
            manager.exec,
            manager.args.join(" ")
        );
    }

    println!("\ngames:");
    if config.games.is_empty() && config.managers.len() > 1 {
        println!("  none - with more than one manager every game needs an assignment,");
        println!("  otherwise there is no correct answer and guessing would misroute the link");
    } else if config.games.is_empty() {
        println!("  none - every game goes to the only registered manager");
    } else {
        for (domain, manager) in &config.games {
            let label = if domain == config::CATCH_ALL {
                "_ (every unassigned game)"
            } else {
                domain.as_str()
            };
            println!("  {label} -> {manager}");
        }
    }
}

/// Forward one link. Exit code is the answer: 0 delivered, 1 not.
fn handle_url(config: &Config, url: &str) -> i32 {
    let domain = match route::domain_of(url) {
        Ok(domain) => domain,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            logger::error(&message);
            return 1;
        }
    };

    let route = route::resolve(config, &domain);
    if let Route::Unhandled { domain, reason } = &route {
        let message = format!("nxmproxy: {}", reason.message(domain));
        eprintln!("{message}");
        logger::error(&message);
        return 1;
    }

    logger::info(format!("routing {url} -> {}", route.domain()));
    match route::deliver(&route, url) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            logger::error(&message);
            1
        }
    }
}

fn handle_test() -> i32 {
    let status = Status {
        registered: match platform::imp::is_registered() {
            Ok(registered) => registered,
            Err(e) => {
                eprintln!("nxmproxy: {e}");
                return 1;
            }
        },
        handler: platform::imp::current_handler().unwrap_or(None),
        caveat: if cfg!(target_os = "macos") {
            Some("the default handler cannot be read without LSCopyDefaultHandlerForURLScheme, \
                  which this fork does not bind; check System Settings > General > Login Items & Extensions"
                .to_string())
        } else {
            None
        },
    };

    println!("registered: {}", status.registered);
    match &status.handler {
        Some(handler) => println!("nxm:// handler: {handler}"),
        None => println!("nxm:// handler: (none - the OS will ask you which app to use)"),
    }
    if let Some(caveat) = &status.caveat {
        println!("note: {caveat}");
    }
    // Upstream's contract, kept: a non-zero code means "not installed".
    i32::from(!status.registered)
}

fn run() -> i32 {
    let cli = Cli::parse();

    let dir = match config::config_dir() {
        Ok(dir) => dir,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            return 1;
        }
    };

    if let Err(message) = config::ensure_dir(&dir) {
        eprintln!("nxmproxy: {message}");
        return 1;
    }
    logger::set_file(&dir.join("nxm.log"));

    let loaded = Config::load(&dir);
    let mut config = loaded.config.clone();
    for warning in &loaded.warnings {
        eprintln!("nxmproxy: warning: {warning}");
        logger::warn(warning);
    }

    // Everything that persists a change goes through here, so "the config is
    // unreadable, do not overwrite it" is one rule rather than four.
    let save = |config: &Config| -> i32 {
        if !loaded.writable {
            eprintln!(
                "nxmproxy: refusing to write: {} could not be parsed. Fix or remove it first, \
                 so this change does not overwrite it.",
                dir.join(config::CONFIG_FILE).display()
            );
            return 1;
        }
        match config.write(&dir) {
            Ok(()) => 0,
            Err(message) => {
                eprintln!("nxmproxy: {message}");
                1
            }
        }
    };

    match cli.command {
        Command::Url { url } => handle_url(&config, &url),

        Command::Install => {
            match platform::imp::install() {
                Ok(()) => {
                    println!("nxmproxy is now able to handle nxm:// links.");
                    println!("It registers, it does not seize: no default association was written, so the");
                    println!("OS will let you choose and other mod managers keep their claim.");
                    0
                }
                Err(message) => {
                    eprintln!("nxmproxy: {message}");
                    1
                }
            }
        }

        Command::Uninstall => match platform::imp::uninstall() {
            Ok(()) => {
                println!("nxmproxy's nxm:// registration was removed.");
                0
            }
            Err(message) => {
                eprintln!("nxmproxy: {message}");
                1
            }
        },

        Command::Test => handle_test(),

        Command::List { json } => {
            if json {
                println!("{}", list_json(&config, &loaded, &dir));
            } else {
                print_list(&config, &loaded, &dir);
            }
            // Warnings are reported, not fatal: the listing is still correct
            // for whatever did parse.
            0
        }

        Command::RegisterManager { name, exec, args } => {
            let manager = Manager {
                exec,
                args: args.clone(),
                pipe: None,
            };
            match config.register_manager(&name, manager) {
                Ok(()) => {}
                Err(message) => {
                    eprintln!("nxmproxy: {message}");
                    return 1;
                }
            }
            let code = save(&config);
            if code == 0 {
                println!("registered mod manager {name:?}.");
                if !config.managers[&name].takes_url() {
                    eprintln!(
                        "warning: {name:?} has no %1 in its arguments, so it will be started but \
                         given no link. Re-run with `-- %1` in the right position."
                    );
                }
                if resolve_exec(&config.managers[&name].exec).is_none() {
                    eprintln!(
                        "warning: {:?} was not found on this machine.",
                        config.managers[&name].exec
                    );
                }
            }
            code
        }

        Command::UnregisterManager { name } => match config.unregister_manager(&name) {
            Ok(true) => {
                println!("removed mod manager {name:?} and its game assignments.");
                save(&config)
            }
            Ok(false) => {
                eprintln!("nxmproxy: no mod manager named {name:?} is registered");
                1
            }
            Err(message) => {
                eprintln!("nxmproxy: {message}");
                1
            }
        },

        Command::Assign { manager, game } => {
            if let Err(message) = config.assign(&manager, &game) {
                eprintln!("nxmproxy: {message}");
                return 1;
            }
            let label = if game == config::CATCH_ALL {
                "every unassigned game"
            } else {
                game.as_str()
            };
            let code = save(&config);
            if code == 0 {
                println!("{label} -> {manager}");
            }
            code
        }

        Command::Pipe { manager, pipe } => {
            if let Err(message) = config.set_pipe(&manager, &pipe) {
                eprintln!("nxmproxy: {message}");
                return 1;
            }
            let code = save(&config);
            if code == 0 {
                println!("manager {manager:?} will try the pipe {pipe:?} before being spawned.");
            }
            code
        }
    }
}

fn main() -> ExitCode {
    // Upstream returned the code from main. Same here, except that a failing
    // run is a number rather than a panic message.
    let code = run();
    ExitCode::from(code as u8)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn a_bare_exec_resolves_only_when_it_is_really_there() {
        // /bin/sh exists here; a made up name must not resolve. The GUI relies
        // on this to flag a broken entry before a link fails silently.
        assert!(resolve_exec("sh").is_some());
        assert!(resolve_exec("nxmproxy-definitely-not-a-real-binary").is_none());
    }
}
