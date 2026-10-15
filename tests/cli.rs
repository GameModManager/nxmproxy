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
//! * `install` on Linux publishes a desktop entry and touches no association.
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

/// Wait, bounded, for `path` to appear. nxmproxy deliberately does not wait for
/// the manager it spawns, so the child may land slightly after we exit.
#[cfg(unix)]
fn wait_for(path: &Path, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    path.exists()
}

/// On unix, a script that records the exact argv it was called with, one
/// argument per line. This is how the quote test proves what the *target
/// process* receives rather than what our own function intended.
#[cfg(unix)]
fn argv_recorder(sandbox: &Sandbox, name: &str) -> (String, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let script = sandbox.root.join(name);
    let out = sandbox.root.join(format!("{name}.argv"));
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nfor a in \"$@\"; do printf '%s\\n' \"$a\" >> '{}'; done\n",
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
    assert!(
        stderr(&none).contains("register-manager"),
        "{}",
        stderr(&none)
    );

    // Two managers and no assignment: guessing would deliver the link to the
    // wrong manager, so we refuse instead, and still do not panic.
    run(
        &sandbox,
        &["register-manager", "one", "/bin/true", "--", "%1"],
    );
    run(
        &sandbox,
        &["register-manager", "two", "/bin/true", "--", "%1"],
    );
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
        &[
            "register-manager",
            "recorder",
            &recorder,
            "--",
            "--flag",
            "%1",
        ],
    );
    assert_eq!(code(&registered), 0, "{}", stderr(&registered));

    let delivered = run(&sandbox, &["url", link]);
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        wait_for(&argv_file, Duration::from_secs(10)),
        "the manager never ran"
    );

    // One argument per line, so a split url would show up as extra lines.
    let recorded = std::fs::read_to_string(&argv_file).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
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

    let registered = run(
        &sandbox,
        &["register-manager", "only", "/bin/true", "--", "%1"],
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
    let assigned = run(
        &sandbox,
        &["register-manager", "v", "/bin/true", "--", "%1"],
    );
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

/// Linux registers by publishing one desktop entry with the right MimeType,
/// and writes no default association into any `mimeapps.list`. Upstream nxmproxy
/// had no Linux code at all, and GMM's own registration calls `xdg-mime default`
/// and force-propagates into three files - which is exactly how one mod manager
/// ends up buried under another.
#[cfg(target_os = "linux")]
#[test]
fn linux_install_publishes_a_desktop_entry_and_writes_no_default() {
    let sandbox = Sandbox::new("install-linux");
    let before = sandbox.all_files();

    let installed = run(&sandbox, &["install"]);
    assert_eq!(code(&installed), 0, "{}", stderr(&installed));

    let entry_path = sandbox
        .root
        .join("data")
        .join("applications")
        .join("nxmproxy.desktop");
    let entry = std::fs::read_to_string(&entry_path)
        .unwrap_or_else(|e| panic!("expected a desktop entry at {}: {e}", entry_path.display()));
    assert!(entry.contains("Type=Application"), "{entry}");
    assert!(
        entry.contains("MimeType=x-scheme-handler/nxm;"),
        "this is what puts us in the OS chooser: {entry}"
    );
    assert!(entry.contains(" url %u"), "{entry}");

    // The decisive assertion. Installing may produce exactly two things: our
    // desktop entry, and a desktop MIME cache rebuilt by `update-desktop-database`
    // (a cache of file contents, not a record of the user's choice). Nothing
    // else, and in particular no `mimeapps.list` of any kind.
    let added: Vec<String> = sandbox
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
    assert_eq!(
        added,
        vec!["data/applications/nxmproxy.desktop".to_string()],
        "install must touch nothing but its own desktop entry"
    );
    for path in sandbox.all_files() {
        let name = path.display().to_string();
        assert!(
            !name.contains("mimeapps.list"),
            "install wrote an association file, which is the behaviour this fork exists to stop: {name}"
        );
        // Belt and braces: even if some future path were named differently, no
        // file we created may record a default handler for the scheme.
        if let Ok(text) = std::fs::read_to_string(&path) {
            assert!(
                !text.contains("[Default Applications]"),
                "{name} records a default application, which would seize the scheme"
            );
        }
    }
    assert!(
        panic_markers(&installed).is_empty(),
        "{}",
        stderr(&installed)
    );
}

/// `test` after `install` must report registered, and `uninstall` must remove
/// exactly what `install` created and nothing else.
#[test]
fn install_and_uninstall_round_trip() {
    let sandbox = Sandbox::new("round-trip");

    let tested = run(&sandbox, &["test"]);
    assert_eq!(code(&tested), 1, "not installed means a non-zero exit code");

    let installed = run(&sandbox, &["install"]);
    assert_eq!(code(&installed), 0, "{}", stderr(&installed));
    let after = run(&sandbox, &["test"]);
    assert_eq!(code(&after), 0, "{}", stderr(&after));
    assert!(
        stdout(&after).contains("registered: true"),
        "{}",
        stdout(&after)
    );

    let uninstalled = run(&sandbox, &["uninstall"]);
    assert_eq!(code(&uninstalled), 0, "{}", stderr(&uninstalled));
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

/// Unregistering a manager must not leave assignments pointing at nothing:
/// upstream's `deregister` did exactly that, and the dangling id then made the
/// game unroutable forever.
#[test]
fn register_assign_unregister_round_trips_through_the_file() {
    let sandbox = Sandbox::new("config-round-trip");

    assert_eq!(
        code(&run(
            &sandbox,
            &[
                "register-manager",
                "vortex",
                "/opt/vortex/vortex",
                "--",
                "%1"
            ]
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
    assert_eq!(code(&run(&sandbox, &["deregister", "vortex"])), 0);
    let text = std::fs::read_to_string(sandbox.config_file()).unwrap();
    assert!(!text.contains("skyrimse"), "dangling assignment: {text}");
    assert!(!text.contains("vortex"), "manager not removed: {text}");

    assert_eq!(
        code(&run(&sandbox, &["deregister", "vortex"])),
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
        &[
            "register-manager",
            "vortex",
            "/opt/vortex/vortex",
            "--",
            "%1",
        ],
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
    let registered = run(&sandbox, &["register-manager", "slow", "sleep", "--", "%1"]);
    assert_eq!(code(&registered), 0, "{}", stderr(&registered));

    let delivered = run(&sandbox, &["url", "nxm://skyrimse/mods/1/files/2"]);
    let elapsed = started.elapsed();
    assert_eq!(code(&delivered), 0, "{}", stderr(&delivered));
    assert!(
        elapsed < Duration::from_secs(5),
        "nxmproxy waited {elapsed:?} on a 30 second child; it must spawn and return"
    );
}
