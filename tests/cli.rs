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

fn run(sandbox: &Sandbox, args: &[&str]) -> Output {
    Command::new(BIN)
        .args(args)
        .env("NXMPROXY_CONFIG_DIR", sandbox.config_dir())
        .env("HOME", sandbox.root.join("home"))
        .env("XDG_CONFIG_HOME", sandbox.root.join("home").join(".config"))
        .env("XDG_DATA_HOME", sandbox.root.join("data"))
        .env("XDG_DATA_DIRS", sandbox.root.join("data").join("dirs"))
        .env("XDG_CONFIG_DIRS", sandbox.root.join("home").join("etc-xdg"))
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

/// Like [`run`], but with `xdg-mime` unreachable.
///
/// This exists because it is a genuinely different code path. When `xdg-mime`
/// is on `PATH`, `install` delegates the whole edit to it and nxmproxy's own
/// file editor never runs - so a test suite on a normal Linux machine exercises
/// **none** of it. The fallback is what runs on a minimal or headless system,
/// and it is the code this project wrote by hand, so it is the code that most
/// needs proving. `xdg-mime` lives in `/usr/bin` alongside almost everything
/// else, so PATH is emptied rather than filtered.
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
fn bin_path() -> String {
    env!("CARGO_BIN_EXE_nxmproxy").to_string()
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
#[cfg(unix)]
fn argv_recorder(sandbox: &Sandbox, name: &str) -> (String, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let script = sandbox.root.join(name);
    let out = sandbox.root.join(format!("{name}.argv"));
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
    let registered = run(
        &sandbox,
        &[
            "register",
            "only",
            env!("CARGO_BIN_EXE_nxmproxy"),
            "--",
            "list",
        ],
    );
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

/// Linux `install` claims `nxm://` in **exactly one file**, leaves every other
/// mod manager's entry byte for byte alone, and does not repeat the claim into
/// the two other files.
///
/// The three files are not a guess. GameMod Manager's own Linux registration
/// (`Core/src/platform/linux/linux_platform.cpp`, `set_mimeapps_defaults`) writes
/// `$XDG_CONFIG_HOME/kde-mimeapps.list`, `$XDG_CONFIG_HOME/mimeapps.list`, and
/// `$XDG_DATA_HOME/applications/mimeapps.list`. Forcing one claim into all three
/// is how a user with two mod managers ends up with one of them buried and no
/// way to see why. nxmproxy writes the middle one and stops.
#[cfg(target_os = "linux")]
#[test]
fn linux_install_claims_the_scheme_in_one_file_and_buries_nobody() {
    let sandbox = Sandbox::new("install-linux");
    let _lock = install_sandbox(&sandbox);

    // Pre-seed every file the behaviour under test could touch, so that
    // "left alone" is a real claim about content rather than about a file that
    // never existed.
    //
    // Vortex appears in `[Added Associations]`, which is *its own* record that
    // it handles nxm, and as the default for an unrelated scheme. It is
    // deliberately NOT seeded as the `[Default Applications]` value for nxm:
    // taking the scheme means replacing that value, which is precisely what
    // `xdg-mime default` does. What has to survive is everything else.
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();

    let other_manager = "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
                         [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
    std::fs::write(sandbox.mimeapps(), other_manager).unwrap();

    // GMM's propagation target. If we force-propagate like it does, this file
    // gains an nxm line and the test fails on content, not on existence.
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

    // 2. The claim itself: our id is the `[Default Applications]` value for the
    //    scheme. The lookup has to be group-aware, because the seeded file has
    //    an `x-scheme-handler/nxm` line in `[Added Associations]` too, and a
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

    // 3. THE decisive assertion: nobody else's entry moved. Every non-blank
    //    line that was in the file before must still be there, byte for byte.
    //    The seeded file holds no `[Default Applications]` nxm line, so there is
    //    nothing in it that taking the scheme was entitled to replace.
    for line in other_manager.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            claimed.contains(line),
            "install altered or dropped another mod manager's line {line:?}:\n{claimed}"
        );
    }

    // 4. The three-file propagation GMM commits must be absent. Both files are
    //    asserted on content, not existence, so this cannot pass by accident.
    assert!(
        !sandbox.kde_mimeapps().exists(),
        "install propagated into kde-mimeapps.list, which is the forced-spread \
         behaviour this fork exists to stop"
    );
    assert_eq!(
        std::fs::read_to_string(sandbox.data_applications().join("mimeapps.list")).unwrap(),
        data_mimeapps,
        "install propagated the claim into the data-level mimeapps.list; that file \
         must be left exactly as it was found"
    );

    // 5. And nothing else appeared at all. A cache rebuilt by
    //    `update-desktop-database` is a cache of file contents, not a record of
    //    the user's choice, so it is not in this list.
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
    assert_eq!(
        added,
        vec![
            "config/config.toml".to_string(),
            "data/applications/nxmproxy.desktop".to_string(),
        ],
        "install must create its desktop entry and nothing else"
    );
    assert!(panic_markers(&after).is_empty(), "{}", stderr(&after));
}

/// The hand-written claim path - the one used when `xdg-mime` is not installed -
/// makes the same promise about other mod managers, and does it in nxmproxy's
/// own file editor rather than a third party's.
///
/// A control proved this was the untested path: breaking the editor left this
/// file green while `xdg-mime` did the work on a normal machine. This is the
/// test that closes it, and it runs on every Linux CI run.
#[cfg(target_os = "linux")]
#[test]
fn the_fallback_claim_path_buries_nobody_either() {
    let sandbox = Sandbox::new("fallback");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();
    std::fs::create_dir_all(sandbox.data_applications()).unwrap();

    let other_manager = "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
                         [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n";
    std::fs::write(sandbox.mimeapps(), other_manager).unwrap();

    let installed = run_without_xdg_mime(&sandbox, &["install"]);
    assert_eq!(
        code(&installed),
        3,
        "no manager configured yet: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert!(
        stdout(&installed).contains("note:"),
        "the user must be told the desktop tool was not available: {}",
        stdout(&installed)
    );

    let claimed = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    assert_eq!(
        default_for_scheme(&claimed, "x-scheme-handler/nxm").as_deref(),
        Some("nxmproxy.desktop"),
        "the fallback did not claim the scheme:\n{claimed}"
    );
    for line in other_manager.lines().filter(|l| !l.trim().is_empty()) {
        assert!(
            claimed.contains(line),
            "the fallback altered another mod manager's line {line:?}:\n{claimed}"
        );
    }
    assert!(
        !sandbox.kde_mimeapps().exists(),
        "the fallback propagated into kde-mimeapps.list too"
    );

    // And it reverses, through the same editor.
    let removed = run_without_xdg_mime(&sandbox, &["uninstall"]);
    assert_eq!(code(&removed), 3);
    let after = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    assert_eq!(
        default_for_scheme(&after, "x-scheme-handler/nxm"),
        None,
        "the fallback left our claim behind:\n{after}"
    );
    assert!(
        after.contains("x-scheme-handler/nxm=vortex.desktop;")
            && after.contains("x-scheme-handler/http=firefox.desktop;"),
        "the fallback took another manager's entries with ours:\n{after}"
    );
}

/// `uninstall` reverses the claim: nxmproxy's own id goes and nothing else does.
///
/// This is the half that makes `install` safe to offer. A claim nxmproxy cannot
/// take back is indistinguishable from the greed it replaced.
///
/// Note what is deliberately NOT asserted: that Vortex is restored as the
/// default. It cannot be. `xdg-mime default` *replaces* the value rather than
/// appending to it, and nothing records what was there before. What survives is
/// Vortex's own `[Added Associations]` entry, which is how the desktop learns it
/// can offer Vortex again - and `uninstall` says so rather than pretending.
#[cfg(target_os = "linux")]
#[test]
fn linux_uninstall_removes_only_our_line() {
    let sandbox = Sandbox::new("uninstall-linux");
    let _lock = install_sandbox(&sandbox);
    std::fs::create_dir_all(sandbox.xdg_config_home()).unwrap();

    std::fs::write(
        sandbox.mimeapps(),
        "[Added Associations]\nx-scheme-handler/nxm=vortex.desktop;\n\n\
         [Default Applications]\nx-scheme-handler/http=firefox.desktop;\n",
    )
    .unwrap();

    assert_eq!(
        code(&run(&sandbox, &["install"])),
        3,
        "no manager yet, so not done"
    );

    let claimed = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    assert_eq!(
        default_for_scheme(&claimed, "x-scheme-handler/nxm").as_deref(),
        Some("nxmproxy.desktop"),
        "no claim was made:\n{claimed}"
    );

    let removed = run(&sandbox, &["uninstall"]);
    assert_eq!(
        code(&removed),
        3,
        "uninstall is honest that the default is not restored: {}{}",
        stdout(&removed),
        stderr(&removed)
    );

    let after = std::fs::read_to_string(sandbox.mimeapps()).unwrap();
    assert_eq!(
        default_for_scheme(&after, "x-scheme-handler/nxm"),
        None,
        "uninstall left our claim behind:\n{after}"
    );
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
    let exec = env!("CARGO_BIN_EXE_nxmproxy");

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
    for gone in ["register-manager", "unregister-manager", "deregister"] {
        let out = run(&sandbox, &[gone, "vortex", exec, "--", "%1"]);
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

/// `test` after `install` must report registered, and `uninstall` must remove
/// exactly what `install` created and nothing else.
///
/// This runs on all three platforms, which is deliberate: the *claim* is
/// platform-specific and mostly unclaimable on macOS and Windows, but the
/// publish/revert round trip and the exit-code contract are not, and a Linux
/// only build cannot see a broken `uninstall` on Windows at all.
#[test]
fn install_and_uninstall_round_trip() {
    let sandbox = Sandbox::new("round-trip");
    let _lock = install_sandbox(&sandbox);

    let tested = run(&sandbox, &["test"]);
    assert_eq!(code(&tested), 1, "not installed means a non-zero exit code");

    let installed = run(&sandbox, &["install"]);
    // No manager is registered in this sandbox and none can be detected, so
    // install has correctly left a step for the user. It must NOT report 0:
    // a script seeing 0 would consider an install that routes nothing done.
    assert_eq!(
        code(&installed),
        3,
        "an install with no mod manager is not done: {}{}",
        stdout(&installed),
        stderr(&installed)
    );
    assert!(
        stdout(&installed).contains("nxmproxy register"),
        "install must name the command to run: {}",
        stdout(&installed)
    );

    // With a manager present the same command succeeds outright.
    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "one", &bin_path(), "--", "%1"]
        )),
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
    // uninstall is honest too: Linux gives the scheme back and says so, the
    // other two cannot and say so. Neither may claim success.
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

/// The design question, made into a test: an install that routes nothing is a
/// broken install, so `install` must find a manager it can prove is here and
/// register it - and must never touch a configuration the user already built.
///
/// The proof is a desktop file the manager writes for itself, which is the only
/// signal that works for a QtIFW install that is not on `PATH`. Both halves
/// matter: a false positive routes links to a program that is not there, and a
/// false negative is only a printed command. So detection is allowed to refuse.
#[test]
fn install_registers_a_manager_it_can_prove_is_present() {
    let sandbox = Sandbox::new("detect");
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

    // Now the evidence appears: the desktop entry GameMod Manager writes for
    // itself when the user turns on its nxm handling. Its `Exec=` line names
    // the binary, which is the whole point - nxmproxy must not have to guess a
    // path, and must not register a bare command name that is not on PATH.
    let real = env!("CARGO_BIN_EXE_nxmproxy");
    let gmm_entry = format!(
        "[Desktop Entry]\nType=Application\nName=GameModManager\nExec=\"{real}\" --handle-nxm %u\n\
         MimeType=x-scheme-handler/nxm;\n"
    );
    std::fs::write(apps.join("gamemodmanager-nxm.desktop"), gmm_entry).unwrap();

    let detected = run(&sandbox, &["install"]);
    assert_eq!(
        code(&detected),
        if cfg!(target_os = "linux") { 0 } else { 3 },
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

    // A configuration the user built is never extended. Two managers is
    // ambiguous on purpose, and install must not resolve it by adding a third.
    let unusable = Sandbox::new("detect-nourl");
    std::fs::create_dir_all(unusable.data_applications()).unwrap();
    // Present, resolvable, and useless: nothing in the command receives the
    // link, so every download would start the manager and hand it nothing.
    std::fs::write(
        unusable
            .data_applications()
            .join("gamemodmanager-nxm.desktop"),
        format!("[Desktop Entry]\nExec=\"{}\" --quiet\n", bin_path()),
    )
    .unwrap();
    let _ = run(&unusable, &["uninstall"]);
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

    let other = Sandbox::new("detect-two");
    for name in ["one", "two"] {
        assert_eq!(
            code(&run(&other, &["register", name, &bin_path(), "--", "%1"])),
            0
        );
    }
    assert!(std::fs::create_dir_all(other.data_applications()).is_ok());
    std::fs::write(
        other.data_applications().join("gamemodmanager-nxm.desktop"),
        format!("[Desktop Entry]\nExec=\"{}\" --handle-nxm %u\n", bin_path()),
    )
    .unwrap();
    let _ = run(&other, &["uninstall"]);
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

/// Unregistering a manager must not leave assignments pointing at nothing:
/// upstream's `deregister` did exactly that, and the dangling id then made the
/// game unroutable forever.
#[test]
fn register_assign_unregister_round_trips_through_the_file() {
    let sandbox = Sandbox::new("config-round-trip");

    assert_eq!(
        code(&run(
            &sandbox,
            &["register", "vortex", "/opt/vortex/vortex", "--", "%1"]
        )),
        0
    );
    assert_eq!(code(&run(&sandbox, &["assign", "vortex", "skyrimse"])), 0);
    assert_eq!(code(&run(&sandbox, &["assign", "vortex", "_"])), 0);

    let text = std::fs::read_to_string(sandbox.config_file()).unwrap();
    assert!(text.contains("skyrimse"), "{text}");
    assert!(
        text.contains("/opt/vortex/vortex"),
        "sanity: the file really is the manager shape: {text}"
    );

    // The alias upstream used still works.
    assert_eq!(code(&run(&sandbox, &["unregister", "vortex"])), 0);
    let text = std::fs::read_to_string(sandbox.config_file()).unwrap();
    assert!(!text.contains("skyrimse"), "dangling assignment: {text}");
    assert!(!text.contains("vortex"), "manager not removed: {text}");

    assert_eq!(
        code(&run(&sandbox, &["unregister", "vortex"])),
        1,
        "nothing left to remove"
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
        json["version"], 1,
        "a GUI needs a version to refuse politely"
    );
    for key in [
        "config_path",
        "config_writable",
        "managers",
        "assignments",
        "known_managers",
        "warnings",
    ] {
        assert!(!json[key].is_null(), "missing key {key} in {json}");
    }

    let manager = &json["managers"][0];
    assert_eq!(manager["id"], "vortex");
    assert_eq!(manager["exec"], "/opt/vortex/vortex");
    assert_eq!(manager["args"][0], "%1");
    assert_eq!(manager["takes_url"], true);
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
    assert_eq!(json["version"], 1);
    assert_eq!(json["config_writable"], true);
    assert_eq!(json["managers"].as_array().unwrap().len(), 0);
    assert_eq!(json["assignments"].as_array().unwrap().len(), 0);
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
