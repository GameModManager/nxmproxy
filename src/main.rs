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
                  Take nxm:// with `nxmproxy install`, add a mod manager with\n\
                  `nxmproxy register <name> <exec> [<args>...]`, then assign games with\n\
                  `nxmproxy assign <manager> <domain>`. `nxmproxy list` shows the routing table.\n\n\
                  Exit codes:\n  \
                    0  done, and verified\n  \
                    1  the command failed\n  \
                    2  bad command line\n  \
                    3  registered, but the OS requires one step you must do by hand\n\
                       (the message says which). Nothing failed; nxmproxy did all the\n\
                       platform allows."
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

    /// Install nxmproxy as the nxm:// handler, and claim the scheme.
    ///
    /// This is both halves in one command: it publishes nxmproxy to the OS
    /// (desktop entry, app bundle, or registry ProgID) **and** asks the OS to
    /// make it the handler for `nxm://`. How hard that second half can push
    /// depends on the platform, and `install` never reports more than it did:
    ///
    /// * Linux - writes one `[Default Applications]` line with `xdg-mime`, and
    ///   `uninstall` takes that line back out.
    /// * macOS - calls LaunchServices, which on macOS 12+ asks you first.
    /// * Windows - **cannot** choose the default app; no program may. It opens
    ///   the Default Apps page for you instead and exits 3.
    ///
    /// Exit 0 only when nxmproxy really is the handler and that was checked.
    Install,

    /// Remove nxmproxy as the nxm:// handler, and undo what `install` published.
    ///
    /// Symmetric with `install`. Linux removes nxmproxy's own
    /// `[Default Applications]` line and touches no other manager's entry.
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
    ///
    /// Upstream's `register` did this too, so the name is unchanged. Upstream
    /// took a command *string*; this takes an executable and a separate
    /// argument vector, which is why a path with spaces in it works here and
    /// did not there.
    Register {
        /// Name to refer to this manager by.
        name: String,
        /// The executable, a path or a bare command name found on PATH.
        exec: String,
        /// Arguments for the manager. Put `%1`, `%u` or `%U` where the link goes.
        ///
        /// `--` before them still works and still means the same thing. It is
        /// no longer required: a user registering a manager from its own
        /// desktop entry types `Exec=` without the separator, and rejecting
        /// that with exit 2 was the whole complaint.
        #[arg(allow_hyphen_values = true)]
        args: Vec<String>,
    },

    /// Remove a mod manager and every game assignment pointing at it.
    ///
    /// Upstream spelled this `deregister`. Renamed, because it removes rather
    /// than deregisters and there is no registry left to speak of.
    Unregister {
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
///
/// On Windows a bare name is resolved through `PATHEXT`, so `vortex` finds
/// `vortex.exe`. Checking only for an exact file name reported perfectly good
/// managers as missing, which is what the `exec_exists` flag in `list --json`
/// would then tell the user.
fn resolve_exec(exec: &str) -> Option<PathBuf> {
    let as_path = PathBuf::from(exec);
    if as_path.components().count() > 1 {
        return as_path.is_file().then_some(as_path);
    }
    let paths = std::env::var_os("PATH")?;
    let extensions = pathext_extensions(std::env::var("PATHEXT").ok());

    for dir in std::env::split_paths(&paths) {
        let candidate = dir.join(exec);
        if candidate.is_file() {
            return Some(candidate);
        }
        for extension in &extensions {
            let mut name = candidate.clone().into_os_string();
            name.push(extension);
            let with_extension = PathBuf::from(name);
            if with_extension.is_file() {
                return Some(with_extension);
            }
        }
    }
    None
}

/// The `PATHEXT` extensions to try, as strings including the leading dot.
///
/// Windows' own fallback set is used when `PATHEXT` is unset, which is what
/// `CreateProcess` does. Pure, so the parsing is testable anywhere.
fn pathext_extensions(value: Option<String>) -> Vec<String> {
    let raw = value.unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    raw.split(';')
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .map(|e| {
            if e.starts_with('.') {
                e.to_string()
            } else {
                format!(".{e}")
            }
        })
        .collect()
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
    /// True when this manager is actually present on this machine. The GUI can
    /// offer it as a one-click register instead of making the user find the
    /// path, which is the only way this works for a QtIFW install that is not
    /// on `PATH`.
    present: bool,
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

/// Where a manager's XDG desktop entry would be, if it writes one.
fn desktop_entry_paths(id: &str) -> Vec<PathBuf> {
    let file = format!("{id}.desktop");
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(data_home) = std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        dirs.push(data_home);
    }
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        dirs.push(home.join(".local").join("share"));
    }
    dirs.iter()
        .map(|d| d.join("applications").join(&file))
        .collect()
}

/// Split the `Exec=` line of a desktop entry into an executable and its argv.
///
/// `%1` is what nxmproxy substitutes, because it is substituted into a single
/// argv element and is therefore never split. `%u` is the desktop entry spec's
/// code for "the URI", and GameMod Manager writes `%u` in its own entry, so the
/// two are mapped onto each other here.
///
/// Only the two quoting forms real desktop entries actually produce are handled:
/// a bare path, and a `"`-quoted path. GameMod Manager quotes its `Exec` path
/// when it contains whitespace (and `xdg-mime`'s own parser does not strip
/// quotes either, which is why it quotes), so those two are the whole of it.
/// Anything else returns `None` rather than a guess.
///
/// Pure, so the quoting is testable on every platform with no desktop anywhere.
pub fn parse_exec_line(line: &str) -> Option<(String, Vec<String>)> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let (exe, rest) = if let Some(quoted) = line.strip_prefix('"') {
        let end = quoted.find('"')?;
        (quoted[..end].to_string(), quoted[end + 1..].trim())
    } else {
        match line.find(char::is_whitespace) {
            Some(i) => (line[..i].to_string(), line[i..].trim()),
            None => (line.to_string(), ""),
        }
    };
    if exe.is_empty() {
        return None;
    }

    let args = rest
        .split_whitespace()
        .map(|a| match a {
            "%u" | "%U" | "%1" => "%1".to_string(),
            // Field codes that carry no argument. They describe a window, not
            // the argv, so passing them through would corrupt the command.
            "%f" | "%F" | "%i" | "%c" | "%k" | "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => {
                String::new()
            }
            other => other.to_string(),
        })
        .filter(|a| !a.is_empty())
        .collect();

    Some((exe, args))
}

/// The `Exec=` line from desktop entry text, if it has one.
fn exec_line_of(entry: &str) -> Option<String> {
    entry
        .lines()
        .find_map(|line| line.trim().strip_prefix("Exec=").map(str::to_string))
}

/// A manager that is actually installed here, and how to invoke it.
///
/// Two sources, in order of trust:
///
/// 1. **The manager's own XDG desktop entry**, when it has one. That file is
///    written by the manager itself and holds its real `Exec=` line, so reading
///    it requires no guess at all. This is the only thing that works for GameMod
///    Manager on Linux: QtIFW installs it to a prefix that is **not** on `PATH`,
///    so `resolve_exec("gamemodmanager")` finds nothing on a machine where GMM is
///    plainly installed, while the desktop entry exists and names the binary.
/// 2. **`PATH`**, for everything else.
///
/// A candidate is accepted only when the resulting executable really resolves.
/// Registering a manager that cannot be started would produce exactly the broken
/// install this exists to prevent, so an unresolvable candidate is a refusal
/// rather than a warning. Refusing is safe: the caller prints the command to type.
///
/// Only ever consulted when the configuration has **no** managers at all, and
/// only ever returns a single one. A user who configured something has already
/// answered this question.
pub fn detect_manager() -> Option<(&'static defaults::Known, Manager)> {
    let candidates: Vec<(&'static defaults::Known, Manager)> = defaults::KNOWN
        .iter()
        .filter_map(|known| {
            let from_entry =
                known
                    .desktop
                    .and_then(desktop_entry_invocation)
                    .filter(|(exe, args)| {
                        resolve_exec(exe).is_some() && args.iter().any(|a| a == "%1")
                    });

            let manager = match from_entry {
                Some((exe, args)) => Manager {
                    exec: exe,
                    args,
                    pipe: None,
                },
                // No usable entry: fall back to PATH, which is all a manager
                // without a desktop file can offer.
                None if resolve_exec(known.exec).is_some() => Manager {
                    exec: known.exec.to_string(),
                    args: known.args.iter().map(|a| a.to_string()).collect(),
                    pipe: None,
                },
                None => return None,
            };
            Some((known, manager))
        })
        .collect();

    // Exactly one, or none. Two managers installed is genuinely ambiguous and
    // guessing routes downloads to the wrong mod queue, which is the failure
    // `Route::Ambiguous` exists to refuse at click time. `install` must not
    // invent the very ambiguity it refuses to resolve later.
    match candidates.as_slice() {
        [(only, manager)] => Some((*only, manager.clone())),
        _ => None,
    }
}

/// The manager's own desktop entry, found and parsed into an executable and argv.
///
/// One definition of "what its entry says", shared by [`detect_manager`] and
/// [`is_present`]. They were separate for a moment and disagreed: `present` was
/// satisfied by the entry merely existing, while `install` also required the
/// executable to resolve and to receive the link. `list --json` therefore told a
/// user a manager was present that `install` then refused to register, which is
/// the kind of disagreement that makes a diagnostic useless.
fn desktop_entry_invocation(id: &str) -> Option<(String, Vec<String>)> {
    desktop_entry_paths(id)
        .into_iter()
        .find(|p| p.is_file())
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| exec_line_of(&text))
        .and_then(|line| parse_exec_line(&line))
}

/// True when `known` is installed here **and usable**, for `list --json`'s
/// `present` flag.
///
/// Same bar as [`detect_manager`], on purpose: an entry naming a binary that is
/// not there is not a manager the user has.
pub fn is_present(known: &defaults::Known) -> bool {
    if let Some((exe, args)) = known.desktop.and_then(desktop_entry_invocation) {
        if resolve_exec(&exe).is_some() && args.iter().any(|a| a == "%1") {
            return true;
        }
    }
    resolve_exec(known.exec).is_some()
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
                present: is_present(k),
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
        println!("  nxmproxy register vortex /path/to/vortex.exe -- %1");
        println!("  see `nxmproxy list --json` for a curated list of known managers");
        return;
    }

    println!("\nmod managers:");
    for (id, manager) in &config.managers {
        let mut notes: Vec<String> = Vec::new();
        if !manager.takes_url() {
            notes.push("no %1, %u or %U in its arguments, it would receive no link".to_string());
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

/// Compose `install`: publish nxmproxy, claim `nxm://`, and make sure a link
/// has somewhere to go.
///
/// Three independent outcomes, and the exit code is the *worst* of them.
/// Anything less would report a working install over a setup that routes
/// nothing, which is the failure this whole command exists to avoid.
fn handle_install(config: &mut Config, save: &dyn Fn(&Config) -> i32) -> i32 {
    // 1. Publish ourselves to the OS. This is what `install` has always done,
    //    and the only step Windows can do by itself.
    if let Err(message) = platform::imp::install() {
        eprintln!("nxmproxy: {message}");
        return platform::EXIT_FAILED;
    }

    // 2. Take the scheme. Cannot be done on every platform, and never reported
    //    as done unless it was checked here.
    let claim = match platform::imp::claim() {
        Ok(claim) => claim,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            return platform::EXIT_FAILED;
        }
    };

    // 3. A handler that delivers every link to a crash is not installed.
    let mut code = claim.exit_code();
    let already_configured = !config.managers.is_empty();
    let mut routing_ready = already_configured;
    if already_configured {
        println!("Keeping the mod managers already in your configuration.");
    } else {
        match detect_manager() {
            Some((known, manager)) => match config.register_manager(known.id, manager) {
                Ok(()) => {
                    routing_ready = true;
                    let saved = save(config);
                    if saved != platform::EXIT_OK {
                        code = saved;
                    }
                    println!(
                        "Found {} on this machine and registered it as {}. Every game routes to \
                         it until you `assign` something else.",
                        known.name, known.id
                    );
                }
                Err(message) => {
                    // Not fatal, and not a claim failure either: the scheme is
                    // claimed and the user can type the command by hand.
                    eprintln!("nxmproxy: could not pre-register {}: {message}", known.name);
                }
            },
            None => {
                println!(
                    "\nnxmproxy handles nxm://, but there is no mod manager to forward to, so a\n\
                     link would arrive and go nowhere. Add one now:\n\n    \
                     nxmproxy register gamemodmanager gamemodmanager -- --handle-nxm %1\n\n\
                     `nxmproxy list --json` lists the managers it can find on this machine."
                );
            }
        }
    }

    // Claiming the scheme and having somewhere to forward it are two different
    // things, and a script that only checks the exit code needs to know about
    // both. Exit 3 already means "registered, one step left for you", and
    // running `register` is exactly such a step.
    if !routing_ready && code == platform::EXIT_OK {
        code = platform::EXIT_NEEDS_USER;
    }

    if claim.claimed {
        println!("nxm:// links now open in nxmproxy.");
    } else {
        println!("nxmproxy is registered as an nxm:// handler.");
        println!("One step is left for you to do:");
        println!("{}", claim.instruction);
    }
    code
}

/// Compose `uninstall`: give the scheme back, then remove what we published.
///
/// Reverses [`handle_install`] in the same order, and is equally honest about
/// the half the OS will not give back: Linux removes its line, and macOS and
/// Windows both report that the final assignment is the user's click.
fn handle_uninstall() -> i32 {
    let claim = match platform::imp::unclaim() {
        Ok(claim) => claim,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            return platform::EXIT_FAILED;
        }
    };
    if let Err(message) = platform::imp::uninstall() {
        eprintln!("nxmproxy: {message}");
        return platform::EXIT_FAILED;
    }

    println!("nxmproxy's nxm:// handler has been removed.");
    if !claim.claimed {
        println!("{}", claim.instruction);
    }
    claim.exit_code()
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

        Command::Install => handle_install(&mut config, &save),

        Command::Uninstall => handle_uninstall(),

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

        Command::Register { name, exec, args } => {
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
                        "warning: {name:?} has no %1, %u or %U in its arguments, so it will be \
                         started but given no link. Re-run with the link code in the right position."
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

        Command::Unregister { name } => match config.unregister_manager(&name) {
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
        // A command that genuinely is on PATH on this platform, and one that
        // is not, must give opposite answers. The GUI relies on this to flag a
        // broken entry before a link fails silently.
        // The name is platform specific: there is no `sh` on Windows and no
        // `cmd` on unix, and the first CI run failed on both platforms for
        // exactly that reason.
        #[cfg(unix)]
        let present = "sh";
        #[cfg(windows)]
        let present = "cmd";

        assert!(
            resolve_exec(present).is_some(),
            "{present} should resolve on this platform"
        );
        assert!(resolve_exec("nxmproxy-definitely-not-a-real-binary").is_none());
    }

    #[test]
    fn an_exec_line_becomes_a_path_and_an_argv() {
        // This is what makes auto-detection work for a manager that is not on
        // PATH: the desktop entry the manager wrote is the only place its real
        // path exists, so getting this parse wrong means either a broken
        // registration or no registration.
        assert_eq!(
            parse_exec_line("/opt/gmm/gamemodmanager --handle-nxm %u"),
            Some((
                "/opt/gmm/gamemodmanager".to_string(),
                vec!["--handle-nxm".to_string(), "%1".to_string()]
            ))
        );
        // A quoted path, which is what GMM writes when the path has a space in
        // it. Splitting naively on whitespace truncates it to `/opt/My`, and
        // the registered manager is then a file that does not exist.
        assert_eq!(
            parse_exec_line(r#""/opt/My Mods/gamemodmanager" --handle-nxm %u"#),
            Some((
                "/opt/My Mods/gamemodmanager".to_string(),
                vec!["--handle-nxm".to_string(), "%1".to_string()]
            ))
        );
        // An executable with no arguments at all is still a manager, just a
        // useless one; detection rejects it later for having no `%1`.
        assert_eq!(
            parse_exec_line("/usr/bin/vortex"),
            Some(("/usr/bin/vortex".to_string(), Vec::new()))
        );
    }

    #[test]
    fn the_desktop_field_codes_that_carry_no_argument_are_dropped() {
        // `%f` is a file, `%i` the icon id, `%c` the translated name. Passing
        // them through would put `%i` on the manager's command line, where it
        // is a real argument to a real program and not a field code.
        let (exe, args) =
            parse_exec_line("/usr/bin/mo2 %U %f %i %c %k").expect("a parsable Exec line");
        assert_eq!(exe, "/usr/bin/mo2");
        assert_eq!(
            args,
            vec!["%1".to_string()],
            "%U maps to %1 and nothing else"
        );
    }

    #[test]
    fn an_unparsable_exec_line_is_refused_rather_than_guessed() {
        // A half-written entry must produce no manager, not a manager pointing
        // at nothing. Detection treats `None` as "not installed", which makes
        // `install` print the command to type - the safe direction.
        assert_eq!(parse_exec_line(""), None);
        assert_eq!(parse_exec_line("   "), None);
        assert_eq!(parse_exec_line(r#""""#), None, "an empty quoted path");
        assert_eq!(parse_exec_line(r#""unterminated"#), None);
    }

    #[test]
    fn pathext_is_parsed_the_way_windows_parses_it() {
        // The bug this asserts against: ignoring PATHEXT, so a manager
        // registered as `vortex` was reported missing on Windows even though
        // `Vortex.exe` was installed and launchable.
        assert_eq!(
            pathext_extensions(Some(".COM;.EXE;.BAT;.CMD".to_string())),
            vec![".COM", ".EXE", ".BAT", ".CMD"]
        );
        // Unset falls back to the set CreateProcess itself uses.
        assert_eq!(
            pathext_extensions(None),
            vec![".COM", ".EXE", ".BAT", ".CMD"]
        );
        // Trailing separators and stray spaces must not become extensions.
        assert_eq!(
            pathext_extensions(Some(".EXE;; .CMD ".to_string())),
            vec![".EXE", ".CMD"]
        );
        // A missing dot is added, or the extension would never match a file.
        assert_eq!(
            pathext_extensions(Some("EXE;CMD".to_string())),
            vec![".EXE", ".CMD"]
        );
        assert!(pathext_extensions(Some(String::new())).is_empty());
    }
}
