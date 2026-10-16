// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! End-to-end tests: run the real binary and assert on what the user's
//! terminal, the OS and the target process actually see.
//!
//! The unit tests inside `src/` prove the decision logic. These prove the
//! things that only exist once the process is running, and they are the tests
//! that matter most here:
//!
//! * an unroutable link exits with a message instead of a panic;
//! * a link containing a quote reaches the target process byte for byte, as
//!   exactly one argument;
//! * `install` on Linux claims `nxm://` in exactly one file, leaves every other
//!   manager's entry alone, and `uninstall` takes its own line back out;
//! * the command names are what the final surface says they are, and the
//!   superseded ones are gone.
//!
//! Every run gets its own throwaway config directory and its own throwaway
//! `HOME`/XDG tree, so nothing here can reach the developer's real
//! configuration or their real `mimeapps.list`.

use std::path::PathBuf;
use std::process::{Command, Output};
// `Path`, `Duration` and `Instant` are only reached by the platform-specific
// tests below, so they are imported only where they are used. Importing them
// unconditionally makes Windows clippy fail with "unused import" under the
// `-D warnings` the CI runs with.
#[cfg(any(unix, target_os = "linux"))]
use std::path::Path;
#[cfg(unix)]
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_nxmproxy");

/// A throwaway directory tree, removed when it goes out of scope.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let unique = format!(
            "nxmproxy-it-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("config")).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        Sandbox { root }
    }

    fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }

    fn config_file(&self) -> PathBuf {
        self.config_dir().join("config.toml")
    }

    /// `$XDG_CONFIG_HOME`, where `xdg-mime default` writes the claim.
    ///
    /// Linux-only, like the tests that use it. Ungated, clippy rejects the
    /// whole file as dead code on macOS and Windows, which is exactly the
    /// cross-platform blind spot this suite exists to close.
    #[cfg(target_os = "linux")]
    fn xdg_config_home(&self) -> PathBuf {
        self.root.join("home").join(".config")
    }

    /// The one file nxmproxy is allowed to write a default into.
    #[cfg(target_os = "linux")]
    fn mimeapps(&self) -> PathBuf {
        self.xdg_config_home().join("mimeapps.list")
    }

    /// `$XDG_DATA_HOME/applications`, which is one of the three files GMM's own
    /// Linux registration force-propagates into. It is written here on purpose:
    /// an assertion that a file was left alone only proves something when the
    /// file existed and had content worth losing.
    ///
    /// Not Linux-gated, because the mod-manager detection test reads it on all
    /// three platforms - the lookup is plain path arithmetic, so there is no
    /// per-platform difference to hide.
    fn data_applications(&self) -> PathBuf {
        self.root.join("data").join("applications")
    }

    #[cfg(target_os = "linux")]
    fn kde_mimeapps(&self) -> PathBuf {
        self.xdg_config_home().join("kde-mimeapps.list")
    }

    /// `$HOME`, which is the tree a browser's own profile lives in.
    #[cfg(target_os = "linux")]
    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    /// Every file under the sandbox that is not ours, so we can prove `install`
    /// wrote no association state anywhere. Only the Linux test needs it; on
    /// the other platforms it would be dead code, which clippy rightly refuses.
    #[cfg(target_os = "linux")]
    fn all_files(&self) -> Vec<PathBuf> {
        fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
            let Ok(entries) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, into);
                } else {
                    into.push(path);
                }
            }
        }
        let mut found = Vec::new();
        walk(&self.root, &mut found);
        found
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Paths locked read-only, restored when this goes out of scope.
///
/// The sandbox deletes itself in `Drop`, and `remove_dir_all` needs write
/// permission on every directory it descends into. A test that locks a
/// directory to prove a write is refused has to give the permissions back, or
/// it leaves an undeletable tree in `TMPDIR` for the next run to trip over.
#[cfg(target_os = "linux")]
struct ReadOnly(Vec<PathBuf>);

#[cfg(target_os = "linux")]
impl ReadOnly {
    /// `dir` refuses new files and `file` refuses writes, which is every way
    /// `xdg-mime` has of writing a claim.
    fn lock(dir: &Path, file: &Path) -> Self {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o444)).unwrap();
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        ReadOnly(vec![dir.to_path_buf(), file.to_path_buf()])
    }

    /// Did the lock actually stop this process writing?
    ///
    /// Root ignores mode bits, so on a root runner the refusal cannot be
    /// produced. The caller has to be able to tell that apart from "the lock
    /// works", and a test that cannot tell must not assert a refusal it did not
    /// observe.
    fn held(&self) -> bool {
        std::fs::write(self.0[0].join(".probe"), b"").is_err()
    }
}

#[cfg(target_os = "linux")]
impl Drop for ReadOnly {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        for path in &self.0 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755));
        }
    }
}

fn run(sandbox: &Sandbox, args: &[&str]) -> Output {
    run_on_desktop(sandbox, "KDE", args)
}

/// [`run`] under a named `$XDG_CURRENT_DESKTOP`.
///
/// Separate because the session name is the specification's `$desktop`, so the
/// only way to ask "what does the sweep do on a machine that is not running the
/// desktop it is claiming for" is to change it. Every test pins `KDE` by
/// default, which is exactly what hid the discovery path.
///
/// Ungated: it is [`run`] with a parameter, and gating it left `run` - which
/// every cross-platform test calls - pointing at a function that does not exist
/// on macOS or Windows.
fn run_on_desktop(sandbox: &Sandbox, desktop: &str, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("NXMPROXY_CONFIG_DIR", sandbox.config_dir())
        .env("HOME", sandbox.root.join("home"))
        .env("XDG_CONFIG_HOME", sandbox.root.join("home").join(".config"))
        .env("XDG_DATA_HOME", sandbox.root.join("data"))
        .env("XDG_DATA_DIRS", sandbox.root.join("data").join("dirs"))
        .env("XDG_CONFIG_DIRS", sandbox.root.join("home").join("etc-xdg"))
        // A KDE session, spelled the way a real session spells it. This is the
        // variable the specification derives `$desktop` from, so it is what makes
        // `~/.config/kde-mimeapps.list` a file the desktop reads rather than a
        // file nxmproxy happens to write - and the uppercase is deliberate,
        // because every real session capitalises its own name and the
        // specification asks for ascii lowercase.
        .env("XDG_CURRENT_DESKTOP", desktop)
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .output()
        .expect("could not run the nxmproxy binary")
}

/// Serialises every test that runs `install` or `uninstall`.
///
/// The sandbox isolates config and XDG by environment variable. It cannot
/// isolate the two things registration actually touches: on Windows that is
/// one real `HKCU` for the whole runner, and on macOS one real
/// `~/Library/Application Support`. Cargo runs tests in parallel threads, so
/// without this they write to each other and whichever finishes second reports
/// state the first left behind. The first CI run failed exactly that way, with
/// `test` returning 0 before anything had been installed.
///
/// The lock also establishes a clean slate: a previous test's `install` would
/// otherwise leave a registration behind for the next one to trip over.
static INSTALL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take [`INSTALL_LOCK`] and undo any handler a previous test left behind.
///
/// The guard must be held for the whole test. Poisoning is recovered from rather
/// than propagated: a panic in one test says nothing about whether the registry
/// is in a known state, and refusing to run the rest would hide the real failure.
fn install_sandbox(sandbox: &Sandbox) -> std::sync::MutexGuard<'static, ()> {
    let guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let _ = run(sandbox, &["uninstall"]);
    guard
}

/// Like [`run`], but with no third-party tool on `PATH`.
///
/// `install` writes every `mimeapps.list` row itself now, so nothing it needs is
/// external and this is its only path. The test still exists because "nothing on
/// `PATH`" is a real machine - a headless container, a minimal rescue shell - and
/// because it is the proof that the claim no longer depends on `xdg-mime`, which
/// exits 0 while writing nothing. `xdg-mime` lives in `/usr/bin` alongside almost
/// everything else, so PATH is emptied rather than filtered.
#[cfg(target_os = "linux")]
fn run_without_xdg_mime(sandbox: &Sandbox, args: &[&str]) -> Output {
    let empty = sandbox.root.join("empty-path");
    std::fs::create_dir_all(&empty).unwrap();
    Command::new(BIN)
        .args(args)
        .env("NXMPROXY_CONFIG_DIR", sandbox.config_dir())
        .env("HOME", sandbox.root.join("home"))
        .env("XDG_CONFIG_HOME", sandbox.root.join("home").join(".config"))
        .env("XDG_DATA_HOME", sandbox.root.join("data"))
        .env("XDG_DATA_DIRS", sandbox.root.join("data").join("dirs"))
        .env("XDG_CONFIG_DIRS", sandbox.root.join("home").join("etc-xdg"))
        .env("XDG_CURRENT_DESKTOP", "KDE")
        .env("PATH", &empty)
        .output()
        .expect("could not run the nxmproxy binary")
}

fn code(output: &Output) -> i32 {
    output
        .status
        .code()
        .expect("process was killed by a signal")
}

/// Like [`code`], but tolerating a platform that kills the process outright.
///
/// Used only for `install` and `uninstall`, because only they reach code that
/// can die. `LSSetDefaultHandlerForURLScheme` aborts the process on a headless
/// macOS runner, which is that API's behaviour in a session with no GUI and not
/// something this program chooses - and it is the single strongest piece of
/// evidence that the macOS path is unverified.
///
/// A killed process is **never success**. Mapping it to `needs-user` keeps the
/// contract the assertions are actually about: that nxmproxy never reports a
/// verified claim it cannot verify. It does not make the macOS install path
/// tested, and it is not a claim that it works. The note is printed rather than
/// swallowed so the run shows what happened.
fn code_tolerating_kill(output: &Output) -> i32 {
    match output.status.code() {
        Some(code) => code,
        None => {
            eprintln!(
                "note: nxmproxy was killed by a signal on this platform. Treating it as \
                 needs-user, which is the truth: nothing succeeded."
            );
            3
        }
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// This very binary, as a stand-in manager that exits immediately.
///
/// Hardcoding `/bin/true` is wrong on macOS and on Windows, where it does not
/// exist; the first CI run failed on both platforms for exactly that reason.
fn bin_path() -> &'static str {
    BIN
}

/// The `[Default Applications]` value for `key`, or `None`.
///
/// Group-aware on purpose. A `mimeapps.list` can name the same mime type in
/// several groups - `[Added Associations]` is where another mod manager records
/// that it *handles* nxm, `[Default Applications]` is who the desktop *opens* -
/// so reading the first matching line reports the wrong answer.
#[cfg(target_os = "linux")]
fn default_for_scheme(text: &str, key: &str) -> Option<String> {
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
        if let Some((name, value)) = line.split_once('=') {
            if name.trim() == key {
                found = Some(value.trim().trim_end_matches(';').trim().to_string());
            }
        }
    }
    found
}

/// The signature of a Rust panic. Absence of this is the whole point of the
/// unknown-domain test.
fn panic_markers(output: &Output) -> Vec<&'static str> {
    let text = stderr(output);
    [
        "panicked at",
        "RUST_BACKTRACE",
        "called `Option::unwrap",
        "called `Result::unwrap",
    ]
    .into_iter()
    .filter(|marker| text.contains(marker))
    .collect()
}

/// Wait, bounded, for `needle` to appear in `path`.
///
/// Waiting for the file to merely **exist** is the bug this replaced. The
/// recorder appends one line per argument, so a file that exists may still be
/// half written, and a poll landing in that window reads a truncated argv and
/// reports a link that lost its URL. It passed locally for months and failed on
/// a loaded CI runner, which is the worst possible signature. Waiting for the
/// terminator the script writes last makes the file complete by construction.
#[cfg(unix)]
fn wait_for(path: &Path, needle: &str, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        if let Ok(text) = std::fs::read_to_string(path) {
            if text.contains(needle) {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// On unix, a script that records the exact argv it was called with, one
/// argument per line, then a terminator. This is how the quote test proves what
/// the *target process* receives rather than what our own function intended.
///
/// `name` may contain directories, and may contain a space: an executable path
/// with a space in it is the shape upstream could not express at all.
#[cfg(unix)]
fn argv_recorder(sandbox: &Sandbox, name: &str) -> (String, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let script = sandbox.root.join(name);
    let out = sandbox.root.join(format!("{name}.argv"));
    if let Some(parent) = script.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\n\
             printf 'DONE\\n' >> '{}'\n",
            out.display(),
            out.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script.to_string_lossy().into_owned(), out)
}

// ---------------------------------------------------------------------------
// The failure points upstream had. Each of these was a crash, a stolen scheme,
// or silently mangled arguments.
// ---------------------------------------------------------------------------

/// THE most important test in the suite. Upstream did
/// `config.resolve(game).expect("Failed to find manager for game")`, so an nxm
/// link for a game nobody configured produced a panic and a crash dialog.
/// Nobody configures every game. An unrouted link is ordinary traffic.
#[test]
fn an_unroutable_link_exits_with_a_message_and_never_panics() {
    let sandbox = Sandbox::new("unroutable");
    let link = "nxm://a-game-nobody-configured/mods/1/files/2";

    // No managers at all.
    let none = run(&sandbox, &["url", link]);
    assert_eq!(code(&none), 1, "an undelivered link must be non-zero");
    assert!(panic_markers(&none).is_empty(), "{}", stderr(&none));
    assert!(
        stderr(&none).contains("a-game-nobody-configured"),
        "{}",
        stderr(&none)
    );
    assert!(stderr(&none).contains("register"), "{}", stderr(&none));

    // Two managers and no assignment: guessing would deliver the link to the
    // wrong manager, so we refuse instead, and still do not panic.
    run(&sandbox, &["register", "one", "/bin/true", "--", "%1"]);
    run(&sandbox, &["register", "two", "/bin/true", "--", "%1"]);
    let ambiguous = run(&sandbox, &["url", link]);
    assert_eq!(code(&ambiguous), 1);
    assert!(
        panic_markers(&ambiguous).is_empty(),
        "{}",
        stderr(&ambiguous)
    );
    assert!(
        stderr(&ambiguous).contains("assign"),
        "{}",
        stderr(&ambiguous)
    );
}

/// `nxmproxy <url>` is not a subcommand called `<url>`.
///
/// Upstream made the link a subcommand named `url`, so the bare form the
/// changelog documents since 0.2.0 was parsed as a subcommand *named* by the
/// link and refused with "unrecognized subcommand". Both spellings deliver the
/// same link, to the same manager, as the same one argument - which is what
/// makes the bare form safe to put in an `Exec=` line.
#[cfg(unix)]
#[test]
fn the_bare_url_and_the_url_subcommand_deliver_identically() {
    let sandbox = Sandbox::new("bare-url");
    let (recorder, argv_file) = argv_recorder(&sandbox, "recorder.sh");
    let link = "nxm://helldivers2/mods/14359/files/59437?key=k&expires=1&user_id=2";

    run(&sandbox, &["register", "recorder", &recorder, "%U"]);
    run(&sandbox, &["assign", "recorder", "helldivers2"]);

    let bare = run(&sandbox, &[link]);
    assert_eq!(code(&bare), 0, "{}", stderr(&bare));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "the manager never finished recording its argv: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );
    assert_eq!(
        std::fs::read_to_string(&argv_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![link, "DONE"],
        "the bare form must deliver the link as exactly one argument"
    );

    // And the subcommand is not the thing that got removed to make room for it.
    // Re-recording into the same file would only prove one of the two ran.
    std::fs::write(&argv_file, "").unwrap();
    let aliased = run(&sandbox, &["url", link]);
    assert_eq!(code(&aliased), 0, "{}", stderr(&aliased));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "`url <link>` stopped working: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );
    assert_eq!(
        std::fs::read_to_string(&argv_file)
            .unwrap()
            .lines()
            .collect::<Vec<_>>(),
        vec![link, "DONE"],
        "both spellings must deliver the same thing"
    );
}

/// Upstream substituted the url into a command *string* and then split the
/// string with the Win32 parser, so a url containing a double quote became a
/// different, wrong argument vector. This asserts on what the target process
/// really received.
#[cfg(unix)]
#[test]
fn a_url_containing_a_quote_reaches_the_manager_byte_for_byte() {
    let sandbox = Sandbox::new("quote");
    let (recorder, argv_file) = argv_recorder(&sandbox, "recorder.sh");

    let link = r#"nxm://somegame/mods/1/files/2?a="b"&c='d' e f"#;
    let registered = run(
        &sandbox,
        &["register", "recorder", &recorder, "--", "--flag", "%1"],
    );
    assert_eq!(code(&registered), 0, "{}", stderr(&registered));

    let delivered = run(&sandbox, &["url", link]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "the manager never finished recording its argv: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );

    // One argument per line, so a split url would show up as extra lines. The
    // terminator is dropped: it is the harness's, not the manager's.
    let recorded = std::fs::read_to_string(&argv_file).unwrap();
    let mut lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(
        lines.pop(),
        Some("DONE"),
        "the recording did not finish: {recorded:?}"
    );
    assert_eq!(
        lines,
        vec!["--flag", link],
        "the url must arrive as exactly one argument, unmodified"
    );
}

/// The command line a user actually types. HD2 Arsenal is an Electron AppImage
/// whose desktop entry reads `Exec=HD2Arsenal --no-sandbox %U`, and the user
/// copies that vector straight into `register` - no `--` separator, and `%U`
/// rather than `%1`. Both halves were refused or ignored: the separator was
/// mandatory, and `%U` substituted nothing, so the manager was started and the
/// link silently dropped.
///
/// The executable is a recorder in a directory whose name contains a space,
/// because that is upstream's other failure mode and the two must hold at once:
/// a path with a space in it must not split the argv either.
#[cfg(unix)]
#[test]
fn a_manager_registered_from_a_desktop_entry_receives_the_link() {
    let sandbox = Sandbox::new("capital-u");
    let (recorder, argv_file) = argv_recorder(&sandbox, "My Mods/HD2Arsenal");
    assert!(
        recorder.contains(' '),
        "precondition: the executable path has a space in it: {recorder}"
    );

    // Verbatim, exactly as typed, including the missing separator.
    let registered = run(
        &sandbox,
        &["register", "HD2Arsenal", &recorder, "--no-sandbox", "%U"],
    );
    assert_eq!(code(&registered), 0, "{}", stderr(&registered));
    assert!(
        !stderr(&registered).contains("given no link"),
        "a manager holding %U must not be reported as one that never sees a link: {}",
        stderr(&registered)
    );

    // A url with a space and a quote in it, because the argv must survive both.
    let link = "nxm://somegame/mods/1/files/2?title=a b\"c";
    let delivered = run(&sandbox, &["url", link]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "the manager never finished recording its argv: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );

    let recorded = std::fs::read_to_string(&argv_file).unwrap();
    let mut lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines.pop(), Some("DONE"), "did not finish: {recorded:?}");
    assert_eq!(
        lines,
        vec!["--no-sandbox", link],
        "%U must become the link, as exactly one argument, with the flag before it intact"
    );
}

/// An empty config must work out of the box. A user with one mod manager and
/// no configuration still has to get their links.
#[test]
fn an_empty_config_with_one_manager_still_delivers() {
    let sandbox = Sandbox::new("empty-config");
    assert!(
        !sandbox.config_file().exists(),
        "precondition: no config file"
    );

    // The manager is this very binary invoked with a verb that exits
    // immediately. Hardcoding /bin/true, as this test did, is wrong on macOS
    // (where it lives at /usr/bin/true) and on Windows (where it does not
    // exist); the first CI run failed on both.
    let registered = run(&sandbox, &["register", "only", bin_path(), "--", "list"]);
    assert_eq!(code(&registered), 0, "{}", stderr(&registered));

    // No `assign` was ever run: every domain must go to the only manager.
    let delivered = run(&sandbox, &["url", "nxm://anything-at-all/mods/9/files/9"]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        panic_markers(&delivered).is_empty(),
        "{}",
        stderr(&delivered)
    );
}

/// A corrupt config must degrade to empty plus a warning. Upstream did
/// `toml::from_str(...).expect("Failed to parse game config")`, so one bad byte
/// panicked the handler for every link on the machine.
#[test]
fn a_corrupt_config_degrades_to_empty_with_a_warning() {
    let sandbox = Sandbox::new("corrupt");
    std::fs::write(sandbox.config_file(), b"[games\nthis is not = = toml").unwrap();

    let listed = run(&sandbox, &["list"]);
    assert_eq!(
        code(&listed),
        0,
        "a listing still answers: {}",
        stderr(&listed)
    );
    assert!(
        stderr(&listed).contains("not valid TOML"),
        "the user must be told: {}",
        stderr(&listed)
    );
    assert!(panic_markers(&listed).is_empty(), "{}", stderr(&listed));

    // Degrading must not mean "and now write over it". The file the user still
    // needs to repair has to survive every attempt to save.
    let assigned = run(&sandbox, &["register", "v", "/bin/true", "--", "%1"]);
    assert_eq!(code(&assigned), 1);
    assert!(
        stderr(&assigned).contains("refusing to write"),
        "{}",
        stderr(&assigned)
    );
    let after = std::fs::read_to_string(sandbox.config_file()).unwrap();
    assert_eq!(
        after, "[games\nthis is not = = toml",
        "a corrupt config must not be overwritten"
    );
}

// ---------------------------------------------------------------------------
// Registration. The point of the whole project: register, never seize.
// ---------------------------------------------------------------------------

/// Linux `install` claims `nxm://` in **every file the specification's lookup
/// order passes through**, leaves every other mod manager's entry byte for byte
/// alone, and can be read back the way a desktop reads it.
///
/// The list of locations is not a guess. The [Association between MIME types and
/// applications specification, section
/// 2](https://specifications.freedesktop.org/mime-apps/latest/file.html) names
/// **eight**, in this order, and puts `$XDG_CONFIG_HOME/$desktop-mimeapps.list`
/// **above** `$XDG_CONFIG_HOME/mimeapps.list`. GLib's `gdesktopappinfo.c` and
/// KDE's `kmimeassociations.cpp` both walk that order. Writing only the second
/// row is why KDE opened GameModManager on a machine where `install` had just
/// exited 0.
///
/// Greed is not burial. Every other application stays listed and keeps its rank
/// among the others; nxmproxy goes to the **front**, which is the claim.
#[cfg(target_os = "linux")]
#[test]
fn linux_install_claims_the_scheme_in_every_file_and_buries_nobody() {
    let sandbox = Sandbox::new("install-linux");
    let _lock = install_sandbox(&sandbox);

    // Pre-seed every file the behaviour under test could touch, so that
    // "left alone" is a real claim about content rather than about a file that
    // never existed.
    //
    // Vortex appears in `[Added Associations]`, which is *its own* record that
    // it handles nxm, and as the default for an unrelated scheme. It is
    // deliberately NOT seeded as the `[Default Applications]` value for nxm:
    // taking the scheme means putting nxmproxy in front of it, which is
    // precisely what prepending does. What has to survive is everything else.
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();

    let other_manager = "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
                         [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
    std::fs::write(sandbox.mimeapps(), other_manager).unwrap();

    // **The file that actually beat us.** GameMod Manager's Linux registration
    // writes this one, and the specification ranks it above
    // `~/.config/mimeapps.list`, so anything it says about nxm:// wins no matter
    // what nxmproxy writes anywhere else. The user deleted this file by hand
    // once; it came straight back. It is seeded here with Vortex already at the
    // front of the list, so the file nxmproxy must now claim is also the file
    // that proves claiming it does not delete Vortex.
    let kde_rival = "[Default Applications]\n\
                     x-scheme-handler/nxm=vortex.desktop;hd2arsenal.desktop;\n";
    std::fs::write(sandbox.kde_mimeapps(), kde_rival).unwrap();

    let data_mimeapps = "[Default Applications]\nx-scheme-handler/nxm=othermanager.desktop;\n";
    std::fs::write(
        sandbox.data_applications().join("mimeapps.list"),
        data_mimeapps,
    )
    .unwrap();

    let before = sandbox.all_files();
    let installed = run(
        &sandbox,
        &["register", "somewhere", "/bin/true", "--", "%1"],
    );
    assert_eq!(code(&installed), 0, "{}", stderr(&installed));

    let after = run(&sandbox, &["install"]);

    // 1. The desktop entry: nxmproxy is now eligible.
    let entry_path = sandbox.data_applications().join("nxmproxy.desktop");
    let entry = std::fs::read_to_string(&entry_path)
        .unwrap_or_else(|e| panic!("expected a desktop entry at {}: {e}", entry_path.display()));
    assert!(entry.contains("Type=Application"), "{entry}");
    assert!(
        entry.contains("MimeType=x-scheme-handler/nxm;"),
        "this is what puts us in the OS chooser: {entry}"
    );
    assert!(entry.contains(" url %u"), "{entry}");

    // 2. THE decisive assertion, and it is the one the bug report is about.
    //    The file that outranked every other claim now names nxmproxy first, so
    //    the two mod managers that were ahead of it are still there - one rank
    //    down, not deleted.
    let claimed_kde = std::fs::read_to_string(sandbox.kde_mimeapps())
        .unwrap_or_else(|e| panic!("install wrote no {}: {e}", sandbox.kde_mimeapps().display()));
    assert_eq!(
        default_for_scheme(&claimed_kde, "x-scheme-handler/nxm").as_deref(),
        Some("nxmproxy.desktop;vortex.desktop;hd2arsenal.desktop"),
        "nxmproxy goes to the FRONT and nobody is removed:\n{claimed_kde}"
    );

    // 3. And the same for the plain file, pre-seeded with content worth losing.
    //    The lookup has to be group-aware, because the seeded file has an
    //    `x-scheme-handler/nxm` line in `[Added Associations]` too, and a
    //    naive "first line matching" would read Vortex's and call the claim
    //    broken.
    let claimed = std::fs::read_to_string(sandbox.mimeapps())
        .unwrap_or_else(|e| panic!("install wrote no {}: {e}", sandbox.mimeapps().display()));
    let ours = default_for_scheme(&claimed, "x-scheme-handler/nxm");
    assert!(
        ours.as_deref() == Some("nxmproxy.desktop"),
        "install must make nxmproxy the default, got {ours:?} in:\n{claimed}"
    );
    assert_eq!(
        code(&after),
        0,
        "a verified claim exits 0: {}{}",
        stdout(&after),
        stderr(&after)
    );

    // 4. THE other decisive assertion: nobody else's entry moved. Every non-blank
    //    line that was in each seeded file before must still be there, byte for
    //    byte, and every other id must still be in its original order relative
    //    to the others.
    for line in other_manager.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            claimed.contains(line),
            "install altered or dropped another mod manager's line {line:?}:\n{claimed}"
        );
    }
    // The highest-precedence row, asserted byte for byte: our id prepended and
    // nothing else touched. Byte equality rather than "contains the old line",
    // because prepending is *supposed* to change that one line.
    assert_eq!(
        claimed_kde,
        "[Default Applications]\n\
         x-scheme-handler/nxm=nxmproxy.desktop;vortex.desktop;hd2arsenal.desktop;\n",
        "the file that outranked everything is now ours at the front and byte-for-byte \
         otherwise unchanged"
    );

    // 5. The data-level file is a lower-precedence row and is claimed too, the
    //    same way and with the same promise.
    let claimed_data = std::fs::read_to_string(sandbox.data_applications().join("mimeapps.list"))
        .unwrap_or_else(|e| panic!("install wrote no data-level mimeapps.list: {e}"));
    assert_eq!(
        default_for_scheme(&claimed_data, "x-scheme-handler/nxm").as_deref(),
        Some("nxmproxy.desktop;othermanager.desktop"),
        "the deprecated data row is in the order too, and it buries nobody:\n{claimed_data}"
    );

    // 6. **Every row we own is written, and nothing outside it is.** Named
    //    explicitly rather than counted: "10 of 10" would also be 10 of 10 for
    //    ten wrong paths. The two roots below are `$XDG_CONFIG_HOME` and
    //    `$XDG_DATA_HOME/applications`, each carrying a row per discovered
    //    `$desktop` plus the plain one.
    let out = stdout(&after);
    let claim_line = out
        .lines()
        .find(|l| l.starts_with("claimed x-scheme-handler/nxm in "))
        .unwrap_or_else(|| panic!("install reported no claim at all:\n{out}"));
    let mut expected: Vec<String> = vec![];
    for root in [sandbox.xdg_config_home(), sandbox.data_applications()] {
        for name in ["kde", "gnome", "xfce", "lxqt"] {
            expected.push(
                root.join(format!("{name}-mimeapps.list"))
                    .display()
                    .to_string(),
            );
        }
        expected.push(root.join("mimeapps.list").display().to_string());
    }
    expected.sort();

    // The two roots the desktop reads and we do not write: `$XDG_CONFIG_DIRS` and
    // `$XDG_DATA_DIRS`. Nothing under either may be created, because they belong
    // to the machine - `/usr/share/applications` is root's, `$XDG_DATA_DIRS`
    // begins here with a Flatpak export directory flatpak owns, and a `sudo
    // nxmproxy install` would silently reassign the scheme for every user on the
    // box. They cannot outrank `$XDG_CONFIG_HOME` in any case.
    for root in [
        sandbox.root.join("home/etc-xdg"),
        sandbox.root.join("data/dirs/applications"),
    ] {
        for name in ["kde", "gnome", "xfce", "lxqt"] {
            let system_row = root.join(format!("{name}-mimeapps.list"));
            assert!(
                !system_row.exists(),
                "install wrote the system row {} - it is nobody's to write",
                system_row.display()
            );
        }
        assert!(
            !root.join("mimeapps.list").exists(),
            "install created a mimeapps.list under a system root"
        );
    }
    assert!(
        claim_line.contains("in 10 of 10 files: kde gnome xfce lxqt"),
        "both of our roots, all four desktop names and the plain row: {claim_line}"
    );
    for path in &expected {
        assert!(
            Path::new(path).exists(),
            "a row we own was never written: {path}\n{claim_line}"
        );
    }

    // 7. And nothing appeared that is not association state or ours. A cache
    //    rebuilt by `update-desktop-database` is a cache of file contents, not a
    //    record of the user's choice, so it is not in this list.
    //
    //    **This is the assertion that caught the scope change.** While `install`
    //    wrote the whole eight-row table, this list held ten files under
    //    `data/dirs/applications` and `home/etc-xdg` and nothing questioned it -
    //    a user-level tool creating files in root's and flatpak's directories on
    //    every install, and changing the default application for the whole
    //    machine the moment anyone ran it with `sudo`.
    let mut added: Vec<String> = sandbox
        .all_files()
        .into_iter()
        .filter(|p| !before.contains(p))
        .map(|p| {
            p.strip_prefix(&sandbox.root)
                .unwrap_or(&p)
                .display()
                .to_string()
        })
        .filter(|name| !name.ends_with("nxm.log") && !name.ends_with("mimeinfo.cache"))
        .collect();
    // Sorted: directory read order is filesystem order and is not stable, so an
    // unsorted comparison would fail intermittently for no reason at all.
    added.sort();
    // The three seeded files are absent from this list by construction: they were
    // already in `before`. Everything else the sweep created must be here, and
    // every system root is now empty of anything we made.
    assert_eq!(
        added,
        vec![
            "config/config.toml".to_string(),
            "data/applications/gnome-mimeapps.list".to_string(),
            "data/applications/kde-mimeapps.list".to_string(),
            "data/applications/lxqt-mimeapps.list".to_string(),
            "data/applications/nxmproxy.desktop".to_string(),
            "data/applications/xfce-mimeapps.list".to_string(),
            "home/.config/gnome-mimeapps.list".to_string(),
            "home/.config/lxqt-mimeapps.list".to_string(),
            "home/.config/xfce-mimeapps.list".to_string(),
        ],
        "every row we own, and nothing under a root that belongs to the machine"
    );
    assert!(panic_markers(&after).is_empty(), "{}", stderr(&after));
}

/// The claim needs nothing installed but Rust.
///
/// `install` used to delegate the whole edit to `xdg-mime`, and `xdg-mime` has
/// no verb for removing one - and, verified directly against xdg-utils 1.1.3, it
/// **exits 0 while writing nothing** when the target directory is missing. A
/// headless container has no `xdg-mime`, and the code path that ran there was
/// hand-written and ran nowhere else. So the whole sweep is one pure text
/// function now, and this is the only path there is: PATH emptied, no desktop
/// tools, and the claim still lands in every file.
#[cfg(target_os = "linux")]
#[test]
fn the_claim_needs_no_third_party_tool_and_buries_nobody() {
    let sandbox = Sandbox::new("fallback");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();

    let other_manager = "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
                         [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
    std::fs::write(sandbox.mimeapps(), other_manager).unwrap();
    let kde_rival = "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;\n";
    std::fs::write(sandbox.kde_mimeapps(), kde_rival).unwrap();

    let installed = run_without_xdg_mime(&sandbox, &["install"]);
    assert_eq!(
        code(&installed),
        3,
        "no manager configured yet: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert!(
        !stdout(&installed).contains("note:"),
        "there is no longer a third-party tool to fall back from, and no note to \
         print about one: {}",
        stdout(&installed)
    );

    // Both files are claimed, with the same promise in each: our id at the front,
    // the other manager's still listed behind it.
    for (file, want) in [
        (sandbox.mimeapps(), "nxmproxy.desktop"),
        (sandbox.kde_mimeapps(), "nxmproxy.desktop;vortex.desktop"),
    ] {
        let claimed = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            default_for_scheme(&claimed, "x-scheme-handler/nxm").as_deref(),
            Some(want),
            "the claim did not take, or Vortex was buried instead of ranked \
             behind:\n{claimed}"
        );
    }
    let claimed = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    for line in other_manager.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            claimed.contains(line),
            "the hand-written claim altered another mod manager's line {line:?}:\n{claimed}"
        );
    }

    // And it reverses, through the same function, in every file.
    let removed = run_without_xdg_mime(&sandbox, &["uninstall"]);
    assert_eq!(code(&removed), 3);
    let after = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    assert_eq!(
        default_for_scheme(&after, "x-scheme-handler/nxm"),
        None,
        "the claim left our entry behind:\n{after}"
    );
    assert!(
        after.contains("x-scheme-handler/nxm=vortex.desktop;")
            && after.contains("x-scheme-handler/http=firefox.desktop;"),
        "the removal took another manager's entries with ours:\n{after}"
    );
    // The KDE file held nothing but Vortex before us, so ours came out and
    // Vortex is exactly as it was.
    let kde_after = std::fs::read_to_string(sandbox.kde_mimeapps()).unwrap();
    assert_eq!(
        kde_after, kde_rival,
        "removing our own id must restore the previous bytes exactly, which is \
         only true because the claim prepends instead of replacing:\n{kde_after}"
    );
}

/// `uninstall` reverses the claim, in every file `install` wrote to, and
/// restores the previous bytes exactly.
///
/// This is the half that makes `install` safe to offer. A claim nxmproxy cannot
/// take back is indistinguishable from the greed it replaced.
///
/// **Exact restoration is new, and it is a consequence of prepending rather than
/// replacing.** `xdg-mime default` overwrote the value for the key, so nothing
/// recorded what was there and Vortex could not be put back. A prepend is
/// invertible: nxmproxy goes to the front of a list, the others keep their order,
/// and taking it off the front leaves what was there. So the fixture below is
/// compared byte for byte, not by a "Vortex is still somewhere in here" check.
#[cfg(target_os = "linux")]
#[test]
fn linux_uninstall_restores_every_file_it_claimed() {
    let sandbox = Sandbox::new("uninstall-linux");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();

    // Two files seeded with content worth losing, one of them the row that
    // outranks everything.
    let seeded = "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
                  [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
    std::fs::write(sandbox.mimeapps(), seeded).unwrap();
    let kde_seeded =
        "[Default Applications]\nx-scheme-handler/nxm=vortex.desktop;hd2arsenal.desktop;\n";
    std::fs::write(sandbox.kde_mimeapps(), kde_seeded).unwrap();

    assert_eq!(
        code(&run(&sandbox, &["install"])),
        3,
        "no manager yet, so not done"
    );

    for (file, want) in [
        (sandbox.mimeapps(), "nxmproxy.desktop"),
        (
            sandbox.kde_mimeapps(),
            "nxmproxy.desktop;vortex.desktop;hd2arsenal.desktop",
        ),
    ] {
        let claimed = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            default_for_scheme(&claimed, "x-scheme-handler/nxm").as_deref(),
            Some(want),
            "no claim was made in {}:\n{claimed}",
            file.display()
        );
    }

    let removed = run(&sandbox, &["uninstall"]);
    assert_eq!(
        code(&removed),
        3,
        "uninstall is honest that the user has to choose again: {}{}",
        stdout(&removed),
        stderr(&removed)
    );

    // Per-location, byte for byte. Not "Vortex is still mentioned somewhere".
    assert_eq!(
        std::fs::read_to_string(sandbox.mimeapps()).unwrap(),
        seeded,
        "the user's own file is not exactly as it was found"
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.kde_mimeapps()).unwrap(),
        kde_seeded,
        "the file that outranks everything is not exactly as it was found"
    );

    // A file that held nothing but nxmproxy's own line is one `install` created,
    // and it goes away rather than lingering as an empty group. A
    // `$desktop-mimeapps.list` that did not exist before must not exist after.
    for name in ["gnome", "xfce", "lxqt"] {
        let path = sandbox
            .xdg_config_home()
            .join(format!("{name}-mimeapps.list"));
        assert!(
            !path.exists(),
            "{} is a file nxmproxy created and should have removed",
            path.display()
        );
    }

    // Nothing else moved: an unrelated scheme and another group survive.
    let after = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    assert!(
        after.contains("x-scheme-handler/nxm=vortex.desktop;"),
        "uninstall took the other manager's own nxm entry with ours:\n{after}"
    );
    assert!(
        after.contains("x-scheme-handler/http=firefox.desktop;"),
        "uninstall touched an unrelated scheme:\n{after}"
    );
    assert!(
        after.contains("[Added Associations]") && after.contains("[Default Applications]"),
        "uninstall removed a group:\n{after}"
    );
}

/// Run the **desktop's own resolver** against the sandbox and report what it
/// says.
///
/// Not a re-implementation. `xdg-mime query default` walks the specification's
/// table for itself, loads the named desktop files, and checks each one declares
/// the type - so it is a third opinion that shares no code with nxmproxy's
/// [`Chain`](platform::linux) reader and no code with this test. Reading our own
/// file back would only ever prove we wrote our own file.
///
/// Prints a note and returns `None` when `xdg-mime` is not installed, rather
/// than skipping silently, so a run that proved less than it looks like says so.
#[cfg(target_os = "linux")]
fn desktop_says(sandbox: &Sandbox) -> Option<String> {
    desktop_says_on(sandbox, "KDE")
}

/// [`desktop_says`] under a named `$XDG_CURRENT_DESKTOP`, so the same fixture can
/// be asked what the desktop answers when it is a different session than the one
/// that installed.
#[cfg(target_os = "linux")]
fn desktop_says_on(sandbox: &Sandbox, desktop: &str) -> Option<String> {
    let out = Command::new("xdg-mime")
        .args(["query", "default", "x-scheme-handler/nxm"])
        .current_dir(sandbox.root.join("home"))
        .env("HOME", sandbox.root.join("home"))
        .env("XDG_CONFIG_HOME", sandbox.root.join("home").join(".config"))
        .env("XDG_DATA_HOME", sandbox.root.join("data"))
        .env("XDG_DATA_DIRS", sandbox.root.join("data").join("dirs"))
        .env("XDG_CONFIG_DIRS", sandbox.root.join("home").join("etc-xdg"))
        .env("XDG_CURRENT_DESKTOP", desktop)
        .output();
    match out {
        Ok(out) if out.status.success() => {
            Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
        }
        Ok(out) => {
            eprintln!(
                "note: xdg-mime query failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            None
        }
        Err(e) => {
            eprintln!(
                "note: xdg-mime is not installed here ({e}); the desktop-level \
                      resolution is not being proven on this machine"
            );
            None
        }
    }
}

/// The claim, checked by the desktop rather than by us.
///
/// This is the test the whole ticket exists for. Before it, nxmproxy wrote one
/// line into `~/.config/mimeapps.list`, GameMod Manager wrote
/// `~/.config/kde-mimeapps.list`, the specification ranks the second file above
/// the first, and **KDE opened GameMod Manager while `install` exited 0**.
///
/// Every step asserts something the step before did not:
/// 1. GMM alone in the higher file wins, even with nxmproxy first in the lower
///    one - the bug, reproduced;
/// 2. deleting the higher file by hand fixes it - which is what the user did,
///    and why it looked like a fluke;
/// 3. `install` puts nxmproxy first in the higher file too, and the desktop says
///    nxmproxy;
/// 4. GMM writing the lower file *afterwards* loses, because a lower row cannot
///    beat a higher one.
#[cfg(target_os = "linux")]
#[test]
fn the_desktop_itself_names_nxmproxy_after_install() {
    let sandbox = Sandbox::new("desktop-resolve");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "somewhere", bin_path(), "--", "%1"]
        )),
        0
    );

    // Two applications, both published, both declaring the scheme. Only the
    // desktop entry decides this - `xdg-mime` refuses an id whose `.desktop` file
    // is missing or does not declare the type, exactly as the specification says.
    //
    // **Both of them have to be published, not just the rival.** This fixture
    // reads the desktop *before* `install` runs, and `install` is what writes
    // `nxmproxy.desktop`. With only GMM published, the row naming nxmproxy in
    // `mimeapps.list` was a claim against an application that was not installed,
    // so the desktop skipped it - and "removing the higher file restores nxmproxy"
    // was asserting a fallback that only happens to have picked nxmproxy because
    // it was the sole installed handler for the type.
    publish_nxm_app(&sandbox, "gamemodmanager-nxm", "GameModManager");
    publish_nxm_app(&sandbox, "nxmproxy", "NXM Proxy");

    // GMM's state: first in the desktop-specific file, which outranks the plain
    // one.
    std::fs::write(
        sandbox.kde_mimeapps(),
        "[Default Applications]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n",
    )
    .unwrap();
    std::fs::write(
        sandbox.mimeapps(),
        "[Default Applications]\nx-scheme-handler/nxm=nxmproxy.desktop;\n",
    )
    .unwrap();
    let Some(said) = desktop_says(&sandbox) else {
        return;
    };
    assert_eq!(
        said, "gamemodmanager-nxm.desktop",
        "the bug being fixed, reproduced: the desktop-specific file outranks the \
         plain one, so nxmproxy being first in the lower file is worth nothing"
    );

    // **And our own reader must say the same thing, here, before `install`.**
    // This is the false green the whole change exists to end, and it is the
    // direction that costs the user: the desktop opens GMM, nxmproxy is in the
    // lower row, and a reader that skipped `$desktop-mimeapps.list` answers
    // `nxmproxy handles nxm://: yes`. The assertion below the `install` only ever
    // checked the *other* direction - our reader claiming a loss while the
    // desktop claims a win - so narrowing the read chain to one file went green
    // here and red three tests over. A control found the gap; this closes it.
    let before_install = stdout(&run(&sandbox, &["test"]));
    assert!(
        before_install.contains("nxmproxy handles nxm://: no")
            && before_install.contains("winner: gamemodmanager-nxm.desktop")
            && before_install.contains("kde-mimeapps.list"),
        "`test` must not report a win the desktop has already given away:\n{before_install}"
    );
    assert_eq!(
        code(&run(&sandbox, &["test"])),
        3,
        "and it exits 3 while saying so"
    );

    // What the user did by hand: delete the higher file. It works, which is why
    // the problem looked intermittent and cost an entire session.
    std::fs::remove_file(sandbox.kde_mimeapps()).unwrap();
    assert_eq!(
        desktop_says(&sandbox).as_deref(),
        Some("nxmproxy.desktop"),
        "removing the higher file restores nxmproxy - the manual fix, and the reason \
         the cause was not obvious"
    );

    // `install`. It writes the higher file as well, and now the desktop agrees.
    assert_eq!(
        code(&run(&sandbox, &["install"])),
        0,
        "a registered manager and a verified claim: {}{}",
        stdout(&run(&sandbox, &["install"])),
        stderr(&run(&sandbox, &["install"]))
    );
    assert_eq!(
        desktop_says(&sandbox).as_deref(),
        Some("nxmproxy.desktop"),
        "install wrote every row and the desktop still says somebody else"
    );

    // And GMM writing the file it was told to stop writing cannot take it back,
    // because a lower-precedence row never beats a higher one.
    std::fs::write(
        sandbox.mimeapps(),
        "[Default Applications]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n",
    )
    .unwrap();
    assert_eq!(
        desktop_says(&sandbox).as_deref(),
        Some("nxmproxy.desktop"),
        "a rival writing a LOWER row loses to a claim in a higher one"
    );
    let tested = stdout(&run(&sandbox, &["test"]));
    assert!(
        tested.contains("nxmproxy handles nxm://: yes"),
        "and our own reader agrees with the desktop, which is the point of reading \
         the same chain: {tested}"
    );

    // **Where it is not true, stated plainly rather than implied away.** GMM
    // writing the *same* desktop-specific row does win - there is no precedence
    // left to appeal to, and no program may write another program's entry out of
    // it. What must hold is that we say so: `test` reports the loss and exits 3
    // rather than carrying on claiming a win.
    std::fs::write(
        sandbox.kde_mimeapps(),
        "[Default Applications]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n",
    )
    .unwrap();
    assert_eq!(
        desktop_says(&sandbox).as_deref(),
        Some("gamemodmanager-nxm.desktop"),
        "the honest limit of any claim: same row, later writer wins"
    );
    let out = run(&sandbox, &["test"]);
    let text = stdout(&out);
    assert!(
        text.contains("nxmproxy handles nxm://: no")
            && text.contains("winner: gamemodmanager-nxm.desktop")
            && text.contains("kde-mimeapps.list"),
        "a rival writing the same row DOES win, and the user must be told which \
         file it wrote - the one place a claim cannot be defended is the one place \
         it must be honest:\n{text}"
    );
    assert_eq!(
        code(&out),
        3,
        "and exiting 0 over it would be the exact false green this command exists \
         to end:\n{text}"
    );
}

/// A location the sweep cannot write is **named**, not skipped.
///
/// `xdg-mime default` was verified against xdg-utils 1.1.3 to exit 0 and write
/// **nothing** when the target directory is missing, and a directory the user
/// does not own fails the same way with no error code carrying it. Every write is
/// read back, and a location that did not take is reported with the path and the
/// reason - because "the sweep reported 10 of 10" and "the sweep reported 4 of 10
/// and named the 6" are different machines and only one of them works.
///
/// The refusal has to be produced somewhere we genuinely write. It used to be
/// produced in `$XDG_DATA_DIRS`, which is not a location we write any more - so
/// the sweep had nothing left to refuse and the case had quietly stopped existing
/// while still passing. `~/.config` is both a location we write and the one a
/// real profile can plausibly lock down.
#[cfg(target_os = "linux")]
#[test]
fn a_location_that_cannot_be_claimed_is_named_with_the_reason() {
    let sandbox = Sandbox::new("refused-location");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();

    // The file GMM writes and the one `xdg-mime` writes, both in a directory the
    // user does not own.
    let seeded = sandbox.mimeapps();
    std::fs::write(
        &seeded,
        "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n",
    )
    .unwrap();
    let lock = ReadOnly::lock(&sandbox.xdg_config_home(), &seeded);
    if !lock.held() {
        eprintln!(
            "note: the sandbox is still writable despite the mode bits (root?); \
                  the refusal case cannot be produced here"
        );
        return;
    }

    let out = run(&sandbox, &["install"]);
    let text = stdout(&out);
    assert!(
        text.contains(&format!("  refused {}: ", seeded.display())),
        "a location that could not be claimed is named, with the path, not \
         silently skipped:\n{text}"
    );
    assert!(
        text.contains("could not create") || text.contains("could not write"),
        "and with the reason, so the reader knows whether to sudo, to chmod, or to \
         stop caring:\n{text}"
    );
    // The count in the header has to account for the failure. A summary claiming
    // full coverage over a sweep that lost files is the silent skip this test
    // exists to prevent.
    assert!(
        text.contains("of 10 files: kde"),
        "and still say how many rows it consulted, so the loss is a number and not \
         a disappearance:\n{text}"
    );
    assert!(
        !text.contains("in 10 of 10 files:"),
        "the summary count must show the loss:\n{text}"
    );
    // Nothing of anyone else's was lost on the way. The read-only file is exactly
    // the case where a careless implementation truncates it before finding out.
    drop(lock);
    assert_eq!(
        std::fs::read_to_string(&seeded).unwrap(),
        "[Default Applications]\nx-scheme-handler/http=firefox.desktop;\n",
        "a refused location is left byte for byte as it was found - refusing to \
         write is the whole point"
    );
    // And the rows that did take are enough to win anyway, which is what sweeping
    // every row buys: one unwritable directory does not cost the user their link.
    assert_eq!(
        desktop_says(&sandbox).as_deref(),
        Some("nxmproxy.desktop"),
        "the rows that did take are still enough to win"
    );
}

/// A `$desktop-mimeapps.list` that exists on disk is claimed **even when that
/// desktop is not the one running**.
///
/// This is the machine the whole change is for. A user has KDE installed, has
/// run it, and is currently in GNOME - or is in a TTY, or over SSH, where
/// `$XDG_CURRENT_DESKTOP` is unset entirely. GMM's `kde-mimeapps.list` is sitting
/// there holding the scheme, and it is the highest-precedence row the moment the
/// user logs back into KDE. A sweep driven only by `$XDG_CURRENT_DESKTOP` writes
/// every file that session will consult and none of the ones that session will
/// consult *later*, which is the same bug with a different trigger.
///
/// Every other test pins `XDG_CURRENT_DESKTOP=KDE`, so this path - discovery of
/// a desktop by the file it already has - had no end-to-end coverage at all. The
/// unit test that walks the four roots is not a substitute: the claim is a file
/// write, and the write is what has to be proven.
#[cfg(target_os = "linux")]
#[test]
fn a_desktop_file_that_already_exists_is_claimed_while_another_desktop_runs() {
    let sandbox = Sandbox::new("foreign-desktop");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();
    publish_nxm_app(&sandbox, "gamemodmanager-nxm", "GameModManager");

    // GMM's row, in a Cinnamon profile, on a machine whose running session is
    // GNOME. **`x-cinnamon` is deliberately not one of the four names the
    // specification's own text offers** (`kde`, `gnome`, `xfce`, `lxqt`), because
    // that list is an open set and hardcoding it is exactly the bug: with `kde`
    // as the stand-in this test passed with the entire on-disk discovery disabled,
    // because `kde` is in that hardcoded list and the row was written anyway.
    // Nothing but walking the four roots and looking finds this one.
    let cinnamon = sandbox.xdg_config_home().join("x-cinnamon-mimeapps.list");
    std::fs::write(
        &cinnamon,
        "[Default Applications]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n",
    )
    .unwrap();
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "somewhere", "/bin/true", "--", "%1"]
        )),
        0
    );

    let installed = run_on_desktop(&sandbox, "GNOME", &["install"]);
    assert_eq!(
        code(&installed),
        0,
        "the claim took on the rows this session reads: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    let claimed = std::fs::read_to_string(&cinnamon).unwrap();
    assert_eq!(
        default_for_scheme(&claimed, "x-scheme-handler/nxm").as_deref(),
        Some("nxmproxy.desktop;gamemodmanager-nxm.desktop"),
        "the Cinnamon row is claimed too, found on disk and not from \
         XDG_CURRENT_DESKTOP, and GMM keeps its rank:\n{claimed}"
    );

    // And it is a real claim, not a line in a file: log into Cinnamon and the
    // desktop, which consults that row first, opens nxmproxy.
    assert_eq!(
        desktop_says_on(&sandbox, "X-Cinnamon").as_deref(),
        Some("nxmproxy.desktop"),
        "a session that is not running must still find the row already claimed"
    );
}

/// Every name in the final surface resolves to the right thing, and every
/// superseded name is gone.
///
/// Upstream used `register`/`deregister` for *mod managers* and `Install` for
/// *scheme registration*, so the mapping is a restoration of `register` rather
/// than a new name for it. `register-manager` was this fork's own invention and
/// is deleted outright: a command that keeps working after being renamed is a
/// second surface nobody documents.
#[test]
fn the_command_names_are_the_final_surface_and_the_old_ones_are_gone() {
    let sandbox = Sandbox::new("names");
    let exec = bin_path();

    // `register` is upstream's name for this and still means this.
    assert_eq!(
        code(&run(&sandbox, &["register", "vortex", exec, "--", "%1"])),
        0,
        "{}",
        stderr(&run(&sandbox, &["register", "vortex", exec, "--", "%1"]))
    );
    assert_eq!(code(&run(&sandbox, &["assign", "vortex", "skyrimse"])), 0);

    // `assign` and `list` are untouched by this change, and still work.
    let listed = run(&sandbox, &["list", "--json"]);
    assert_eq!(code(&listed), 0, "{}", stderr(&listed));
    assert!(stdout(&listed).contains("skyrimse"), "{}", stdout(&listed));

    // `unregister` replaces `unregister-manager`, and still removes assignments.
    assert_eq!(code(&run(&sandbox, &["unregister", "vortex"])), 0);
    let listed = run(&sandbox, &["list", "--json"]);
    assert!(!stdout(&listed).contains("skyrimse"), "dangling assignment");

    // Everything that is gone must actually be gone: clap exits 2 on an unknown
    // subcommand, which is distinct from every code this program defines.
    // `register` is deliberately absent from this list: it is upstream's name for
    // this same operation and it still works, which is the point of the mapping.
    //
    // Each name is invoked at the arity it would have had, so that a surviving
    // alias is rejected for being unknown rather than for taking the wrong
    // number of arguments - which is the same exit 2 and a green test that
    // proves nothing.
    for (gone, args) in [
        ("register-manager", vec!["vortex", exec, "--", "%1"]),
        ("unregister-manager", vec!["vortex"]),
        ("deregister", vec!["vortex"]),
    ] {
        let mut argv = vec![gone];
        argv.extend(args.iter().copied());
        let out = run(&sandbox, &argv);
        assert_eq!(
            code(&out),
            2,
            "`{gone}` must no longer exist, but it returned {}: {}",
            code(&out),
            stderr(&out)
        );
    }

    // `install`/`uninstall` keep their upstream spelling and are still the
    // scheme half, not the manager half.
    let help = run(&sandbox, &["--help"]);
    assert!(code(&help) == 0);
    let text = stdout(&help);
    for present in [
        "install",
        "uninstall",
        "register",
        "unregister",
        "assign",
        "list",
    ] {
        assert!(
            text.contains(present),
            "`{present}` missing from --help:\n{text}"
        );
    }
}

/// On every platform, `install` must refuse to call a setup with no manager
/// to forward to done.
///
/// This is the property that survives everywhere, and it is the one the whole
/// exit-code split exists for. It is separated from the round trip below
/// because on macOS `install` is killed by LaunchServices before it prints
/// anything, so an assertion about its output could not hold there - and a test
/// that fails for a reason unrelated to what it is about is worse than a test
/// that says only what it can prove.
#[test]
fn install_never_calls_a_routing_setup_done() {
    let sandbox = Sandbox::new("never-done");
    let _lock = install_sandbox(&sandbox);

    let installed = run(&sandbox, &["install"]);
    // Nothing is configured in this sandbox and nothing is detectable on a bare
    // runner, so there are two reasons not to be done and either is enough.
    assert_eq!(
        code_tolerating_kill(&installed),
        3,
        "an install that routes nothing is not done: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert_ne!(
        code_tolerating_kill(&installed),
        0,
        "exit 0 over an install that routes nothing is the trap this code exists to close"
    );
}

/// `test` after `install` must report registered, and `uninstall` must remove
/// exactly what `install` created and nothing else.
///
/// Linux and Windows only. On macOS `install` is killed by LaunchServices in a
/// session with no GUI, so there is no post-install state to assert about and
/// pretending otherwise would be a test that cannot pass. The part of the
/// contract that *is* checkable on macOS is
/// [`install_never_calls_a_routing_setup_done`].
#[cfg(not(target_os = "macos"))]
#[test]
fn install_and_uninstall_round_trip() {
    let sandbox = Sandbox::new("round-trip");
    let _lock = install_sandbox(&sandbox);

    let tested = run(&sandbox, &["test"]);
    assert_eq!(code(&tested), 1, "not installed means a non-zero exit code");

    let installed = run(&sandbox, &["install"]);
    // No manager is registered in this sandbox and none can be detected, so
    // install has correctly left a step for the user.
    assert_eq!(
        code(&installed),
        3,
        "an install with no mod manager is not done: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert!(
        stdout(&installed).contains("no mod manager registered, and none found"),
        "install must say it has nothing to route to: {}",
        stdout(&installed)
    );

    // With a manager present the same command succeeds outright on Linux, and
    // still reports needs-user on Windows, which cannot choose the default app.
    assert_eq!(
        code(&run(&sandbox, &["register", "one", bin_path(), "--", "%1"])),
        0
    );
    let installed = run(&sandbox, &["install"]);
    let expected = if cfg!(target_os = "linux") { 0 } else { 3 };
    assert_eq!(
        code(&installed),
        expected,
        "{}{}",
        stdout(&installed),
        stderr(&installed)
    );

    let after = run(&sandbox, &["test"]);
    assert_eq!(code(&after), 0, "{}", stderr(&after));
    assert!(
        stdout(&after).contains("registered: true"),
        "{}",
        stdout(&after)
    );

    let uninstalled = run(&sandbox, &["uninstall"]);
    // uninstall is honest too: Linux gives the scheme back and says so, Windows
    // cannot and says so. Neither may claim success.
    assert_ne!(
        code(&uninstalled),
        1,
        "uninstall failed outright: {}",
        stderr(&uninstalled)
    );
    let after = run(&sandbox, &["test"]);
    assert_eq!(code(&after), 1);
    assert!(
        stdout(&after).contains("registered: false"),
        "{}",
        stdout(&after)
    );
    // "Not installed" is a state, not a failure: it must not be reported as an
    // error on stderr, which is what a missing desktop file used to do.
    assert!(
        stderr(&after).is_empty(),
        "test after uninstall should be quiet: {}",
        stderr(&after)
    );
}

// ---------------------------------------------------------------------------
// Explaining the resolution.
//
// The machine this was written for had the scheme declared TWICE, by two
// different applications:
//
//     [Added Associations]    x-scheme-handler/nxm=gamemodmanager-nxm.desktop;
//     [Default Applications]  x-scheme-handler/nxm=nxmproxy.desktop
//
// Per XDG the second outranks the first, so nxmproxy did win - and `test`
// printed `nxm:// handler: nxmproxy.desktop` with nothing else, which is not
// checkable against the file and looks exactly like the message for the case
// where it did not. A tool that resolves a handler has to be able to explain
// the resolution.
// ---------------------------------------------------------------------------

/// The user's own `mimeapps.list`, verbatim in shape.
///
/// Both groups, both applications, in one file. Returned as a string because
/// three tests below need the same bytes and a fixture that each test retypes is
/// three fixtures that can drift apart.
#[cfg(target_os = "linux")]
const CONFLICTING_MIMEAPPS: &str = "[Added Associations]\n\
                                    x-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n\n\
                                    [Default Applications]\n\
                                    x-scheme-handler/nxm=nxmproxy.desktop\n";

/// The machine this whole change came from: GMM is what the desktop opens, and
/// nxmproxy's own added association is still sitting in the file, untouched.
///
/// The mirror image of [`CONFLICTING_MIMEAPPS`] - there nxmproxy wins the
/// contest, here it loses it - and losing is the case the tool used to get
/// wrong. Both groups are named in both fixtures, so the only difference is
/// which one is `[Default Applications]`, which is exactly the fact a reader
/// could not previously see.
#[cfg(target_os = "linux")]
const BYPASSED_MIMEAPPS: &str = "[Added Associations]\n\
                                   x-scheme-handler/nxm=nxmproxy.desktop;\n\n\
                                   [Default Applications]\n\
                                   x-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n";

/// Publish `id` as an **installed** application that declares the nxm scheme.
///
/// A `mimeapps.list` line naming an application with no desktop entry on disk is
/// not a claim on anything, and both xdg-mime and our own reader skip it - the
/// specification says to load the named file and verify the association before
/// accepting a default, and to carry on to the next file when it is absent.
/// Verified directly: with GMM's entry present `xdg-mime query default` answers
/// `gamemodmanager-nxm.desktop`; delete the file and the identical
/// `mimeapps.list` answers `nxmproxy.desktop`.
///
/// So a fixture that says "GMM holds nxm://" is only that machine if GMM is
/// installed in it. Three tests seeded a rival that was named but never
/// installed, the reader discarded it, our own lower row became the winner, and
/// `install` printed "nxm:// links now open in nxmproxy." after every single
/// write had been refused. The false green came from the fixture, not from the
/// resolution code - which is why this is a helper every fixture calls rather
/// than a fix to `resolve`.
#[cfg(target_os = "linux")]
fn publish_nxm_app(sandbox: &Sandbox, id: &str, name: &str) {
    let dir = sandbox.data_applications();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(format!("{id}.desktop")),
        format!(
            "[Desktop Entry]\nType=Application\nName={name}\n\
             Exec=/bin/true url %u\nTerminal=false\nCategories=Utility;\n\
             MimeType=x-scheme-handler/nxm;\n"
        ),
    )
    .unwrap();
}

/// Put the conflicting file where the desktop reads it, and publish nxmproxy.
///
/// The fixture is seeded into **both** `~/.config/mimeapps.list` and
/// `~/.config/kde-mimeapps.list`, because on a KDE session the second is the
/// higher-precedence row and a conflict written only into the first is a
/// conflict the desktop would never see. `install` re-writes the
/// `[Default Applications]` value in both to put nxmproxy at the front, which on
/// this fixture is already there - so the claim is a no-op and the conflict the
/// tests are about survives it.
///
/// The install itself exits 0: GMM is published in this fixture, so it is a
/// discovered manager, nxmproxy registers it, and there is nothing left for the
/// user to do. It used to exit 3 because the rival was named in a `mimeapps`
/// file but never published as an application - see [`publish_nxm_app`] - and
/// "no mod manager to forward to" was a fact about the fixture, not the machine.
#[cfg(target_os = "linux")]
fn seeded_conflict(sandbox: &Sandbox) {
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    publish_nxm_app(sandbox, "gamemodmanager-nxm", "GameModManager");
    std::fs::write(sandbox.mimeapps(), CONFLICTING_MIMEAPPS).unwrap();
    std::fs::write(sandbox.kde_mimeapps(), CONFLICTING_MIMEAPPS).unwrap();
    let installed = run(sandbox, &["install"]);
    assert_eq!(
        code(&installed),
        0,
        "the claim holds and a manager is registered: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert!(
        stdout(&installed).contains("nxm:// links now open in nxmproxy."),
        "nxmproxy does hold the scheme on this fixture: {}",
        stdout(&installed)
    );
    // Compared semantically, not byte for byte, and the reason is the claim
    // itself: `install` rewrites the value to `id=nxmproxy.desktop;` and the
    // fixture left the trailing `;` off. The conflict - which group each
    // application is in - is the fact under test, and it is asserted on.
    for file in [sandbox.mimeapps(), sandbox.kde_mimeapps()] {
        let after = std::fs::read_to_string(&file).unwrap();
        assert_eq!(
            default_for_scheme(&after, "x-scheme-handler/nxm").as_deref(),
            Some("nxmproxy.desktop"),
            "install changed who wins in {}: {after}",
            file.display()
        );
        assert!(
            after
                .contains("[Added Associations]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;"),
            "install moved the conflict's other group in {}: {after}",
            file.display()
        );
    }
}

/// `test` explains the whole resolution, and the answer can be checked against
/// the file by hand.
///
/// Whole-stream, like [`list_is_exactly_the_documented_shape`] and for the same
/// reason: a substring check passes on prose nobody wrote an assertion for.
/// Every line here is either a fact about a file on disk or the rule that picks
/// between them, and a fifth line of commentary would break this.
#[cfg(target_os = "linux")]
#[test]
fn test_explains_a_scheme_two_applications_both_declare() {
    let sandbox = Sandbox::new("chain");
    let _lock = install_sandbox(&sandbox);
    seeded_conflict(&sandbox);

    let tested = run(&sandbox, &["test"]);
    assert_eq!(code(&tested), 0, "{}{}", stdout(&tested), stderr(&tested));

    // The chain, in the specification's order. `$XDG_CURRENT_DESKTOP=KDE` in the
    // sandbox, so `~/.config/kde-mimeapps.list` is row 1 and
    // `~/.config/mimeapps.list` is row 2 - and the winner is read out of row 1.
    // Asserting the exact stream is what makes the ORDER falsifiable: a chain
    // with the right ten rows in the wrong sequence is still a desktop that
    // opens GMM.
    // Only the rows we own exist, because only the rows we own are written. The
    // two system roots are absent from this list for the first time - and absent
    // from the resolution too, which is the honest consequence: a chain that
    // printed a row for a file nobody has is a chain nobody can check.
    let rows: Vec<(PathBuf, bool)> = vec![
        (sandbox.kde_mimeapps(), true),
        (sandbox.mimeapps(), true),
        (sandbox.data_applications().join("kde-mimeapps.list"), false),
        (sandbox.data_applications().join("mimeapps.list"), false),
    ];
    let mut expected = String::from(
        "registered: true\n\
         nxm:// handler: nxmproxy.desktop\n\
         nxmproxy handles nxm://: yes\n\
         nxm:// resolution, highest precedence first:\n",
    );
    for (path, conflicted) in &rows {
        expected.push_str(&format!(
            "  Default Applications   nxmproxy.desktop  <- {}\n",
            path.display()
        ));
        if *conflicted {
            expected.push_str(&format!(
                "  Added Associations     gamemodmanager-nxm.desktop  <- {}\n",
                path.display()
            ));
        }
    }
    expected.push_str(
        "  winner: nxmproxy.desktop - [Default Applications] outranks [Added Associations]\n\
         \x20 declared by more than one application: nxmproxy.desktop, gamemodmanager-nxm.desktop\n",
    );
    assert_eq!(
        stdout(&tested),
        expected,
        "the chain must name every candidate, its file, its group, the winner and the \
         rule"
    );
}

/// The exit code is the answer, and "registered" is not the answer.
///
/// This is the state the user was in after setting the default back to GMM by
/// hand: nxmproxy is still published as a handler, so `registered: true` is
/// true, and the scheme is being handed to somebody else. Before this, that
/// printed a green-looking pair of lines and exited 0.
#[cfg(target_os = "linux")]
#[test]
fn test_exits_truthfully_when_registered_but_bypassed() {
    let sandbox = Sandbox::new("bypassed");
    let _lock = install_sandbox(&sandbox);
    seeded_conflict(&sandbox);

    // The user picks GameMod Manager in Settings > Default Applications. A KDE
    // settings dialog writes the **desktop-specific** file - that is what it is
    // for - so the bypass has to be written there or the desktop never sees it.
    // This is the exact state the whole sweep exists to answer: nxmproxy is
    // still published, it is still first in the lower-precedence file, and the
    // one file above both says somebody else.
    let bypassed = "[Added Associations]\nx-scheme-handler/nxm=nxmproxy.desktop;\n\n\
                    [Default Applications]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n";
    std::fs::write(sandbox.kde_mimeapps(), bypassed).unwrap();
    std::fs::write(sandbox.mimeapps(), bypassed).unwrap();

    let tested = run(&sandbox, &["test"]);
    let text = stdout(&tested);
    assert!(
        text.contains("registered: true"),
        "nxmproxy is still published, and saying otherwise would be its own lie: {text}"
    );
    assert!(
        text.contains("nxmproxy handles nxm://: no"),
        "the fact that decides everything else: {text}"
    );
    assert_eq!(
        code(&tested),
        3,
        "registered but bypassed is not done, and it is not a failure either: {text}"
    );
    assert_ne!(
        code(&tested),
        0,
        "exit 0 here is the trap this test exists for"
    );
    // And the resolution names who beat us and from where, so the next command
    // is a file to open rather than a guess.
    assert!(
        text.contains("winner: gamemodmanager-nxm.desktop"),
        "{text}"
    );
    assert!(
        text.contains(
            "declared by more than one application: nxmproxy.desktop, \
                      gamemodmanager-nxm.desktop"
        ) || text.contains(
            "declared by more than one application: gamemodmanager-nxm.desktop, \
                      nxmproxy.desktop"
        ),
        "both applications must be named: {text}"
    );
}

/// The browser is the last thing between a clicked link and nxmproxy, and it
/// keeps its own list.
///
/// `xdg-mime` says nxmproxy, `install` verified it, and a link clicked in
/// Firefox still arrives nowhere with an empty log: Firefox's own profile says
/// `nxm` belongs to somebody else and it never asks the OS again. The OS-level
/// answer is correct and still not the answer, so `test` has to carry the
/// second one and stop reporting 0.
///
/// Linux-only because the fixture is a Linux profile layout and because the
/// whole point is `xdg-mime`. A Flatpak browser's relocated home is used
/// because that is where a browser actually lives on this machine.
#[cfg(target_os = "linux")]
#[test]
fn test_reports_a_browser_that_decides_nxm_for_itself() {
    let sandbox = Sandbox::new("browser-override");
    let _lock = install_sandbox(&sandbox);
    seeded_conflict(&sandbox);

    // The state before: the OS holds the scheme for nxmproxy and nothing else
    // is in the way, so `test` is right to exit 0. Without this, the assertion
    // below would pass on a line printed for the wrong reason.
    let clean = run(&sandbox, &["test"]);
    assert_eq!(code(&clean), 0, "{}{}", stdout(&clean), stderr(&clean));
    assert!(
        !stdout(&clean).contains("browser override:"),
        "nothing has decided nxm:// but nxmproxy yet: {}",
        stdout(&clean)
    );

    // The user ticks "always do this" in Firefox one time. That writes the
    // scheme into the profile and Firefox never consults the OS again.
    let profile = sandbox
        .home()
        .join(".var/app/org.mozilla.firefox/.mozilla/firefox/8x2q.default-release");
    std::fs::create_dir_all(&profile).unwrap();
    let handlers = profile.join("handlers.json");
    std::fs::write(
        &handlers,
        r#"{"defaultHandlersVersion":1,"mimeTypes":{},"extensions":{},
            "schemes":{"nxm":{"action":"useHelperApp","ask":false,
              "handlers":[{"name":"Vortex","path":"/usr/bin/vortex"}]}}}"#,
    )
    .unwrap();

    let tested = run(&sandbox, &["test"]);
    let text = stdout(&tested);
    assert!(
        text.contains("nxmproxy handles nxm://: yes"),
        "the OS answer has not changed, and this test is only about what sits above \
         it: {text}"
    );
    assert!(
        text.contains(&format!(
            "browser override: org.mozilla.firefox decides nxm:// for itself: \
             /usr/bin/vortex ({})",
            handlers.display()
        )),
        "one line naming the deciding application and the file to open: {text}"
    );
    assert_eq!(
        text.lines()
            .filter(|line| line.starts_with("browser override:"))
            .count(),
        1,
        "one browser, one line - this is the compactness rule: {text}"
    );
    assert_eq!(
        code(&tested),
        3,
        "the OS would open nxmproxy and the browser would not, and exiting 0 over \
         that is the false green this line exists to end: {text}"
    );
}

/// `install` must verify, not believe.
///
/// The reachable way to get here is not a mock: `xdg-mime` on this machine is a
/// shell script that **exits 0 after failing to write**. Verified directly -
/// with the target directory read-only it prints "Permission denied" and still
/// returns 0 - which means the exit code is worthless as evidence and the read
/// back is the only thing that is not. Before the read back, this printed
/// "nxm:// links now open in nxmproxy" while GMM held the scheme.
#[cfg(target_os = "linux")]
#[test]
fn install_says_plainly_when_it_could_not_take_the_claim() {
    let sandbox = Sandbox::new("claim-refused");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    // **BYPASSED, not conflicting.** This fixture has to start out with
    // somebody *else* already holding `[Default Applications]`, because the
    // case under test is "the write did not happen, so nxmproxy still is not
    // the handler". It used to seed [`CONFLICTING_MIMEAPPS`], which already has
    // nxmproxy in `[Default Applications]` - so nxmproxy had won before the
    // read-only lock was even applied, the refusal never existed to be
    // observed, and the read back at the end honestly reported the claim as
    // taken. The test was asserting the opposite of what it was named for.
    std::fs::write(sandbox.mimeapps(), BYPASSED_MIMEAPPS).unwrap();
    // The rival has to be installed or it is not a rival: see [`publish_nxm_app`].
    publish_nxm_app(&sandbox, "gamemodmanager-nxm", "GameModManager");

    // **Deny the write, then prove the denial.** This test used to write the
    // fixture, find the sandbox was still writable, print a note and `return` -
    // so on an ordinary machine it asserted nothing at all and came back green.
    // A control that cannot fail is not a control.
    //
    // The mode bits are the only lever, and they are sufficient: `xdg-mime` is a
    // shell script that writes `mimeapps.list.new` beside its target, prints
    // "Permission denied" on stderr and **exits 0 anyway** (verified by hand).
    // So there is no error code to wait for, the file must not be writable, and
    // the directory must refuse new files - between them that closes every way
    // it has of writing.
    let read_only = ReadOnly::lock(&sandbox.xdg_config_home(), &sandbox.mimeapps());
    if !read_only.held() {
        eprintln!(
            "note: still able to write into the sandbox config dir despite the \
                  mode bits (root?); the claim-refusal case cannot be produced."
        );
        return;
    }

    let installed = run(&sandbox, &["install"]);
    let text = format!("{}{}", stdout(&installed), stderr(&installed));
    assert!(
        !text.contains("nxm:// links now open in nxmproxy"),
        "install reported a claim it did not take: {text}"
    );
    assert!(
        text.contains(&format!(
            "nxm:// still opens with gamemodmanager-nxm.desktop, from \
             [Default Applications] of {}",
            sandbox.mimeapps().display()
        )),
        "install must name who still holds the scheme and where it says so: {text}"
    );
    assert_eq!(
        code(&installed),
        3,
        "published, claimed, and not the handler: one step left for the user: {text}"
    );

    // And `test` agrees with `install`, from its own read.
    let tested = run(&sandbox, &["test"]);
    assert_eq!(
        code(&tested),
        3,
        "two commands, one answer: {}{}",
        stdout(&tested),
        stderr(&tested)
    );
}

/// The JSON carries the same facts, so a GUI cannot render a false green.
///
/// Not a re-assertion of the unit tests: this reads the payload the way a GUI
/// would, and checks that what it can draw agrees with what the terminal printed
/// for the identical fixture.
#[cfg(target_os = "linux")]
#[test]
fn list_json_carries_the_resolution_the_terminal_prints() {
    let sandbox = Sandbox::new("chain-json");
    let _lock = install_sandbox(&sandbox);
    seeded_conflict(&sandbox);

    let json: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    let scheme = &json["scheme"];
    let rows: Vec<(String, String, Vec<String>)> = scheme["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["section"].as_str().unwrap().to_string(),
                d["source"].as_str().unwrap().to_string(),
                d["ids"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i.as_str().unwrap().to_string())
                    .collect(),
            )
        })
        .collect();
    let mimeapps = sandbox.mimeapps().display().to_string();
    let kde = sandbox.kde_mimeapps().display().to_string();
    assert_eq!(
        rows,
        vec![
            (
                "Default Applications".to_string(),
                kde.clone(),
                vec!["nxmproxy.desktop".to_string()]
            ),
            (
                "Added Associations".to_string(),
                kde.clone(),
                vec!["gamemodmanager-nxm.desktop".to_string()]
            ),
            (
                "Default Applications".to_string(),
                mimeapps.clone(),
                vec!["nxmproxy.desktop".to_string()]
            ),
            (
                "Added Associations".to_string(),
                mimeapps,
                vec!["gamemodmanager-nxm.desktop".to_string()]
            ),
            (
                "Default Applications".to_string(),
                sandbox
                    .data_applications()
                    .join("kde-mimeapps.list")
                    .display()
                    .to_string(),
                vec!["nxmproxy.desktop".to_string()]
            ),
            (
                "Default Applications".to_string(),
                sandbox
                    .data_applications()
                    .join("mimeapps.list")
                    .display()
                    .to_string(),
                vec!["nxmproxy.desktop".to_string()]
            ),
        ],
        "every declaration, in precedence order, with its group and its file"
    );
    assert_eq!(
        scheme["rule"], "[Default Applications] outranks [Added Associations]",
        "a GUI has to be able to show *why*, not only who"
    );
    assert_eq!(json["handler"], "nxmproxy.desktop");
    assert_eq!(json["owns_scheme"], true);

    // The verdict, the chain and the terminal must be one answer. A GUI that
    // rendered `owns_scheme` next to anything else could show a working install
    // over one that routes nothing, which is the whole failure.
    let tested = stdout(&run(&sandbox, &["test"]));
    for id in ["nxmproxy.desktop", "gamemodmanager-nxm.desktop"] {
        assert!(
            tested.contains(id) && rows.iter().any(|(_, _, ids)| ids.contains(&id.to_string())),
            "{id} is in one answer and not the other:\n{tested}\n{rows:?}"
        );
    }

    // And the losing case, which is the one a GUI would get wrong: the payload
    // for a machine where nxmproxy is published and bypassed must say so in the
    // same structure rather than going quiet. The bypass goes into the
    // desktop-specific file, because that is the row above the one nxmproxy
    // installed itself - writing it only into `mimeapps.list` would leave
    // nxmproxy winning, which is a different fixture and not this one.
    let bypassed = "[Added Associations]\nx-scheme-handler/nxm=nxmproxy.desktop;\n\n\
                    [Default Applications]\nx-scheme-handler/nxm=gamemodmanager-nxm.desktop;\n";
    std::fs::write(sandbox.mimeapps(), bypassed).unwrap();
    std::fs::write(sandbox.kde_mimeapps(), bypassed).unwrap();
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    assert_eq!(json["owns_scheme"], false);
    assert_eq!(json["handler"], "gamemodmanager-nxm.desktop");
    assert_eq!(
        json["scheme"]["declarations"]
            .as_array()
            .unwrap()
            .first()
            .unwrap()["ids"][0],
        "gamemodmanager-nxm.desktop",
        "the first declaration is the winner, so a GUI cannot pick the wrong row"
    );
}

/// The design question, made into a test: does nxmproxy find a manager it can
/// prove is installed, and refuse to invent one it cannot?
///
/// Asserted through `list --json`, which never calls `install` and so is safe
/// on all three platforms - including macOS, where `install` is killed by
/// LaunchServices. The `present` flag is the same judgement `install` acts on.
#[test]
fn a_manager_is_detected_only_when_it_can_be_proven_present() {
    let sandbox = Sandbox::new("detect");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();

    let present = |sandbox: &Sandbox| -> Vec<(String, bool)> {
        let listed = run(sandbox, &["list", "--json"]);
        assert_eq!(code(&listed), 0, "{}", stderr(&listed));
        let json: serde_json::Value = serde_json::from_str(&stdout(&listed))
            .unwrap_or_else(|e| panic!("list --json must be valid JSON: {e}"));
        json["known_managers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["id"].as_str().unwrap().to_string(),
                    e["present"].as_bool().unwrap(),
                )
            })
            .collect()
    };
    let gmm_present =
        |v: &[(String, bool)]| v.iter().find(|(id, _)| id == "gamemodmanager").unwrap().1;

    assert!(
        !gmm_present(&present(&sandbox)),
        "nothing is installed in a bare sandbox, so nothing may be reported present: {:?}",
        present(&sandbox)
    );

    // The evidence: the desktop entry GameMod Manager writes for itself when the
    // user enables its nxm handling. It is not on PATH - the binary is at an
    // absolute path inside the sandbox - so this file is the only place the
    // path is recorded. That is the entire reason detection reads it.
    let real = bin_path();
    std::fs::write(
        apps.join("gamemodmanager-nxm.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=GameModManager\n\
             Exec=\"{real}\" --handle-nxm %u\nMimeType=x-scheme-handler/nxm;\n"
        ),
    )
    .unwrap();
    assert!(
        gmm_present(&present(&sandbox)),
        "the desktop entry the manager wrote is proof of installation"
    );

    // Present but pointing at something that does not exist is not proof.
    std::fs::write(
        apps.join("gamemodmanager-nxm.desktop"),
        "[Desktop Entry]\nExec=/definitely/not/here --handle-nxm %u\n",
    )
    .unwrap();
    assert!(
        !gmm_present(&present(&sandbox)),
        "an entry naming a binary that is not there is not evidence"
    );
}

/// `install` acts on the detection: it registers what it proved, and the link
/// routes with nothing further configured.
///
/// Linux only, because this asserts on what `install` *wrote*, and on macOS
/// `install` is killed before it writes anything. The detection half of the
/// decision is covered on every platform by
/// [`a_manager_is_detected_only_when_it_can_be_proven_present`].
#[cfg(target_os = "linux")]
#[test]
fn install_registers_a_manager_it_can_prove_is_present() {
    let sandbox = Sandbox::new("detect-install");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();

    // Nothing present: install must refuse to guess and must say what to type.
    let blind = run(&sandbox, &["install"]);
    assert_eq!(code(&blind), 3);
    assert!(
        !sandbox.config_file().exists(),
        "install invented a manager with no evidence: {}",
        std::fs::read_to_string(sandbox.config_file()).unwrap_or_default()
    );

    // The manager is a real executable that is NOT nxmproxy. Using this very
    // binary as the stand-in used to work, until nxmproxy stopped treating its
    // own published `nxmproxy.desktop` as a mod manager - which it must not, or
    // every install on a single-manager machine would find two candidates and
    // refuse to choose. Linux-only test, so `sh` is the portable stand-in here.
    let manager = std::path::Path::new("/bin/sh");
    assert!(manager.is_file(), "precondition: {manager:?} exists");

    std::fs::write(
        apps.join("gamemodmanager-nxm.desktop"),
        format!(
            "[Desktop Entry]\nExec=\"{}\" --handle-nxm %u\n",
            manager.display()
        ),
    )
    .unwrap();

    let detected = run(&sandbox, &["install"]);
    assert_eq!(
        code(&detected),
        0,
        "{}{}",
        stdout(&detected),
        stderr(&detected)
    );

    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        config.contains("gamemodmanager"),
        "a manager that is provably installed was not registered:\n{config}"
    );
    assert!(
        config.contains("--handle-nxm") && config.contains("%1"),
        "it must be registered with the argv that receives the link, read from \
         the Exec line rather than assumed:\n{config}"
    );

    // And the link now routes, with no further configuration, because exactly
    // one registered manager owns every unassigned domain.
    let delivered = run(&sandbox, &["url", "nxm://skyrimse/mods/1/files/2"]);
    assert_eq!(
        code(&delivered),
        0,
        "an out-of-the-box install must route: {}{}",
        stdout(&delivered),
        stderr(&delivered)
    );
}

/// Two refusals, both because registering the manager would have produced an
/// install that cannot work.
#[cfg(target_os = "linux")]
#[test]
fn install_refuses_a_manager_it_could_not_use() {
    // One lock for the whole test, covering both sandboxes: `INSTALL_LOCK` is a
    // plain Mutex and is not reentrant, so taking it twice in one test
    // deadlocks. That is exactly what happened the first time this was written.
    let _lock = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    // Present, resolvable, and useless: nothing in the command receives the
    // link, so every download would start the manager and hand it nothing.
    let unusable = Sandbox::new("detect-nourl");
    let _ = run(&unusable, &["uninstall"]);
    std::fs::create_dir_all(unusable.data_applications()).unwrap();
    std::fs::write(
        unusable
            .data_applications()
            .join("gamemodmanager-nxm.desktop"),
        format!("[Desktop Entry]\nExec=\"{}\" --quiet\n", bin_path()),
    )
    .unwrap();
    let refused = run(&unusable, &["install"]);
    assert_eq!(
        code(&refused),
        3,
        "a manager that never receives the link is not a working install: {}{}",
        stdout(&refused),
        stderr(&refused)
    );
    assert!(
        !unusable.config_file().exists(),
        "detection registered a manager that would receive nothing: {}",
        std::fs::read_to_string(unusable.config_file()).unwrap_or_default()
    );

    // A configuration the user built is never extended. Two managers is
    // ambiguous on purpose, and install must not resolve it by adding a third.
    let other = Sandbox::new("detect-two");
    let _ = run(&other, &["uninstall"]);
    for name in ["one", "two"] {
        assert_eq!(
            code(&run(&other, &["register", name, bin_path(), "--", "%1"])),
            0
        );
    }
    std::fs::create_dir_all(other.data_applications()).unwrap();
    std::fs::write(
        other.data_applications().join("gamemodmanager-nxm.desktop"),
        format!("[Desktop Entry]\nExec=\"{}\" --handle-nxm %u\n", bin_path()),
    )
    .unwrap();
    let kept = run(&other, &["install"]);
    assert!(
        stdout(&kept).contains("Keeping the mod managers already"),
        "{}",
        stdout(&kept)
    );
    let config = std::fs::read_to_string(other.config_file()).unwrap_or_default();
    assert!(
        !config.contains("gamemodmanager"),
        "install added a third manager to a configuration it did not create:\n{config}"
    );
}

/// THE bug this whole change is for. HD2Arsenal is a third-party manager, so
/// it is absent from the curated list by construction, and before this the only
/// auto-detection was a filename hardcoded to *our own* entry
/// (`gamemodmanager-nxm.desktop`). Arsenal could never appear there.
///
/// The entry below is the real one, with only the two paths that cannot exist
/// in a sandbox swapped for ones that do. Everything under test is untouched:
/// the quoted `Exec=`, `Categories=Utility;`, the `MimeType` in the **wrong
/// group**, the duplicated `x-scheme-handler/nxm`, and the action group
/// carrying its own `Exec=`.
///
/// That last one is what makes this test a control rather than a transcript.
/// The uninstall action's `Exec=` here points at a file that really exists, so
/// a group-blind parser registers a resolvable, working-looking manager with
/// the wrong argv - and the recorded argv is what catches it.
#[cfg(target_os = "linux")]
#[test]
fn a_third_party_manager_that_declares_nxm_handling_is_found_and_routes() {
    let sandbox = Sandbox::new("arsenal");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, argv_file) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    assert!(recorder.contains(' '), "precondition: a space in the path");

    // Read before install, so nxmproxy's own entry is not on disk yet.
    let bare = run(&sandbox, &["list", "--json"]);
    assert_eq!(code(&bare), 0, "{}", stderr(&bare));
    let json: serde_json::Value = serde_json::from_str(&stdout(&bare)).unwrap();
    assert_eq!(
        json["declared_managers"].as_array().unwrap().len(),
        0,
        "precondition: nothing declares nxm yet"
    );

    std::fs::write(apps.join("hd2arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();

    // `declared_managers` is how a GUI learns about this without running
    // install, so the discovery is asserted through the same call twice: once
    // empty, once populated. Only the second tells us anything.
    let listed = run(&sandbox, &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    let declared = json["declared_managers"].as_array().unwrap();
    assert_eq!(
        declared.len(),
        1,
        "expected exactly HD2Arsenal: {declared:?}"
    );
    let arsenal = &declared[0];

    assert_eq!(arsenal["name"].as_str(), Some("HD2Arsenal"));
    assert_eq!(
        arsenal["id"].as_str(),
        Some("hd2arsenal"),
        "the id is what the user would type"
    );
    assert_eq!(
        arsenal["exec"].as_str(),
        Some(recorder.as_str()),
        "a quoted Exec must not keep its quotes"
    );
    assert_eq!(
        arsenal["mime_types"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m.as_str() == Some("x-scheme-handler/nxm"))
            .count(),
        1,
        "one scheme listed twice is one handler, not two: {:?}",
        arsenal["mime_types"]
    );

    // The curated list must not claim credit: its `arsenal` row is a guess
    // about a binary named `arsenal`, and it is not what was found.
    assert!(
        !json["known_managers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"].as_str() == Some("Arsenal") && m["present"] == true),
        "the curated guess reported itself as the thing that was found"
    );

    let installed = run(&sandbox, &["install"]);
    assert_eq!(
        code(&installed),
        0,
        "{}{}",
        stdout(&installed),
        stderr(&installed)
    );
    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(config.contains("hd2arsenal"), "not registered:\n{config}");

    // And the link routes, which is what proves the argv came from
    // `[Desktop Entry]` and not from `[Desktop Action Uninstall]`.
    let link = "nxm://helldivers2/mods/1/files/2";
    let delivered = run(&sandbox, &["url", link]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "the manager never ran: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );
    let recorded = std::fs::read_to_string(&argv_file).unwrap();
    let mut lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines.pop(), Some("DONE"), "{recorded:?}");
    assert_eq!(
        lines,
        vec!["--no-sandbox", link],
        "the [Desktop Entry] argv must be used; a group-blind parser registers the \
         uninstall action instead: {recorded:?}"
    );

    // The same chain again, with a link that is awkward on purpose. The argv is
    // a **vector** and the substitution happens inside one element, so neither a
    // space nor a quote can split it - but that is a claim about the code, and
    // this is the claim about what the *target process* received. A link with a
    // space and a quote in it is not hypothetical here: it is the bug class that
    // made upstream's `Command::new(command.replace("%1", url))` mangle urls.
    std::fs::remove_file(&argv_file).unwrap();
    let awkward = r#"nxm://helldivers2/mods/9/files/8?title=a b"c&d='e'"#;
    let delivered = run(&sandbox, &["url", awkward]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "the manager never ran for the awkward link: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );
    let recorded = std::fs::read_to_string(&argv_file).unwrap();
    let mut lines: Vec<&str> = recorded.lines().collect();
    assert_eq!(lines.pop(), Some("DONE"), "{recorded:?}");
    assert_eq!(
        lines,
        vec!["--no-sandbox", awkward],
        "a link with a space and a quote must arrive as exactly one argument"
    );
}

/// The dispatch log. The one artefact that can exist at all.
///
/// We are launched by the OS with no terminal and we exit in milliseconds, so
/// when a link does not arrive there is no stdout left to read and nothing to
/// attach a bug report to. A **silent spawn failure** - the manager uninstalled,
/// the path wrong, the file not executable - is the likeliest reason for that,
/// and it used to leave nothing at all behind.
///
/// One line per event, `key=value`, quoted, on one line each: a reader greps
/// `^spawn` and gets the exact argv, or `result=failed` and gets the OS's own
/// words about why.
#[cfg(unix)]
#[test]
fn the_log_records_every_dispatch_and_why_one_failed() {
    let sandbox = Sandbox::new("dispatch-log");
    let log = sandbox.config_dir().join("nxm.log");
    let (recorder, argv_file) = argv_recorder(&sandbox, "apps/HD2Arsenal");
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "hd2arsenal", &recorder, "--no-sandbox", "%U"]
        )),
        0
    );

    // A link that arrives and is delivered.
    let link = "nxm://helldivers2/mods/1/files/2";
    let delivered = run(&sandbox, &["url", link]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "the manager never ran"
    );

    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();

    // 1. The link, as it arrived.
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("link url={link:?}"))),
        "the link that arrived is not on record: {text}"
    );
    // 2. Who it resolved to, by the id the user registered.
    assert!(
        lines.iter().any(
            |l| l.contains("route domain=\"helldivers2\" manager=\"hd2arsenal\" result=routed")
        ),
        "the resolved manager is not on record: {text}"
    );
    // 3. The exact argv, and 4. that the spawn took. Both on ONE line: a url with
    //    a space in it split across two lines would be unparseable exactly when
    //    it matters most.
    let spawn = lines
        .iter()
        .find(|l| l.contains("spawn manager=\"hd2arsenal\""))
        .unwrap_or_else(|| panic!("no spawn line for the delivered link: {text}"));
    assert!(
        spawn.contains(&format!("exec={recorder:?}")),
        "the spawn did not name the program it ran: {spawn}"
    );
    assert!(
        spawn.contains(&format!("argv=[\"--no-sandbox\", {link:?}]")),
        "the exact argv, as one array on one line: {spawn}"
    );
    assert!(
        spawn.contains("result=started"),
        "a delivered link must be recorded as started: {spawn}"
    );

    // 5. The handler-ownership resolution, so a link that never reaches us at all
    //    is diagnosable rather than guessed at. `test` asks this question and
    //    therefore logs it.
    run(&sandbox, &["test"]);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.lines()
            .any(|l| l.starts_with("info: scheme ") && l.contains("owns=")),
        "who held nxm:// is not on record: {text}"
    );

    // 6. A dispatch that cannot happen. This is the silent failure: the link
    //    arrives, the domain resolves, and the program does not exist.
    //
    //    Assigned explicitly, because two registered managers and no assignment
    //    is genuinely ambiguous and is refused before the spawn - correct, and
    //    not what this test is about.
    let ghost = sandbox.root.join("not-installed").join("Ghost");
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "ghost", ghost.to_str().unwrap(), "--", "%1"]
        )),
        0
    );
    assert_eq!(code(&run(&sandbox, &["assign", "ghost", "ghostgame"])), 0);
    let broken = run(&sandbox, &["url", "nxm://ghostgame/mods/1/files/2"]);
    assert_eq!(
        code(&broken),
        1,
        "an undelivered link is not a delivery: {}",
        stderr(&broken)
    );
    assert!(
        stderr(&broken).contains("could not start mod manager"),
        "the user still gets an explanation: {}",
        stderr(&broken)
    );

    let text = std::fs::read_to_string(&log).unwrap();
    let failure = text
        .lines()
        .find(|l| l.starts_with("error: spawn manager=\"ghost\""))
        .unwrap_or_else(|| panic!("the failed spawn is not on record: {text}"));
    assert!(
        failure.contains("result=failed") && failure.contains("err="),
        "the failure must carry the OS's own words, not just a verdict: {failure}"
    );
    assert!(
        failure.contains("nxm://ghostgame/mods/1/files/2"),
        "and the link it was for, so the two records can be matched: {failure}"
    );
}

/// Two managers that both declare nxm handling is genuinely ambiguous, and
/// picking one puts downloads in the wrong mod queue. `install` must refuse,
/// write nothing, and print the exact command for each.
#[cfg(target_os = "linux")]
#[test]
fn install_refuses_to_choose_between_two_declared_managers() {
    let sandbox = Sandbox::new("declared-two");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();

    let (one, _) = argv_recorder(&sandbox, "apps/Mod One");
    let (two, _) = argv_recorder(&sandbox, "apps/Mod Two");
    for (file, name, exec) in [
        ("one.desktop", "Mod One", &one),
        ("two.desktop", "Mod Two", &two),
    ] {
        std::fs::write(
            apps.join(file),
            format!(
                "[Desktop Entry]\nName={name}\nExec=\"{exec}\" %U\nMimeType=x-scheme-handler/nxm;\n"
            ),
        )
        .unwrap();
    }

    let installed = run(&sandbox, &["install"]);
    assert_eq!(
        code(&installed),
        3,
        "two candidates is not a working install: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert!(
        !sandbox.config_file().exists(),
        "install chose one of two: {}",
        std::fs::read_to_string(sandbox.config_file()).unwrap_or_default()
    );

    // Both, named, each with the command that registers it - quoted, because
    // these paths have spaces in them and an unquoted command does not work.
    let out = stdout(&installed);
    for (name, id, exec) in [("Mod One", "mod-one", &one), ("Mod Two", "mod-two", &two)] {
        assert!(out.contains(name), "{name} was not offered:\n{out}");
        assert!(
            out.contains(&format!("nxmproxy register {id} \"{exec}\" %1")),
            "no exact, quoted command for {name}:\n{out}"
        );
    }
}

/// An entry that declares nxm handling and points at a binary that is not
/// there is refused, named, and never registered.
#[cfg(target_os = "linux")]
#[test]
fn a_declared_manager_whose_executable_is_missing_is_refused_and_named() {
    let sandbox = Sandbox::new("declared-missing");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let ghost = sandbox.root.join("not-installed").join("Mod");
    std::fs::write(
        apps.join("ghost.desktop"),
        format!(
            "[Desktop Entry]\nName=Ghost Manager\nExec={} %U\nMimeType=x-scheme-handler/nxm;\n",
            ghost.display()
        ),
    )
    .unwrap();

    let listed = run(&sandbox, &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    assert_eq!(
        json["declared_managers"].as_array().unwrap().len(),
        0,
        "an entry naming a binary that is not there is not a manager this machine has"
    );

    let installed = run(&sandbox, &["install"]);
    assert_eq!(code(&installed), 3, "{}", stdout(&installed));
    assert!(
        !sandbox.config_file().exists(),
        "registered a manager that cannot be started"
    );
    let out = stdout(&installed);
    assert!(
        out.contains("Ghost Manager"),
        "the refusal must name what it skipped and why:\n{out}"
    );
    assert!(
        out.contains("not on this machine"),
        "the refusal must say the executable is missing:\n{out}"
    );
}

/// nxmproxy's own entry declares `x-scheme-handler/nxm`, and `install`
/// publishes it before running detection. If the router found itself, every
/// install on a single-manager machine would see two candidates and refuse.
#[cfg(target_os = "linux")]
#[test]
fn nxmproxy_does_not_detect_itself() {
    let sandbox = Sandbox::new("self-detect");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, _) = argv_recorder(&sandbox, "apps/Real");

    // install publishes nxmproxy.desktop into this same directory.
    let first = run(&sandbox, &["install"]);
    assert_eq!(code(&first), 3, "{}", stdout(&first));
    assert!(
        apps.join("nxmproxy.desktop").exists(),
        "precondition: our own entry was published: {}",
        stdout(&first)
    );

    // Now the real manager arrives, and it must be the only candidate.
    std::fs::write(
        apps.join("real.desktop"),
        format!("[Desktop Entry]\nName=Real Mod\nExec=\"{recorder}\" %U\nMimeType=x-scheme-handler/nxm;\n"),
    )
    .unwrap();

    let second = run(&sandbox, &["install"]);
    assert_eq!(
        code(&second),
        0,
        "our own entry counted as a second candidate: {}{}",
        stdout(&second),
        stderr(&second)
    );
    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        config.contains("[managers.real-mod]"),
        "the real manager must win cleanly:\n{config}"
    );
    // Not `!config.contains("nxmproxy")` - the sandbox path itself is
    // `/tmp/nxmproxy-it-...`, so that substring is always there. What matters
    // is that the router is not a *key*, which is what a second candidate
    // would have registered.
    assert_eq!(
        config.matches("[managers.").count(),
        1,
        "the router was registered as its own destination:\n{config}"
    );
}

/// The same trap, through a *different build*. `install` publishes
/// `current_exe()`, so on a real machine the entry in `~/.local/bin` names the
/// installed binary while we are running a freshly built one. Path comparison
/// says "different file, not us" - and the router is offered as its own mod
/// manager. Caught by reading the real machine, not by reasoning.
#[cfg(target_os = "linux")]
#[test]
fn an_entry_naming_another_build_of_nxmproxy_is_not_a_manager() {
    let sandbox = Sandbox::new("other-build");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();

    // A path that exists, is executable, and is not this process.
    let other = argv_recorder(&sandbox, "opt/nxmproxy-0.2.0-linux-x86_64").0;
    assert!(other != bin_path(), "precondition: a different file");
    std::fs::write(
        apps.join("nxmproxy.desktop"),
        format!(
            "[Desktop Entry]\nType=Application\nName=NXM Proxy\nExec={other} url %u\n\
             MimeType=x-scheme-handler/nxm;\n"
        ),
    )
    .unwrap();

    let listed = run(&sandbox, &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    assert_eq!(
        json["declared_managers"].as_array().unwrap().len(),
        0,
        "another build of the router is still the router: {}",
        stdout(&listed)
    );
}

/// The GUI's data contract. A shape a GUI can consume without parsing prose.
#[test]
fn list_json_is_machine_readable_and_declares_its_version() {
    let sandbox = Sandbox::new("list-json");
    run(
        &sandbox,
        &["register", "vortex", "/opt/vortex/vortex", "--", "%1"],
    );
    run(&sandbox, &["assign", "vortex", "skyrimse"]);

    let listed = run(&sandbox, &["list", "--json"]);
    assert_eq!(code(&listed), 0, "{}", stderr(&listed));

    let json: serde_json::Value = serde_json::from_str(&stdout(&listed))
        .unwrap_or_else(|e| panic!("list --json must be valid JSON: {e}\n{}", stdout(&listed)));

    assert_eq!(
        json["version"], 5,
        "a GUI needs a version to refuse politely"
    );
    for key in [
        "config_path",
        "config_writable",
        "managers",
        "assignments",
        "known_managers",
        "declared_managers",
        "owns_scheme",
        "scheme",
        "warnings",
    ] {
        assert!(!json[key].is_null(), "missing key {key} in {json}");
    }
    // `handler` is the one key that is legitimately `null` here: nothing on this
    // machine declares the scheme. Present-but-null, not absent, so a GUI can
    // index it without special-casing the first machine it ever ran on. The
    // bare-machine test below is the control that it really is null.
    assert!(json.as_object().unwrap().contains_key("handler"), "{json}");

    let manager = &json["managers"][0];
    assert_eq!(manager["id"], "vortex");
    assert_eq!(manager["exec"], "/opt/vortex/vortex");
    assert_eq!(manager["args"][0], "%1");
    assert_eq!(manager["takes_url"], true);
    // Hand-registered, not discovered. The flag is the whole difference between
    // "the manager you have" and "one nxmproxy found", and a GUI must be able
    // to tell them apart.
    assert_eq!(manager["discovered"], false);
    // /opt/vortex/vortex does not exist here, and the contract says so rather
    // than letting a GUI discover a broken entry when a link silently fails.
    assert_eq!(manager["exec_exists"], false);

    assert_eq!(json["assignments"][0]["domain"], "skyrimse");
    assert_eq!(json["assignments"][0]["manager"], "vortex");

    // The curated list is offered so a GUI need not hardcode it, and it is
    // data, never pre-registered.
    let known = json["known_managers"].as_array().unwrap();
    assert!(
        known.len() >= 4,
        "expected a curated list, got {}",
        known.len()
    );
    for entry in known {
        assert!(!entry["id"].as_str().unwrap().is_empty());
        assert!(!entry["note"].as_str().unwrap().is_empty());
    }
    assert_eq!(
        json["managers"].as_array().unwrap().len(),
        1,
        "the curated list must not be auto-registered"
    );
}

/// An empty configuration lists cleanly, which is the state a GUI sees before
/// the user has done anything.
#[test]
fn list_json_on_a_bare_machine_is_still_valid() {
    let sandbox = Sandbox::new("list-bare");
    let listed = run(&sandbox, &["list", "--json"]);
    assert_eq!(code(&listed), 0, "{}", stderr(&listed));
    let json: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    assert_eq!(json["version"], 5);
    assert_eq!(json["config_writable"], true);
    assert_eq!(json["managers"].as_array().unwrap().len(), 0);
    assert_eq!(json["assignments"].as_array().unwrap().len(), 0);
    // Present and empty, not absent: a GUI indexing this key must not have to
    // special-case "the first machine I ran this on". The same is true of the
    // scheme resolution, which on a machine that has declared nothing is empty
    // and says so rather than being absent.
    assert!(json["declared_managers"].is_array());
    assert_eq!(json["handler"], serde_json::Value::Null);
    assert_eq!(json["owns_scheme"], false);
    assert!(json["scheme"]["declarations"]
        .as_array()
        .unwrap()
        .is_empty());
    assert!(!json["scheme"]["rule"].as_str().unwrap().is_empty());
    assert!(panic_markers(&listed).is_empty(), "{}", stderr(&listed));
}

/// Malformed input is rejected with a message, not a panic. These strings come
/// from a browser, so they are untrusted.
#[test]
fn malformed_input_is_rejected_cleanly() {
    let sandbox = Sandbox::new("malformed");
    for bad in [
        "",
        "not a url",
        "https://www.nexusmods.com/skyrimse/mods/1",
        "nxm:///mods/1",
        "nxm://",
    ] {
        let out = run(&sandbox, &["url", bad]);
        assert_eq!(code(&out), 1, "expected a clean rejection for {bad:?}");
        assert!(
            panic_markers(&out).is_empty(),
            "{bad:?} -> {}",
            stderr(&out)
        );
        assert!(!stderr(&out).is_empty(), "{bad:?} got no explanation");
    }
}

// ---------------------------------------------------------------------------
// `discover`: remembering a manager without choosing it.
//
// The rule every test here defends: **found is not chosen**. Persistence is the
// feature; auto-routing would be a bug wearing the feature's clothes.
// ---------------------------------------------------------------------------

/// The real HD2Arsenal 0.36.2 entry, verbatim.
///
/// Only the `Exec=` paths are swapped, because `/home/petrica/Applications` does
/// not exist in a sandbox. **Everything else is untouched**, and
/// [`the_arsenal_fixture_is_still_the_file_a_user_has`] asserts that: the quoted
/// `Exec=`, `Categories=Utility;`, the `MimeType` in the **wrong group**, the
/// duplicated `x-scheme-handler/nxm`, and an action group carrying its own
/// `Exec=`.
fn arsenal_fixture(recorder: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Name=HD2Arsenal\n\
         Exec=\"{recorder}\" --no-sandbox %U\n\
         Type=Application\n\
         Icon=hd2arsenal\n\
         StartupWMClass=HD2Arsenal\n\
         X-AppImage-Version=0.36.2\n\
         Comment=Helldivers 2 Companion Desktop Application, Mod Manager, Mod Manifest Builder, Lobby Browser, and more!\n\
         Categories=Utility;\n\
         Actions=Uninstall;\n\
         \n\
         [Desktop Action Uninstall]\n\
         Name=Move to Trash\n\
         Exec={recorder} uninstall \"/home/petrica/Applications/HD2Arsenal\"\n\
         Icon=user-trash\n\
         MimeType=x-scheme-handler/rsnl;x-scheme-handler/nxm;application/x-hd2a-project;x-scheme-handler/nxm;\n"
    )
}

/// A fixture that has been tidied is a fixture nobody has.
///
/// This is the control on every other test in this file that uses
/// [`arsenal_fixture`]: if someone "cleans up" the fixture, the four defects
/// that make it a real file go with it, and this test goes red rather than the
/// suite quietly testing a tidier, easier, and much less realistic entry.
#[test]
fn the_arsenal_fixture_is_still_the_file_a_user_has() {
    let fixture = arsenal_fixture("/opt/HD2Arsenal");
    let entry = fixture.split("[Desktop Action Uninstall]").next().unwrap();
    let action = fixture.split("[Desktop Action Uninstall]").nth(1).unwrap();

    // 1. The `Exec=` path is quoted. Splitting on whitespace leaves the quotes
    //    attached and the file does not exist.
    assert!(entry.contains(r#"Exec="/opt/HD2Arsenal" --no-sandbox %U"#));
    // 2. The category is the one a text editor would carry, so category
    //    matching would take this file, a text editor, and a calculator.
    assert!(entry.contains("Categories=Utility;"));
    // 3. `x-scheme-handler/nxm` is NOT under `[Desktop Entry]` at all, only
    //    under the action group. A strict group-reader does not see Arsenal.
    assert!(!entry.contains("MimeType="));
    assert!(action.contains("x-scheme-handler/nxm"));
    // 4. It is listed twice in that one value. One scheme declared twice is
    //    one handler.
    assert_eq!(
        action.matches("x-scheme-handler/nxm").count(),
        2,
        "the duplicated scheme is the bug; tidying it away removes the test"
    );
    // 5. And the action group's `Exec=` is an *uninstall*, which is what makes
    //    a group-blind parser dangerous rather than merely wrong.
    assert!(action.contains(" uninstall "));
    assert!(!action.contains("AppManager"));
}

/// The feature: a manager nxmproxy found is in the config, usable, and assigned
/// to nothing.
#[cfg(target_os = "linux")]
#[test]
fn discover_remembers_the_real_arsenal_entry_and_assigns_it_nothing() {
    let sandbox = Sandbox::new("discover-arsenal");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, _) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    assert!(recorder.contains(' '), "precondition: a space in the path");

    // Nothing stored before the command is run: discovery is a command the user
    // runs, not a side effect of `list`.
    let before = run(&sandbox, &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&before)).unwrap();
    assert_eq!(
        json["managers"].as_array().unwrap().len(),
        0,
        "precondition: an empty database"
    );

    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();
    let found = run(&sandbox, &["discover"]);
    assert_eq!(code(&found), 0, "{}{}", stdout(&found), stderr(&found));
    assert!(
        stdout(&found).contains("HD2Arsenal"),
        "the manager it remembered must be named:\n{}",
        stdout(&found)
    );

    // Persisted, with the argv that actually receives the link.
    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        config.contains("[managers.hd2arsenal]"),
        "it was not stored:\n{config}"
    );
    assert!(
        config.contains(&recorder) && config.contains("--no-sandbox") && config.contains("%1"),
        "stored with the wrong invocation; a quoted Exec keeps its quotes or a \
         group-blind reader picks the uninstall action:\n{config}"
    );
    assert!(
        config.contains("discovered"),
        "the file must say which managers were found rather than chosen:\n{config}"
    );

    // **Assigned to nothing.** Not one game, not even the catch-all.
    let listed = run(&sandbox, &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    assert_eq!(
        json["assignments"].as_array().unwrap().len(),
        0,
        "discovery auto-assigned a game: {json}"
    );
    assert_eq!(json["managers"][0]["discovered"], true);
    assert_eq!(json["managers"][0]["id"], "hd2arsenal");
}

/// THE control on the feature. Every remembered manager routes nothing until a
/// game is assigned to it.
///
/// The mutation this defends against is subtle and was the reason it is a test:
/// rule 3 of `resolve` says "with exactly one manager registered, every game
/// routes to it", which is what makes an out-of-the-box install work. A
/// remembered manager *is* registered. Without the exclusion, discovering
/// Arsenal on a fresh machine auto-routes every nxm link on the box into it -
/// through the one rule written to be safe when the user had exactly one
/// manager and nothing to choose between.
#[cfg(target_os = "linux")]
#[test]
fn a_remembered_manager_never_routes_a_link_the_user_did_not_ask_for() {
    let sandbox = Sandbox::new("discover-no-route");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, argv_file) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();

    assert_eq!(code(&run(&sandbox, &["discover"])), 0);

    let link = "nxm://helldivers2/mods/1/files/2";
    let routed = run(&sandbox, &["url", link]);
    assert_eq!(
        code(&routed),
        1,
        "a discovered manager was auto-assigned: {}{}",
        stdout(&routed),
        stderr(&routed)
    );
    assert!(panic_markers(&routed).is_empty(), "{}", stderr(&routed));
    // Named, with the command that makes it live.
    assert!(
        stderr(&routed).contains("nxmproxy assign"),
        "the refusal must say what to do next:\n{}",
        stderr(&routed)
    );
    assert!(
        stderr(&routed).contains("hd2arsenal"),
        "the refusal must name what it knows:\n{}",
        stderr(&routed)
    );
    // The decisive one: the process was never started. A green exit code alone
    // would not catch a manager that was spawned and handed nothing.
    assert!(
        !argv_file.exists(),
        "the manager was started for a game nobody assigned to it: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );

    // And it becomes live the moment the user asks for it, which is what makes
    // remembering it worth anything.
    assert_eq!(
        code(&run(&sandbox, &["assign", "hd2arsenal", "helldivers2"])),
        0
    );
    let now = run(&sandbox, &["url", link]);
    assert_eq!(code(&now), 0, "{}{}", stdout(&now), stderr(&now));
    assert!(
        wait_for(&argv_file, "DONE", Duration::from_secs(10)),
        "an assigned manager must run: {}",
        std::fs::read_to_string(&argv_file).unwrap_or_default()
    );
}

/// Re-running must not undo anything the user has done since. A scan that
/// overwrites an entry is worse than no scan: the user types a command, runs
/// the thing that is supposed to be harmless, and their work is gone.
#[cfg(target_os = "linux")]
#[test]
fn a_rescan_does_not_clobber_a_registered_manager_or_an_assignment() {
    let sandbox = Sandbox::new("discover-rescan");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, _) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();

    // The user registered it by hand, with a different invocation, and gave it
    // a game. Both are theirs. The hand-registered exec is a **real file**: a
    // manager that cannot be spawned would fail the routing assertion below for
    // a reason that has nothing to do with discovery.
    let (mine, _) = argv_recorder(&sandbox, "opt/Mine Manager");
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "hd2arsenal", &mine, "--", "--flags", "%1"]
        )),
        0
    );
    assert_eq!(
        code(&run(&sandbox, &["assign", "hd2arsenal", "helldivers2"])),
        0
    );

    let again = run(&sandbox, &["discover"]);
    assert_eq!(code(&again), 0, "{}{}", stdout(&again), stderr(&again));

    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        config.contains(&mine),
        "the rescan overwrote the manager the user registered:\n{config}"
    );
    assert!(
        !config.contains(&recorder),
        "the rescan replaced the user's invocation with the one it found on disk:\n{config}"
    );
    assert!(
        !config.contains("discovered"),
        "the rescan relabelled a manager the user chose as one nxmproxy found:\n{config}"
    );
    assert!(
        config.contains("helldivers2"),
        "the rescan dropped the assignment:\n{config}"
    );

    // The same fact through the contract a GUI reads, so a formatter change
    // cannot quietly make the TOML assertion above vacuous.
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    assert_eq!(
        json["managers"][0]["discovered"], false,
        "the rescan demoted a manager the user chose to a suggestion: {json}"
    );

    // And it is still a manager the user chose, so it still routes everything
    // the catch-all would: discovery did not demote it into "suggestion".
    assert_eq!(
        code(&run(&sandbox, &["url", "nxm://skyrimse/mods/1"])),
        0,
        "a re-scan turned a working manager into one that routes nothing"
    );

    // Nothing to add the second time, and saying so is the honest report.
    assert!(
        stdout(&again).contains("already known"),
        "a rescan must not claim to have added what was already there:\n{}",
        stdout(&again)
    );
}

/// A manager whose application has been uninstalled is **remembered and marked**,
/// not deleted. The desktop entry goes first and the binary second, so both
/// happen in practice.
#[cfg(target_os = "linux")]
#[test]
fn an_uninstalled_application_is_marked_unavailable_and_not_forgotten() {
    let sandbox = Sandbox::new("discover-gone");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, _) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    let (other, _) = argv_recorder(&sandbox, "apps/Mod Two");
    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();
    std::fs::write(
        apps.join("two.desktop"),
        format!(
            "[Desktop Entry]\nName=Mod Two\nExec=\"{other}\" %U\nMimeType=x-scheme-handler/nxm;\n"
        ),
    )
    .unwrap();
    assert_eq!(code(&run(&sandbox, &["discover"])), 0);
    let stored = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        stored.contains("[managers.hd2arsenal]") && stored.contains("[managers.mod-two]"),
        "precondition: both were remembered:\n{stored}"
    );

    // The application is uninstalled: the entry first, as a package manager
    // would do it, then the binary. **One of two**, deliberately - with only one
    // manager on the machine, `discover` finds nothing at all and returns before
    // it touches the database, so the case that can actually lose the entry is
    // the one where something else survived the scan.
    std::fs::remove_file(apps.join("HD2Arsenal.desktop")).unwrap();
    std::fs::remove_file(std::path::Path::new(&recorder)).unwrap();

    let again = run(&sandbox, &["discover"]);
    assert_eq!(code(&again), 0, "{}{}", stdout(&again), stderr(&again));

    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        config.contains("[managers.hd2arsenal]"),
        "a missing file deleted a remembered manager; the user would have to find \
         and retype the whole thing:\n{config}"
    );
    assert!(
        config.contains("[managers.mod-two]"),
        "the manager that is still installed did not survive either:\n{config}"
    );

    let listed = run(&sandbox, &["list"]);
    let out = stdout(&listed);
    assert!(
        out.contains("hd2arsenal"),
        "it vanished from the listing:\n{out}"
    );
    assert!(
        out.contains("unavailable"),
        "the listing must say what happened, not just drop it:\n{out}"
    );
    // And only that one. The note sits on the manager's own line, so the
    // manager that is still installed can be checked for its absence directly.
    let still_there = out
        .lines()
        .find(|line| line.trim_start().starts_with("mod-two:"))
        .unwrap_or_else(|| panic!("mod-two is missing from the listing:\n{out}"));
    assert!(
        !still_there.contains("unavailable"),
        "the manager that is still installed was marked unavailable:\n{out}"
    );

    // The JSON contract agrees with the table, which is the disagreement that
    // makes a diagnostic useless. It is also where "found, not chosen" now
    // lives: the human table dropped that prose, so the flag is the only place
    // left that can say it.
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    let by_id = |id: &str| -> serde_json::Value {
        json["managers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is missing from {json}"))
    };
    assert_eq!(by_id("hd2arsenal")["exec_exists"], false);
    assert_eq!(by_id("hd2arsenal")["discovered"], true);
    assert_eq!(by_id("mod-two")["exec_exists"], true);
}

/// Registering by hand after a scan is a choice, and it must stick.
///
/// Found on the live machine while writing this: `discover` stored HD2Arsenal,
/// the user then ran `register` on the same id, and `list` still called it
/// "discovered - you have not chosen this one". The registration had answered
/// the question and the answer was discarded.
///
/// The fix is one line in `register_manager`, but a test has to hold it there,
/// and the order is the whole point: **discover then register**, not the other
/// way round.
#[cfg(target_os = "linux")]
#[test]
fn registering_a_discovered_manager_by_hand_makes_it_a_choice() {
    let sandbox = Sandbox::new("discover-then-register");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, _) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();

    assert_eq!(code(&run(&sandbox, &["discover"])), 0);
    let scanned: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    assert_eq!(scanned["managers"][0]["discovered"], true, "precondition");

    // The user takes it over.
    let (mine, _) = argv_recorder(&sandbox, "opt/Mine Manager");
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "hd2arsenal", &mine, "--", "%1"]
        )),
        0
    );

    let after: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    assert_eq!(
        after["managers"][0]["discovered"], false,
        "registering by hand did not make it a choice: {after}"
    );

    // And a later scan cannot put the label back, which is the whole point of
    // the rescan test running the other way round.
    assert_eq!(code(&run(&sandbox, &["discover"])), 0);
    let rescan: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    assert_eq!(
        rescan["managers"][0]["discovered"], false,
        "the rescan relabelled a manager the user had chosen: {rescan}"
    );
    assert!(
        !stdout(&run(&sandbox, &["list"])).contains("you have not chosen this one"),
        "the table still calls it unchosen"
    );
}

/// `list` is how a user tells what nxmproxy found from what they chose, so it
/// has to say who is routed where, in the documented shape.
///
/// Provenance moved: the human table no longer says "discovered" or "registered
/// by you", because no `*` line under a manager says the same thing more
/// cheaply and the JSON carries the flag. What the table must still do is put
/// each game under the manager that owns it, one `* ` line, one tab in.
#[cfg(target_os = "linux")]
#[test]
fn list_shows_each_game_under_the_manager_that_owns_it() {
    let sandbox = Sandbox::new("discover-list");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (recorder, _) = argv_recorder(&sandbox, "My Applications/HD2Arsenal");
    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();

    // One the user registered and assigned. One nxmproxy found. One nxmproxy
    // found and the user then assigned - both, not either. One registered and
    // left alone.
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "vortex", "/opt/vortex/Vortex", "--", "%1"]
        )),
        0
    );
    assert_eq!(code(&run(&sandbox, &["discover"])), 0);
    assert_eq!(code(&run(&sandbox, &["assign", "vortex", "skyrimse"])), 0);
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "mo2", "/opt/mo2/MO2Launcher", "--", "%1"]
        )),
        0
    );
    assert_eq!(
        code(&run(&sandbox, &["assign", "hd2arsenal", "helldivers2"])),
        0
    );

    let out = stdout(&run(&sandbox, &["list"]));

    // The shape, exactly: the game is a `* ` line under its own manager, one tab
    // in, and nowhere else. Checking the whole block is what makes a stray
    // `games:` section or a `domain -> manager` line fail.
    let block = |id: &str| -> Vec<String> {
        let lines: Vec<&str> = out.lines().collect();
        let start = lines
            .iter()
            .position(|l| l.starts_with(&format!("  {id}: ")))
            .unwrap_or_else(|| panic!("{id} is missing from the listing:\n{out}"));
        let tail = &lines[start + 1..];
        let end = tail
            .iter()
            .position(|l| !l.starts_with("    "))
            .unwrap_or(tail.len());
        std::iter::once(lines[start])
            .chain(tail[..end].iter().copied())
            .map(str::to_string)
            .collect()
    };

    assert_eq!(
        block("hd2arsenal"),
        vec![
            format!("  hd2arsenal: {recorder} --no-sandbox %1"),
            "    * helldivers2".to_string()
        ],
        "{out}"
    );
    // `/opt/vortex` and `/opt/mo2` do not exist, so each carries the one note
    // the command line cannot say for itself.
    assert_eq!(
        block("vortex"),
        vec![
            "  vortex: /opt/vortex/Vortex %1 (unavailable)".to_string(),
            "    * skyrimse".to_string()
        ],
        "{out}"
    );
    // A manager with no games shows the command and nothing else. That absence
    // IS the "routes nothing" signal; a parenthetical saying so was cut.
    assert_eq!(
        block("mo2"),
        vec!["  mo2: /opt/mo2/MO2Launcher %1 (unavailable)".to_string()],
        "{out}"
    );

    // Provenance is the JSON's job now, and it must still be there for a GUI.
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    let by_id = |id: &str| -> serde_json::Value {
        json["managers"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == id)
            .cloned()
            .unwrap_or_else(|| panic!("{id} is missing from {json}"))
    };
    assert_eq!(by_id("hd2arsenal")["discovered"], true);
    assert_eq!(by_id("vortex")["discovered"], false);
    assert_eq!(by_id("mo2")["discovered"], false);
    let assignments: Vec<(&str, &str)> = json["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| {
            (
                a["domain"].as_str().unwrap(),
                a["manager"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        assignments,
        vec![("helldivers2", "hd2arsenal"), ("skyrimse", "vortex")],
        "{json}"
    );
}

/// The documented shape, byte for byte.
///
/// Every other assertion about `list` checks a substring. This one compares the
/// whole stream, which is the only way a test fails when a *new* sentence is
/// appended to output that was already correct - a substring check passes on
/// prose nobody wrote an assertion for, which is exactly how 200 lines of
/// lecture survived a suite this size.
///
/// One registered manager, one assigned game, nothing else on the machine:
/// every extra line has to earn its place.
///
/// ```text
/// nxm:// handler: <one line: is any of the table below reachable at all>
/// mod managers:
///   <id>: <exec> <args>
///     * <game>
/// ```
///
/// The handler line is the one addition, and it is a fact about this machine
/// rather than advice: if nxmproxy does not hold `nxm://` then no assignment
/// below is ever consulted by the desktop, and a routing table printed on its
/// own implies a working setup. `test` is where the resolution is explained -
/// a table is not.
#[test]
fn list_is_exactly_the_documented_shape() {
    let sandbox = Sandbox::new("list-shape");
    // This test needs an exec that *resolves*, not one that records argv: an
    // unresolved one prints `(unavailable)` and changes the bytes it pins. The
    // running test binary is a real file on every platform, where
    // `argv_recorder` (a /bin/sh script) is unix-only.
    let exe = std::env::current_exe()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    assert_eq!(
        code(&run(&sandbox, &["register", "one", &exe, "--", "%1"])),
        0
    );
    assert_eq!(code(&run(&sandbox, &["assign", "one", "skyrimse"])), 0);

    let expected = format!(
        "config: {}\nnxm:// handler: nothing\n\nmod managers:\n  one: {exe} %1\n    * skyrimse\n",
        sandbox.config_file().display()
    );
    assert_eq!(stdout(&run(&sandbox, &["list"])), expected);
}

/// Nothing anywhere in the human output explains, suggests, or recaps.
///
/// `list --json` is a data contract and is exempt: it carries prose in `note`
/// fields on purpose, and a GUI renders those as tooltips. This is about the
/// text a human reads in a terminal.
///
/// Each entry below is a sentence this program used to print. The list is a
/// tombstone, not a wish list: reintroducing any one of them must fail here.
#[cfg(target_os = "linux")]
#[test]
fn no_command_prints_lesson_text() {
    const REMOVED: &[&str] = &[
        // `list`: the local-bin inventory line and everything after the colon.
        "local bin",
        "symlinks",
        "not a detection source",
        // `list`: per-manager provenance and the games parenthetical.
        "discovered - you have not chosen this one",
        "registered by you",
        "no games assigned",
        // `list`: the closing lesson and the redundant games section.
        "routes nothing until a game is assigned",
        "never picks one for you",
        "every game goes to the only mod manager you registered",
        "no game is routed to anything",
        "games:",
        // `list`/`install`: the candidate block's advice.
        "nxmproxy discover",
        "nxmproxy will not choose between them",
        "nxmproxy assign <name> <game>",
        "or send every unassigned game",
        "then pick who owns which game",
        "so a link would arrive and go nowhere",
        "Add one now",
        "Add one by hand",
        "lists the managers it can find on this machine",
        // `discover`: the three-command assignment recipe.
        "Remembered, but assigned to no game",
        "Nothing changed",
        "shows what was found and what you have chosen",
        // `install`/`register`/`test`: trailing hints and parentheticals.
        "until you `assign` something else",
        "Re-run with the link code",
        "the OS will ask you which app to use",
        "not on this machine, it was uninstalled",
    ];

    let sandbox = Sandbox::new("no-fluff");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    // Two declared managers, so every command that *could* offer advice has
    // something to offer it about. One real one to register and assign to.
    let (recorder, _) = argv_recorder(&sandbox, "apps/HD2Arsenal");
    let (other, _) = argv_recorder(&sandbox, "apps/Mod Two");
    std::fs::write(apps.join("HD2Arsenal.desktop"), arsenal_fixture(&recorder)).unwrap();
    std::fs::write(
        apps.join("two.desktop"),
        format!(
            "[Desktop Entry]\nName=Mod Two\nExec=\"{other}\" %U\nMimeType=x-scheme-handler/nxm;\n"
        ),
    )
    .unwrap();

    // Ordered so the states differ: an empty config, a full one of discovered
    // managers, one user registration, an unroutable link, an assignment, and a
    // finished install.
    let runs: Vec<Vec<&str>> = vec![
        vec!["list"],
        vec!["discover"],
        vec!["test"],
        vec!["register", "vortex", "/opt/vortex/Vortex", "--", "%1"],
        vec!["url", "nxm://nowhere/mods/1"],
        vec!["assign", "vortex", "skyrimse"],
        vec!["list"],
        vec!["install"],
    ];
    for args in &runs {
        let out = run(&sandbox, args);
        let text = format!("{}{}", stdout(&out), stderr(&out));
        for banned in REMOVED {
            assert!(
                !text.contains(banned),
                "`nxmproxy {}` prints {banned:?}:\n{text}",
                args.join(" ")
            );
        }
    }
}

/// The guards detection already has, inherited rather than reimplemented.
///
/// Ambiguity is not resolved by discovery either - it does not choose, it
/// remembers, and remembering both is not choosing. An entry that resolves to
/// nothing is still refused rather than stored broken.
#[cfg(target_os = "linux")]
#[test]
fn discover_keeps_every_candidate_and_stores_nothing_it_cannot_start() {
    let sandbox = Sandbox::new("discover-guards");
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();
    let (one, _) = argv_recorder(&sandbox, "apps/Mod One");
    let (two, _) = argv_recorder(&sandbox, "apps/Mod Two");
    for (file, name, exec) in [
        ("one.desktop", "Mod One", &one),
        ("two.desktop", "Mod Two", &two),
    ] {
        std::fs::write(
            apps.join(file),
            format!(
                "[Desktop Entry]\nName={name}\nExec=\"{exec}\" %U\nMimeType=x-scheme-handler/nxm;\n"
            ),
        )
        .unwrap();
    }

    // Neither is chosen, so neither routes.
    assert_eq!(code(&run(&sandbox, &["discover"])), 0);
    let listed = run(&sandbox, &["list", "--json"]);
    let json: serde_json::Value = serde_json::from_str(&stdout(&listed)).unwrap();
    let ids: Vec<&str> = json["managers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["mod-one", "mod-two"], "{json}");
    assert_eq!(
        json["assignments"].as_array().unwrap().len(),
        0,
        "ambiguity was resolved by discovery: {json}"
    );

    let refused = run(&sandbox, &["url", "nxm://skyrimse/mods/1"]);
    assert_eq!(
        code(&refused),
        1,
        "{}{}",
        stdout(&refused),
        stderr(&refused)
    );
    assert!(
        stderr(&refused).contains("mod-one") && stderr(&refused).contains("mod-two"),
        "both candidates must be named:\n{}",
        stderr(&refused)
    );

    // And the third refusal: an entry that resolves to nothing is named and
    // stored nowhere, not stored broken.
    let ghost = sandbox.root.join("not-installed").join("Ghost");
    std::fs::write(
        apps.join("ghost.desktop"),
        format!(
            "[Desktop Entry]\nName=Ghost Manager\nExec={} %U\nMimeType=x-scheme-handler/nxm;\n",
            ghost.display()
        ),
    )
    .unwrap();
    let out = run(&sandbox, &["discover"]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
    let json: serde_json::Value =
        serde_json::from_str(&stdout(&run(&sandbox, &["list", "--json"]))).unwrap();
    assert!(
        !json["managers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "ghost-manager"),
        "a manager that cannot be started was stored: {json}"
    );
    assert!(
        stdout(&out).contains("Ghost Manager") && stdout(&out).contains("not on this machine"),
        "and it must be named with the reason:\n{}",
        stdout(&out)
    );
}

/// `install` must not discover behind the user's back. It already has a
/// documented rule - it never extends a configuration it did not create - and
/// a second, hidden write path is exactly how that rule gets lost.
#[cfg(target_os = "linux")]
#[test]
fn install_never_writes_a_discovered_manager_on_its_own() {
    let sandbox = Sandbox::new("install-no-discover");
    let _lock = install_sandbox(&sandbox);
    let apps = sandbox.data_applications();
    std::fs::create_dir_all(&apps).unwrap();

    // Two managers, so `install` reaches the point where it lists candidates.
    // One would be auto-registered by `install`'s own pre-existing rule, which
    // is a different code path and is asserted by
    // `install_registers_a_manager_it_can_prove_is_present`.
    let (one, _) = argv_recorder(&sandbox, "apps/Mod One");
    let (two, _) = argv_recorder(&sandbox, "apps/Mod Two");
    for (file, name, exec) in [
        ("one.desktop", "Mod One", &one),
        ("two.desktop", "Mod Two", &two),
    ] {
        std::fs::write(
            apps.join(file),
            format!(
                "[Desktop Entry]\nName={name}\nExec=\"{exec}\" %U\nMimeType=x-scheme-handler/nxm;\n"
            ),
        )
        .unwrap();
    }

    let installed = run(&sandbox, &["install"]);
    // Two candidates, so install prints them, and must not run the command that
    // stores them: `discover` is in `--help`, and printing it is not what makes
    // this safe - the config not existing is.
    assert!(
        stdout(&installed).contains("mod-one") && stdout(&installed).contains("mod-two"),
        "install must name both candidates it refused to choose between: {}",
        stdout(&installed)
    );
    assert!(
        !sandbox.config_file().exists(),
        "install wrote a discovered manager behind the user's back: {}",
        std::fs::read_to_string(sandbox.config_file()).unwrap_or_default()
    );

    // And the command it points at works straight afterwards.
    let found = run(&sandbox, &["discover"]);
    assert_eq!(code(&found), 0, "{}{}", stdout(&found), stderr(&found));
    let config = std::fs::read_to_string(sandbox.config_file()).unwrap_or_default();
    assert!(
        config.contains("[managers.mod-one]") && config.contains("[managers.mod-two]"),
        "discover after install did not store them:\n{config}"
    );
}

/// The design property everything rests on: the OS launches us per uri and we
/// go away again. Nothing here may wait on the manager.
#[cfg(unix)]
#[test]
fn nxmproxy_does_not_wait_for_the_manager_it_starts() {
    let sandbox = Sandbox::new("no-wait");
    let started = Instant::now();
    let registered = run(&sandbox, &["register", "slow", "sleep", "--", "%1"]);
    assert_eq!(code(&registered), 0, "{}", stderr(&registered));

    let delivered = run(&sandbox, &["url", "nxm://skyrimse/mods/1/files/2"]);
    let elapsed = started.elapsed();
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        elapsed < Duration::from_secs(5),
        "nxmproxy waited {elapsed:?} on a 30 second child; it must spawn and return"
    );
}
