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

mod browser;
mod config;
mod defaults;
mod logger;
mod pipe;
mod platform;
mod route;
mod xdg;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};

use config::{Config, Manager};
use platform::Status;
use route::Route;

/// Bumped when the `list --json` shape changes, so a GUI can refuse a format
/// it does not understand instead of misreading it.
const LIST_SCHEMA_VERSION: u32 = 5;

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
                       platform allows.",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    /// An nxm:// link to forward. The same command as `nxmproxy url <url>`.
    ///
    /// This is the form the changelog has always documented and the form a
    /// hand-written or third-party `Exec=nxmproxy %u` hands over. Upstream made
    /// the link a *subcommand* named `url`, so `nxmproxy nxm://...` was parsed
    /// as a subcommand called `nxm://...` and refused with
    /// "unrecognized subcommand" - a surface its own documentation contradicts.
    /// Both spellings work now: clap gives the subcommand priority when the
    /// first word names one, and fills this positional when it does not.
    url: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Forward an nxm:// link to the mod manager assigned for its game.
    ///
    /// An alias for the bare `nxmproxy <nxm://url>`, kept because this is the
    /// form the desktop entry nxmproxy publishes uses, and because it is what
    /// the surface has always said this subcommand is.
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
    /// * Linux - writes one `[Default Applications]` line into **every** file
    ///   the specification's lookup order passes through, desktop-specific
    ///   (`kde-mimeapps.list` and the rest) and plain alike, and confirms each
    ///   write by reading it back. `uninstall` takes every one of them out.
    /// * macOS - calls LaunchServices, which on macOS 12+ asks you first.
    /// * Windows - **cannot** choose the default app; no program may. It opens
    ///   the Default Apps page for you instead and exits 3.
    ///
    /// Exit 0 only when nxmproxy really is the handler and that was checked.
    Install,

    /// Remove nxmproxy as the nxm:// handler, and undo what `install` published.
    ///
    /// Symmetric with `install`, location for location. Linux removes nxmproxy's
    /// own id from every `[Default Applications]` line it added and leaves every
    /// other id, in every other file, exactly where it was; a file that held
    /// nothing but nxmproxy's own line is removed rather than left empty.
    Uninstall,

    /// Report whether nxmproxy is registered, and who opens nxm://.
    ///
    /// Three answers, because there are three decisions. The OS's - the whole
    /// `mimeapps.list` chain including the `$desktop-mimeapps.list` files, which
    /// is everything `install` can affect - and any browser that decides for
    /// itself and never asks the OS. A machine can pass the first and fail the
    /// second, and that is the state a user cannot see from either answer alone.
    Test,

    /// Find every mod manager this machine declares, and remember them.
    ///
    /// An application is found because its own desktop entry says
    /// `MimeType=x-scheme-handler/nxm` - the XDG declaration of "I take nxm
    /// links", published by the application itself. HD2Arsenal is the case this
    /// exists for: a third-party manager, absent from any list we ship.
    ///
    /// **Found is not chosen.** Every manager it stores is assigned to nothing,
    /// and routes nothing, until you `assign` it. Silently routing a link into
    /// an application you never picked is the one thing this program must not
    /// do, so nothing is ever registered *and* routed in the same step.
    ///
    /// Safe to run as often as you like: it never changes a manager you
    /// already have, never removes one whose application has been uninstalled
    /// (it is marked unavailable instead), and never picks between candidates.
    /// Run it again after installing a mod manager or a game.
    Discover,

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
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).collect())
        .unwrap_or_default();
    // `$XDG_BIN_HOME`, else `~/.local/bin`. Installer-managed apps drop a
    // symlink there for every app they own - `hd2arsenal ->
    // ~/Applications/HD2Arsenal` on the machine this was written for - so this
    // is where an `Exec=` name usually lives. Listing it explicitly rather than
    // trusting `PATH` is what makes it work under a shell profile that never
    // set `PATH`.
    if let Some(bin) = xdg::local_bin() {
        if !dirs.contains(&bin) {
            dirs.push(bin);
        }
    }
    let extensions = pathext_extensions(std::env::var("PATHEXT").ok());

    for dir in dirs {
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
    /// True when discovery found this manager on this machine and nobody
    /// chose it. A GUI must present it as a suggestion, never as the one that
    /// is set up: nxmproxy itself refuses to route to one of these until a
    /// game is assigned to it.
    discovered: bool,
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
struct JsonDeclaredManager {
    id: String,
    name: String,
    /// The desktop entry that declared it, so a GUI can say where it found it
    /// rather than presenting a bare command as magic.
    entry: String,
    /// The MIME types that entry declares, deduplicated. The one that matched
    /// is `x-scheme-handler/nxm`.
    mime_types: Vec<String>,
    exec: String,
    args: Vec<String>,
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
    /// Applications found by their own `x-scheme-handler/nxm` declaration that
    /// the curated list above could never contain, because we did not know they
    /// existed. HD2Arsenal is the case this exists for.
    ///
    /// This is a **live** scan of the desktop directories, independent of the
    /// config: it reports what is there now, including managers `discover`
    /// never stored because one already had that id. A GUI offers these for
    /// one-click registration; a user who has already run `discover` reads
    /// `managers[].discovered` instead.
    declared_managers: Vec<JsonDeclaredManager>,
    /// What the OS says handles `nxm://`, or `null` when nothing claims it.
    ///
    /// From the same read `nxmproxy test` prints, so a GUI cannot report a
    /// different answer from the CLI.
    handler: Option<String>,
    /// True when **nxmproxy** is that handler.
    ///
    /// The field that stops the false green. Everything above describes
    /// capability - a published entry, a registered manager, an assignment -
    /// and a GUI that renders all of it green while nxmproxy is bypassed shows
    /// the user a working install over one that routes nothing. This is the
    /// single fact that distinguishes them, and it is the same one `test`
    /// exits on.
    owns_scheme: bool,
    /// The whole resolution, in the order the OS considered it.
    ///
    /// Not a summary of `handler`: this is the evidence behind it. Every
    /// declaration with **the file it came from and the group it is in**, plus
    /// the rule that made `declarations[0]` the winner. A GUI that renders
    /// `owns_scheme: false` next to nothing else leaves the user in exactly the
    /// position this change was made to end - knowing nxmproxy lost, with no
    /// way to see who beat it or where they said so.
    ///
    /// A contest is two or more distinct `ids` across `declarations`; the first
    /// `ids` entry of each is the application that would get the link. There is
    /// no precomputed `conflicting` flag because it is one comparison, and a
    /// derived field is one more thing that can disagree with the data.
    scheme: platform::Resolution,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The id `register` would give it.
    pub id: String,
    pub name: String,
    pub manager: Manager,
}

/// An id for a manager we have no curated entry for: its `Name=`, lowercased
/// and reduced to characters a user can type.
///
/// "HD2Arsenal" becomes `hd2arsenal`, which is what the user would have
/// guessed, so the command we print is the command they would have written.
fn id_for(name: &str) -> String {
    let mut id = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_alphanumeric() {
            id.extend(ch.to_lowercase());
        } else if !id.ends_with('-') {
            id.push('-');
        }
    }
    id.trim_matches('-').to_string()
}

/// Every manager this machine can prove it has, from both sources.
///
/// Two sources, in order of trust:
///
/// 1. **The application's own XDG desktop entry**, when it declares
///    `x-scheme-handler/nxm`. That is the XDG declaration of "I take nxm
///    links", published by the application itself and read by the desktop to
///    route links, and it needs no knowledge of the application at all. This is
///    what finds HD2Arsenal, and any third-party manager published the same
///    way - the curated list below could never find one, because a third-party
///    app never appears in our own list.
/// 2. **The curated list**, by its desktop file where it has one and `PATH`
///    otherwise. This is what catches a manager with no desktop entry at all.
///
/// A candidate is accepted only when the resulting executable really resolves
/// and its argv really receives the link. Registering a manager that cannot be
/// started, or that is started and handed nothing, is exactly the broken
/// install this exists to prevent - so an unusable candidate is dropped and the
/// caller prints the command to type instead.
///
/// Duplicates are collapsed on the invocation, not on the name: one program
/// described by two desktop files in two directories is one manager, and
/// counting it twice would manufacture an ambiguity out of a single app.
pub fn detect_candidates() -> Vec<Candidate> {
    let mut candidates: Vec<Candidate> = defaults::KNOWN
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
            Some(Candidate {
                id: known.id.to_string(),
                name: known.name.to_string(),
                manager,
            })
        })
        .collect();

    for declared in declared_candidates() {
        // The same application reached by both routes is one manager, and the
        // curated entry keeps the documented id.
        if candidates
            .iter()
            .any(|seen| seen.manager == declared.manager)
        {
            continue;
        }
        candidates.push(declared);
    }

    candidates
}

/// Candidates that came from an application's **own** nxm declaration.
///
/// The third-party finds, which [`defaults::KNOWN`] cannot contain by
/// construction: a manager we never heard of has no entry in our curated list,
/// and that is the entire class of application this covers. `list --json`
/// reports them separately so a GUI can offer them as a one-click register.
fn declared_candidates() -> Vec<Candidate> {
    xdg::declared()
        .into_iter()
        .filter(|declared| {
            resolve_exec(&declared.manager.exec).is_some()
                && declared.manager.args.iter().any(|arg| arg == "%1")
        })
        .map(|declared| Candidate {
            id: id_for(&declared.name),
            name: declared.name,
            manager: declared.manager,
        })
        .collect()
}

/// The one manager `install` may register on the user's behalf, or `None`.
///
/// Exactly one or nothing. Two managers installed is genuinely ambiguous and
/// guessing routes downloads to the wrong mod queue, which is the failure
/// `Route::Ambiguous` exists to refuse at click time. `install` must not
/// invent the very ambiguity it refuses to resolve later - and it must not
/// pick the "nicer looking" one either, so there is no preference to express.
fn sole_candidate(candidates: &[Candidate]) -> Option<&Candidate> {
    match candidates {
        [only] => Some(only),
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
    // The same read, through the same predicate, that `install` verifies with
    // and `test` exits on. `list` is exempt from no rule: a JSON contract that
    // could render a false green is the exact failure this field exists to
    // stop, so a GUI reading it cannot end up greener than the terminal.
    let resolution = scheme_ownership();
    let handler = resolution.handler();
    let owns = resolution.owns();
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
                discovered: config.discovered.contains(id),
            })
            .collect(),
        assignments: config
            .games
            .iter()
            .map(|(domain, manager)| JsonAssignment { domain, manager })
            .collect(),
        known_managers: known_managers_json(),
        declared_managers: xdg::declared()
            .into_iter()
            .filter(|declared| {
                resolve_exec(&declared.manager.exec).is_some()
                    && declared.manager.args.iter().any(|arg| arg == "%1")
            })
            .map(|declared| JsonDeclaredManager {
                id: id_for(&declared.name),
                entry: declared.entry.to_string_lossy().into_owned(),
                exec: declared.manager.exec,
                args: declared.manager.args,
                name: declared.name,
                mime_types: declared.mime_types,
            })
            .collect(),
        warnings: &loaded.warnings,
        handler,
        owns_scheme: owns,
        scheme: resolution,
    };
    // BTreeMaps plus declared field order make this byte stable across runs.
    serde_json::to_string_pretty(&payload).expect("the list payload is all owned, printable data")
}

/// The exact command a user types to register `candidate`.
///
/// Quoted where it has to be, because the single most common reason this
/// project's users are here is a path with a space in it - and an unquoted
/// command we print is a command that does not work.
fn register_command(candidate: &Candidate) -> String {
    fn quote(value: &str) -> String {
        if value.contains(char::is_whitespace) {
            format!("\"{value}\"")
        } else {
            value.to_string()
        }
    }
    std::iter::once("nxmproxy".to_string())
        .chain([
            "register".to_string(),
            candidate.id.clone(),
            quote(&candidate.manager.exec),
        ])
        .chain(candidate.manager.args.iter().map(|a| quote(a)))
        .collect::<Vec<String>>()
        .join(" ")
}

/// Applications that declared nxm handling but cannot be used, as a line a user
/// can act on.
///
/// Dropping them in silence is the one thing worse than refusing them: there
/// is an entry on disk declaring the application handles nxm, nxmproxy found
/// nothing, and the message says there is nothing to be found.
fn unusable_declarations() -> Vec<String> {
    xdg::declared()
        .into_iter()
        .filter(|declared| resolve_exec(&declared.manager.exec).is_none())
        .map(|declared| {
            format!(
                "{} ({}) - not on this machine",
                declared.name, declared.manager.exec
            )
        })
        .collect()
}

/// Print what this machine has, and the command that registers each one.
///
/// A candidate list and a choice, never a choice made for the user: routing an
/// nxm link into the wrong mod manager puts a download in somebody else's
/// queue, which is worse than not routing it at all.
///
/// The register command is the one command worth printing. It is not advice -
/// it is this machine's own path and argv, which exist nowhere else and which
/// `xdg-mime`/a desktop-entry file would otherwise take a human to reconstruct.
/// Every *other* command is in `--help`, where it belongs.
fn print_candidates(candidates: &[Candidate]) {
    let refused = unusable_declarations();
    if candidates.is_empty() {
        println!("no mod manager registered, and none found on this machine.");
        print_refused(&refused);
        return;
    }

    let count = candidates.len();
    println!(
        "{count} application{} on this machine {} nxm://:",
        if count == 1 { "" } else { "s" },
        if count == 1 { "declares" } else { "declare" }
    );
    for candidate in candidates {
        println!("    {}", register_command(candidate));
    }
    print_refused(&refused);
}

/// Applications we found and would not use, named, with the reason.
///
/// The path this takes matters: the common case for an entry being refused is
/// a *partly* working install, and "nothing found" for a user who can see the
/// desktop entry would be a lie.
fn print_refused(refused: &[String]) {
    if refused.is_empty() {
        return;
    }
    println!("unusable:");
    for line in refused {
        println!("  {line}");
    }
}

/// Remember every manager this machine declares, assigned to nothing.
///
/// Only [`declared_candidates`] feeds this - the applications whose **own**
/// desktop entry declares `x-scheme-handler/nxm`. The curated list in
/// [`defaults`] is deliberately left out: it is a list of guesses about
/// executable names, and writing guesses into the user's database is the thing
/// the declaration-based detection exists to avoid.
///
/// Both guards detection already applies are inherited by calling it rather than
/// reimplementing it: a candidate whose executable does not resolve, or whose
/// argv would receive no link, is never a candidate and is never stored.
///
/// Re-runnable by construction: [`Config::discover_manager`] refuses to touch an
/// id that is already known, so running this twice, or after a `register` or an
/// `assign`, changes nothing that was already there.
fn discover_managers(config: &mut Config) -> Vec<String> {
    let mut added = Vec::new();
    for candidate in declared_candidates() {
        // The "already known" decision lives in `Config::discover_manager` and
        // nowhere else. A second copy of it here was written first, and a
        // mutation of that copy left the suite green - which is what a
        // duplicated guard buys.
        match config.discover_manager(&candidate.id, candidate.manager.clone()) {
            Ok(true) => added.push(format!("{} ({})", candidate.name, candidate.id)),
            Ok(false) => {}
            // A refused registration is reported, never swallowed: it means a
            // candidate exists that we could not store, and silence would make
            // the count of found managers and the count of stored managers
            // disagree with no explanation.
            Err(message) => {
                eprintln!("nxmproxy: could not remember {}: {message}", candidate.name)
            }
        }
    }
    added
}

/// Compose `discover`: remember what is on this machine, choose nothing.
fn handle_discover(config: &mut Config, save: &dyn Fn(&Config) -> i32) -> i32 {
    let found = declared_candidates();
    if found.is_empty() {
        println!("nothing on this machine declares x-scheme-handler/nxm.");
        print_refused(&unusable_declarations());
        return 0;
    }

    let added = discover_managers(config);
    let code = save(config);
    if code != 0 {
        return code;
    }

    // What was found, and nothing else. What a user does about it is in `--help`;
    // repeating the assignment recipe here was three commands of lecture per run.
    if added.is_empty() {
        println!(
            "{} mod manager{} already known.",
            found.len(),
            if found.len() == 1 { "" } else { "s" }
        );
    } else {
        println!(
            "found {} mod manager{}:",
            found.len(),
            if found.len() == 1 { "" } else { "s" }
        );
        for line in &added {
            println!("  {line}");
        }
    }

    print_refused(&unusable_declarations());
    0
}

/// The games assigned to `id`, as the `list` table shows them.
fn games_of(config: &Config, id: &str) -> Vec<String> {
    config
        .games
        .iter()
        .filter(|(_, assigned)| assigned.as_str() == id)
        .map(|(domain, _)| {
            if domain == config::CATCH_ALL {
                "every unassigned game".to_string()
            } else {
                domain.clone()
            }
        })
        .collect()
}

/// The routing table, in the shape the README documents:
///
/// ```text
/// mod managers:
///   <id>: <exec> <args>
///     * <game>
/// ```
///
/// Nothing else. A manager with no `*` line under it routes nothing, which
/// says "assigned to nothing" without a parenthetical saying so - and saying it
/// in prose is what this function used to do, four sentences per manager.
///
/// Provenance is not shown either: `discovered` is still in `list --json`, and a
/// manager found by the back door with no games under it is self-evident.
fn print_list(config: &Config, loaded: &config::Loaded, dir: &Path) {
    println!("config: {}", dir.join(config::CONFIG_FILE).display());
    // Second line, above the table, because it is the one fact that decides
    // whether anything in the table is reachable at all: if nxmproxy does not
    // hold `nxm://`, no assignment below is ever consulted by the desktop.
    // A routing table printed on its own implies a working setup.
    //
    // One line, not the chain. `test` is where the resolution is explained; this
    // is the verdict only, so `list` stays a table.
    let resolution = scheme_ownership();
    let handler = handler_name(&resolution);
    let foreign = handler != "nothing" && !resolution.owns();
    println!(
        "nxm:// handler: {handler}{}",
        if foreign { " (not nxmproxy)" } else { "" }
    );
    for warning in &loaded.warnings {
        eprintln!("warning: {warning}");
    }

    if config.managers.is_empty() {
        println!();
        print_candidates(&detect_candidates());
        return;
    }

    println!("\nmod managers:");
    for (id, manager) in &config.managers {
        let command = std::iter::once(manager.exec.as_str())
            .chain(manager.args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");

        // Only what the command itself does not already say. A file going
        // missing is the commonest way a list silently loses an entry, so that
        // one stays.
        let mut notes: Vec<String> = Vec::new();
        if !manager.takes_url() {
            notes.push("no link code".to_string());
        }
        if resolve_exec(&manager.exec).is_none() {
            notes.push("unavailable".to_string());
        }
        if let Some(pipe) = &manager.pipe {
            notes.push(format!("pipe {pipe}"));
        }
        let notes = if notes.is_empty() {
            String::new()
        } else {
            format!(" ({})", notes.join(", "))
        };

        println!("  {id}: {command}{notes}");
        for game in games_of(config, id) {
            println!("    * {game}");
        }
    }
}

/// Forward one link. Exit code is the answer: 0 delivered, 1 not.
///
/// Four log events, one line each, machine-greppable, covering the whole life of
/// a link: it arrived, who it resolved to, exactly what argv it went out with,
/// and whether the spawn took. This is the diagnostic the user asked for, and
/// it is the only place it can live: the OS launches us with no terminal and we
/// exit in milliseconds, so when a link does not arrive the reason cannot be
/// printed to anybody. It has to already be in the file.
fn handle_url(config: &Config, url: &str) -> i32 {
    logger::info(format!("link url={url:?}"));

    let domain = match route::domain_of(url) {
        Ok(domain) => domain,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            logger::error(format!("route result=rejected reason={message:?}"));
            return 1;
        }
    };

    let route = route::resolve(config, &domain);
    if let Route::Unhandled { domain, reason } = &route {
        let message = format!("nxmproxy: {}", reason.message(domain));
        eprintln!("{message}");
        logger::error(format!(
            "route domain={domain:?} manager=none result=unrouted reason={}",
            reason_tag(reason)
        ));
        return 1;
    }

    logger::info(format!(
        "route domain={domain:?} manager={:?} result=routed",
        // `unwrap_or_default`, not `{:?}` on the Option: `Some("vortex")` in a
        // `key=value` line is a Rust type leaking into a format that claims to
        // be greppable, and `manager=Some(` is not a token anyone can match.
        route.manager_id().unwrap_or_default()
    ));
    match route::deliver(&route, url) {
        Ok(()) => 0,
        Err(message) => {
            eprintln!("nxmproxy: {message}");
            logger::error(format!(
                "route domain={domain:?} result=undelivered reason={message:?}"
            ));
            1
        }
    }
}

/// The one word a log reader can grep for to say why a domain was unrouted.
///
/// [`Reason::message`] is the human sentence and it spans two lines with an
/// indented command in it, which is not greppable and not one line per event.
/// The tag carries the same three-way distinction without the prose.
fn reason_tag(reason: &route::Reason) -> &'static str {
    match reason {
        route::Reason::NoManagers => "no-managers",
        route::Reason::Ambiguous(_) => "ambiguous",
        route::Reason::Unassigned(_) => "unassigned",
    }
}

/// Ask the OS who owns `nxm://`, and write the answer down.
///
/// One function for three commands, because three independent readings of the
/// same question are how they came to disagree: `install` used to decide from
/// the [`platform::Claim`] it was handed, `test` from `is_registered`, and
/// `list --json` did not ask at all. They now answer from this.
///
/// The log is the point of it as much as the return value. A link that never
/// arrives leaves no stdout to read - we are launched by the OS with no terminal
/// and exit in milliseconds - so "who held the scheme when you ran install", and
/// "which of two declared applications outranked the other", have to be
/// recoverable afterwards rather than remembered.
///
/// One line per declaration and one for the verdict, all machine-greppable: a
/// reader asking "who owned nxm:// at 14:02" wants `grep '^scheme '`, not a
/// paragraph.
fn scheme_ownership() -> platform::Resolution {
    let resolution = platform::imp::resolve();
    let owns = resolution.owns();
    for (rank, declaration) in resolution.declarations.iter().enumerate() {
        logger::info(format!(
            "scheme declaration rank={} section={:?} ids={:?} source={:?}",
            rank + 1,
            declaration.section,
            declaration.ids,
            declaration.source
        ));
    }
    logger::info(format!(
        "scheme handler={:?} owns={owns} rule={:?}",
        resolution.handler().unwrap_or_else(|| "none".to_string()),
        resolution.rule
    ));
    if !owns {
        logger::warn(format!(
            "nxmproxy does not own nxm://; {} does",
            resolution
                .handler()
                .unwrap_or_else(|| "nothing".to_string())
        ));
    }
    resolution
}

/// The handler's name for a user, or a word for "nobody".
fn handler_name(resolution: &platform::Resolution) -> String {
    resolution
        .handler()
        .unwrap_or_else(|| "nothing".to_string())
}

/// The resolution chain, as the lines a user reads and a test pins.
///
/// Every candidate, in the order the OS would consider them, each with **the
/// group it came from and the file it is in**, then the winner and the rule that
/// made it the winner. A bare application name could not be checked by hand and
/// could not tell a real conflict from a ranked list; this can be read against
/// the file in either direction.
///
/// Two facts about *different applications* is its own line, because that is the
/// state that is indistinguishable from a working setup without it.
fn print_resolution(resolution: &platform::Resolution) {
    println!("nxm:// resolution, highest precedence first:");
    for declaration in &resolution.declarations {
        println!(
            "  {:<22} {}  <- {}",
            declaration.section,
            declaration.ids.join(" "),
            declaration.source
        );
    }
    match resolution.winner() {
        None => println!("  {}", resolution.rule),
        Some(winner) => println!("  winner: {} - {}", winner.ids.join(" "), resolution.rule),
    }
    let claimants = resolution.claimants();
    if claimants.len() > 1 {
        println!(
            "  declared by more than one application: {}",
            claimants.join(", ")
        );
    }
}

fn handle_test() -> i32 {
    let resolution = scheme_ownership();
    let status = Status {
        registered: match platform::imp::is_registered() {
            Ok(registered) => registered,
            Err(e) => {
                eprintln!("nxmproxy: {e}");
                return 1;
            }
        },
        caveat: if cfg!(target_os = "macos") {
            Some(
                "the default handler cannot be read on macOS; check System Settings > General \
                  > Login Items & Extensions"
                    .to_string(),
            )
        } else {
            None
        },
    };

    println!("registered: {}", status.registered);
    println!("nxm:// handler: {}", handler_name(&resolution));
    // The line that was missing. `registered: true` above means "nxmproxy is one
    // of the applications this desktop may offer for nxm://", which is what
    // `install` publishes and is true the moment it runs. It says nothing about
    // whether nxmproxy is the one that gets the link, and printing it alone is
    // how a bypassed install reads as a working one.
    println!(
        "nxmproxy handles nxm://: {}",
        if resolution.owns() { "yes" } else { "no" }
    );
    print_resolution(&resolution);
    if let Some(caveat) = &status.caveat {
        println!("note: {caveat}");
    }
    // Everything above is the OS's answer, and the OS is not what decides a link
    // clicked in a browser. One line per browser that has decided it for
    // itself, and nothing else: the file to open, and the setting inside it.
    // `README` has the rest, and the edit is the user's - no program may write
    // another application's configuration.
    let overriding = browser_overrides();
    for over in &overriding {
        println!(
            "browser override: {} decides nxm:// for itself: {} ({})",
            over.browser,
            over.says,
            over.source.display()
        );
    }
    // Upstream's contract, kept: a non-zero code means "not working". It is no
    // longer the same thing as "not installed", because those are now separable
    // facts:
    //
    //   1  nxmproxy is not even published as a handler
    //   3  it is published, and something other than nxmproxy still gets the link
    //
    // 3 is `EXIT_NEEDS_USER` for the same reason `install` returns it: nothing
    // failed, the platform allows nothing further, and the remaining step is a
    // click in settings. A caller that only cares whether nxm:// works has a
    // single answer now, and a caller that distinguishes the two cases still
    // can.
    //
    // A browser override is 3 and not 0 because exiting 0 over it is the false
    // green this command exists to end, one level further up than the OS: the
    // OS would open nxmproxy, and the browser would not.
    if !status.registered {
        platform::EXIT_FAILED
    } else if resolution.owns() && overriding.is_empty() {
        platform::EXIT_OK
    } else {
        platform::EXIT_NEEDS_USER
    }
}

/// The browsers on this machine that answer for `nxm://` without the OS.
///
/// One home, one read. Not logged and not in `list --json`: this is a
/// diagnostic for a person staring at a link that arrived nowhere, and a GUI
/// gets the same answer from the line `test` prints rather than a second field
/// that could disagree with it.
fn browser_overrides() -> Vec<browser::Override> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|home| browser::overrides(&home))
        .unwrap_or_default()
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
        match sole_candidate(&detect_candidates()) {
            Some(candidate) => {
                match config.register_manager(&candidate.id, candidate.manager.clone()) {
                    Ok(()) => {
                        routing_ready = true;
                        let saved = save(config);
                        if saved != platform::EXIT_OK {
                            code = saved;
                        }
                        println!(
                            "registered {} as {}; every game routes to it.",
                            candidate.name, candidate.id
                        );
                    }
                    Err(message) => {
                        // Not fatal, and not a claim failure either: the scheme is
                        // claimed and the user can type the command by hand.
                        eprintln!(
                            "nxmproxy: could not pre-register {}: {message}",
                            candidate.name
                        );
                    }
                }
            }
            None => print_candidates(&detect_candidates()),
        }
    }

    // Claiming the scheme and having somewhere to forward it are two different
    // things, and a script that only checks the exit code needs to know about
    // both. Exit 3 already means "registered, one step left for you", and
    // running `register` is exactly such a step.
    if !routing_ready && code == platform::EXIT_OK {
        code = platform::EXIT_NEEDS_USER;
    }

    // **Verify, do not believe.** Everything above worked from the [`Claim`] it
    // was handed, which is a statement about what we asked for. The only fact a
    // user cares about is what the machine now says, so it is read back here -
    // by the same function `test` and `list` use, and logged, so a claim that
    // did not take can be read later instead of guessed at.
    let resolution = scheme_ownership();
    if resolution.owns() {
        println!("nxm:// links now open in nxmproxy.");
        return code;
    }
    for line in claim_refusal(&resolution, &claim.instruction) {
        println!("{line}");
    }
    // Nothing failed here; the platform allowed nothing further and one click
    // remains. Never 0 over a scheme nxmproxy does not hold.
    if code == platform::EXIT_OK {
        platform::EXIT_NEEDS_USER
    } else {
        code
    }
}

/// What `install` says after asking for the scheme and reading back, given that
/// it did not get it. Never called when it did.
///
/// Pure, and extracted from the middle of [`handle_install`] for a reason that
/// is not tidiness: the decision "is this a verified claim or a request that
/// did not take" is the one this command exists to get right, and inline in a
/// thirty-line function it had no reachable control. A mutation that swapped the
/// read for the [`Claim`] it was handed went **green** on the end-to-end test,
/// because on Linux `claim()` happens to verify by the very same read - so the
/// two paths were indistinguishable there and the branch was untested. Here they
/// are distinguishable, and a caller that has just read the machine cannot
/// accidentally believe something else.
///
/// `instruction` is what the platform said to do about it, and is never empty
/// when the platform could not take the claim.
fn claim_refusal(resolution: &platform::Resolution, instruction: &str) -> Vec<String> {
    let mut lines = vec!["nxmproxy is registered as an nxm:// handler.".to_string()];
    match resolution.winner() {
        // Someone else still gets the link. Say who **and from where**: a bare
        // application name is exactly what the user could not act on last time.
        Some(winner) => lines.push(format!(
            "nxm:// still opens with {}, from [{}] of {}",
            winner.ids.join(" "),
            winner.section,
            winner.source
        )),
        // The chain was read and says nobody claims it. That is a fact.
        None if resolution.readable => {
            lines.push("nxm:// has no default handler; the desktop will ask.".to_string())
        }
        // The platform cannot answer at all. Saying "nothing claims nxm://" here
        // would be a confident sentence about something nobody looked at.
        None => lines.push(
            "nxm:// still opens with nxmproxy. nxmproxy cannot read \
                            the answer back here, so it will not claim it either."
                .to_string(),
        ),
    }
    lines.push("One step is left for you to do:".to_string());
    lines.push(if instruction.is_empty() {
        "Set nxm:// in your desktop's default applications settings.".to_string()
    } else {
        instruction.to_string()
    });
    lines
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

    // Two spellings, one command: `nxmproxy <url>` and `nxmproxy url <url>`.
    //
    // Folding them together here, once, is why the rest of this function still
    // matches on `Command` alone. `args_conflicts_with_subcommands` means clap
    // refuses a link *and* a command on one line, so the two remaining states
    // are the two real ones plus "neither", which is an empty command line.
    let command = match (cli.url, cli.command) {
        (Some(url), None) => Command::Url { url },
        (None, Some(command)) => command,
        _ => Cli::command()
            .error(
                clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand,
                "an nxm:// link, or a command",
            )
            .exit(),
    };

    match command {
        Command::Url { url } => handle_url(&config, &url),

        Command::Install => handle_install(&mut config, &save),

        Command::Uninstall => handle_uninstall(),

        Command::Test => handle_test(),

        Command::Discover => handle_discover(&mut config, &save),

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
                         started but given no link."
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
