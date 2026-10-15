# NXMProxy

Forwards Nexus Mods `nxm://` download links to the mod manager assigned for that game.

The operating system launches this binary once per link. It resolves the link,
spawns the manager with the link as one argument, and exits without waiting.
There is no daemon, no resident process and no service: the scheme registration
is the only thing that persists.

---

## Provenance

This is a **fork of [TanninOne/nxmproxy](https://github.com/TanninOne/nxmproxy)**,
upstream HEAD `f9f72e0`, tag `0.1.0`. That is Tanner Parnell's project, the author
of Mod Organizer 2, and the original was released under the GNU General Public
License v3. **This fork remains GPL-3.0-or-later and keeps upstream's `LICENSE.md`
verbatim.** Upstream's four commits are preserved in this repository's history;
everything after them is this fork.

The original is credited here, in the licence file, and in the module headers of
every source file. Upstream's own comparison to MO2's `nxmhandler`, and the notes
on named pipes, are retained below.

### Why fork rather than write fresh

Upstream's design is right and worth keeping: a mod manager is identified by an
executable plus an argument vector, the routing table is two maps, and the
process is short-lived and unprivileged. Its *implementation* is where the
problems are. Upstream is four commits from one week in September 2021, has no
tests, no CI, and does not compile anywhere but Windows.

---

## Divergences from upstream

Every item here is a deliberate change, with the reason.

### Fixed: things that crashed or corrupted the link

| Upstream | Here | Why |
| --- | --- | --- |
| `config.resolve(game).expect("Failed to find manager for game")` (`src/main.rs:83`) | An unroutable domain is a **value** with a message naming the domain and the command to type next | An `nxm` link for a game nobody configured is ordinary traffic. Upstream showed the user a crash dialog for it. |
| `toml::from_str(...).expect("Failed to parse game config")` (`src/config_file.rs:31`) | One bad byte degrades to an empty config plus a warning; **writes are refused** while the file is unparseable, so the file the user needs to repair is never overwritten | Upstream panicked the handler for every link on the machine. |
| `command.replace("%1", url)` **then** `CommandLineToArgvW` (`src/main.rs:98-100`) | The command is held as an **argv vector** and `%1` is substituted inside a single element | Upstream substituted into a string and then split it, so a URL containing `"` produced a different and wrong argument vector. See the failure below. |
| `Url::parse(url).expect("Failed to parse url")` | Validated, with the reason returned | The URL comes from a browser. It is untrusted input. |
| `.spawn().expect("Failed to spawn manager")` | Reported as a message, exit 1 | A missing executable is a configuration problem, not a crash. |
| `deregister` removed the manager but left `games` entries pointing at it (`src/config_file.rs:73-77`) | Removing a manager removes its assignments | A dangling id made that game unroutable forever, with no way to see why. |
| `install()` wrote `HKEY_CLASSES_ROOT` and, on `ERROR_ACCESS_DENIED`, **re-launched itself elevated** with the `runas` verb (`src/main.rs:122-129`, `src/win32.rs:130-163`) | A ProgID of our own under **`HKEY_CURRENT_USER`**. No elevation, anywhere | `HKCR` is the merged machine-plus-user view: it overwrites whatever Vortex or MO2 registered and affects every user on the machine. Upstream expected the write to fail and asked for UAC to install a per-user setting. |
| `test_installed` compared the handler exe name to the literal `"nxmproxy.exe"` (`src/main.rs:145`) | Compares against this executable's real path | The literal only ever matched an install with that exact name. |
| Config read was all-or-nothing | Per item: one invalid entry is dropped with a warning, the rest survive | Same failure as above, at finer grain. |
| Upstream had **no Linux and no macOS code at all** | Both platforms implemented | 176 of upstream's 560 lines were Win32. |
| `File::create(r"\\.\pipe\{name}")` on every platform (`src/pipe.rs:3-4`) | Windows only; other platforms return false | On POSIX that is not a special path, it is a **relative file name**, so every forwarded link would have dropped a stray file named `\\.\pipe\<name>` into whatever directory the desktop launched from. |

### Added

* **`list --json`**: a machine-readable contract for a GUI or another manager.
* **`register-manager` / `unregister-manager`**: a custom manager as
  `{exec, argv vector}`. Upstream's `register` took a command *string*, and its
  own README example (`README.md:11`, `c:\program files\...\vortex.exe`) was
  **unquoted with spaces in it**, so it could not have worked.
* **A curated list of known managers**, reported by `list --json`. Data only,
  never auto-registered.
* **Empty configuration works out of the box**: with exactly one manager
  registered, every domain routes to it with nothing assigned.
* **`uninstall`**, which upstream did not have.
* **Tests and CI.** Upstream had no tests at all.

### The rule all three platforms obey: register, never seize

Nothing here writes a default association. Not `xdg-mime default`, not
`LSSetDefaultHandlerForURLScheme`, not the bare `nxm` registry key. Seizing the
scheme is the exact behaviour this tool exists to fix, and a tool that fixes it
while doing the same thing on the way in is worse than the thing it replaces.

That means a user with two mod managers gets **a chooser**, and keeps the right
to change their mind.

---

## The quote failure, concretely

Upstream built a command string and split it afterwards. Feeding it a link with
a space in a query parameter produced this (this is the output of the negative
control in `tests/cli.rs`, not a guess):

```
argument vector: ["--flag", "nxm://somegame/mods/1/files/2?a=\"b\"&c='d'", "e"]
expected:        ["--flag", "nxm://somegame/mods/1/files/2?a=\"b\"&c='d' e f"]
```

The link arrives at the mod manager as **two arguments**, and the last one is
lost. The manager either rejects it or downloads the wrong thing. This fork
holds the command as a vector and substitutes into one element, so the URL
arrives byte for byte.

---

## Installation and registration

### Linux

`nxmproxy install` writes exactly one file:

```
~/.local/share/applications/nxmproxy.desktop
    MimeType=x-scheme-handler/nxm;
```

That line is what places nxmproxy in the list the desktop shows when an `nxm://`
link is opened. It **registers; it does not decide.** nxmproxy never runs
`xdg-mime default`, never writes a `[Default Applications]` group into any
`mimeapps.list`, and never force-propagates into another file.

This is enforced by a test, not by intention: `install` is run inside a
throwaway `HOME`/XDG tree and the test asserts that the only file that appears is
the desktop entry, and that **no `mimeapps.list` and no `[Default Applications]`
group exists anywhere**.

The one other thing `install` does is run `update-desktop-database` on the
directory, best effort, to refresh the desktop's cache of desktop files. That is
a cache, not a record of the user's choice.

### macOS — compiles, but the runtime behaviour is UNVERIFIED

`install` publishes a minimal `.app` bundle under
`~/Library/Application Support/nxmproxy/` with an `Info.plist` whose
`CFBundleURLTypes` declares `nxm`, then calls `lsregister -f` to make the bundle
**eligible**.

`LSSetDefaultHandlerForURLScheme` is deliberately **not** called. It is the
macOS equivalent of `xdg-mime default`: it assigns the scheme outright, there is
no "suggested", and it buries any other mod manager. `LSHandlerRank` is
deliberately left as `Alternate` rather than `Owner` or `Default` for the same
reason.

`nxmproxy test` cannot report the current default handler on macOS without
binding `LSCopyDefaultHandlerForURLScheme`. This fork does not bind it, so `test`
says so and points at System Settings rather than inventing a boolean.

> **Honest status.** The code was written on a machine with no macOS SDK, so it
> was never run there by its author. What is now verified: **it compiles on
> `aarch64-apple-darwin` and the unit tests pass on `macos-latest` in CI.**
> What is **not** verified, and needs a real Mac and a real click: that
> `lsregister` makes LaunchServices offer the bundle, in what order the entries
> appear in the chooser, that the `Alternate` rank has the intended effect, and
> what `test` reports on a machine where something else already owns `nxm://`.
> Every one of those is a LaunchServices behaviour, and nothing here exercises
> LaunchServices.

### Windows

`install` writes, all under `HKCU`:

| Key | Purpose |
| --- | --- |
| `HKCU\Software\Classes\GameModManager.NxmProxy` | Our ProgID. Friendly name `URL:NXM Protocol` |
| `...\GameModManager.NxmProxy` (value `URL Protocol`, empty) | Marks it as a scheme handler |
| `...\GameModManager.NxmProxy\DefaultIcon` | |
| `...\GameModManager.NxmProxy\shell\open\command` | `"<exe>" url "%1"` |
| `...\GameModManager.NxmProxy\SupportedTypes` (`nxm`) | |
| `...\GameModManager.NxmProxy\Capabilities` | |
| `...\Capabilities\URLAssociations` (`nxm`) | |
| `HKCU\Software\Microsoft\Windows\CurrentVersion\RegisteredApplications` | Puts us in Default Apps so the shell can offer a choice |

`SupportedTypes` plus `RegisteredApplications` plus `Capabilities` is the
documented shape that makes Windows **offer** the application rather than
silently taking the scheme, and it is why the "How do you want to open this?"
chooser can appear.

**Nothing under `HKEY_CLASSES_ROOT` is ever written.** `reg.exe` is used rather
than the Win32 API, which removes the `windows` crate and its build script, and
leaves the risky part (exact key names and quoting) as plain argument vectors that
are unit tested on every platform.

> **Honest status: the Windows path has never been executed against a Windows
> registry.** It compiles on `x86_64-pc-windows-msvc` and its unit tests pass on
> `windows-latest` in CI, and the key names, argument construction and per-user
> scope are asserted by tests. What is **not** verified: that `reg.exe` accepts
> and writes every key as intended, that Windows then offers nxmproxy in the
> `nxm://` chooser, and that the `RegisteredApplications` entry behaves. Those
> need a real machine and a real click.

---

## Command line

| Command | Description |
| --- | --- |
| `install` | Register nxmproxy as a handler for `nxm://`. Registers; does not seize. |
| `uninstall` | Remove what `install` created, and nothing else. |
| `test` | Report whether nxmproxy is registered and who the OS says handles `nxm://`. Exit 0 if registered, 1 if not. |
| `list [--json]` | Show the routing table. `--json` is the machine-readable contract. |
| `register-manager <name> <exec> [-- <args>...]` | Register a mod manager. Alias: `register`. |
| `unregister-manager <name>` | Remove a manager and its assignments. Alias: `deregister`. |
| `assign <manager> <domain>` | Assign a domain. `<domain>` is the `nexusmods.com` subdomain, or `_` for every game with no assignment. |
| `pipe <manager> <pipe>` | Give a manager a named pipe to try before spawning it. |
| `url <nxm url>` | Forward a link. This is what the OS invokes. |

`%1` inside any element of `args` is where the link goes. Use `--` before the
argument vector so a link or flag-looking argument is never parsed as an option.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Delivered, or the command succeeded. |
| 1 | Not delivered, or the command failed. **Never a panic.** |
| 2 | Bad command line (from `clap`). |

### Routing rules

1. An exact assignment for the domain.
2. The `_` catch-all.
3. **The only registered manager**, if there is exactly one.

Rule 3 is why an empty configuration works out of the box: a user with one mod
manager needs nothing configured. It deliberately does **not** fire when two or
more managers are registered. With two, there is no obvious owner, and a wrong
guess is a link landing in someone else's mod queue, so nxmproxy says so and
exits non-zero rather than guessing.

---

## `list --json`

Stable, versioned, and byte-stable across runs (`BTreeMap` plus declared field
order). Unknown keys are ignored by readers.

```json
{
  "version": 1,
  "config_path": "/home/u/.config/nxmproxy/config.toml",
  "config_writable": true,
  "managers": [
    {
      "id": "vortex",
      "exec": "/opt/vortex/Vortex",
      "args": ["%1"],
      "pipe": null,
      "takes_url": true,
      "exec_exists": true
    }
  ],
  "assignments": [
    { "domain": "skyrimse", "manager": "vortex" }
  ],
  "known_managers": [
    {
      "id": "vortex",
      "name": "Vortex Mod Manager",
      "exec": "C:\\Program Files (x86)\\Black Tree Gaming\\Vortex\\Vortex.exe",
      "args": ["%1"],
      "note": "Windows default install path. Also exposes a named pipe: vortex_download."
    }
  ],
  "warnings": []
}
```

| Field | Why it is there |
| --- | --- |
| `version` | A GUI must be able to refuse a shape it does not understand instead of misreading it. |
| `config_writable` | `false` when the file exists but does not parse. A GUI must **not** offer to save in that state, because saving would overwrite the file the user still needs to repair. |
| `managers[].takes_url` | `false` when no element of `args` contains `%1`, i.e. the manager would be launched and handed nothing. A GUI can show this before the user clicks a link. |
| `managers[].exec_exists` | Whether the executable is actually present on this machine, so a GUI can flag a broken entry instead of letting the user discover it when a link silently fails. |
| `known_managers` | The curated list, so a GUI need not hardcode it. **Data only, never registered.** Each entry carries a `note` saying what is and is not verified about it. |
| `warnings` | Anything dropped while loading, so the user is not left guessing. |

The shape resembles GameModManager's `gmm.supported-games/1` only in that both
are versioned JSON with an explicit version field. **The two projects are not
coupled**: nxmproxy has no dependency on GameModManager and never will.

---

## Configuration

Per-platform standard convention:

| Platform | Path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/nxmproxy/config.toml`, or `~/.config/nxmproxy/config.toml` |
| macOS | `~/Library/Application Support/nxmproxy/config.toml` |
| Windows | `%APPDATA%\nxmproxy\config.toml` |

Upstream used `%LOCALAPPDATA%`, which is where an application keeps *data*
rather than *settings*.

`NXMPROXY_CONFIG_DIR` overrides the location. That exists so tests and sandboxed
runs cannot touch a real configuration.

A log is written beside it as `nxm.log`. We are launched by the OS with no
terminal attached and exit in milliseconds, so when a link does not arrive the
only place the reason can live is there.

### Format

```toml
[games]
skyrimse = "vortex"
_ = "vortex"          # catch-all for every game with no assignment

[managers.vortex]
exec = "/opt/vortex/Vortex"   # a path, or a bare name resolved through PATH
args = ["%1"]                 # the link goes here
pipe = "vortex_download"      # optional, Windows only
```

**Writes are atomic**: temp file plus rename. A concurrent reader sees either the
old file or the new one, never a half-written one.

A manager is stored as an **argv vector**, not a command string. That is the fix
for the quote bug above, and it is also why an executable path containing spaces
is not a quoting problem any more.

---

## Building and testing

```sh
cargo build --release
cargo test
```

Tests are split deliberately:

* **Unit tests** live in the `src/` modules and cover the decision logic, the
  desktop entry text, the registry operation list and the config file handling.
* **`tests/cli.rs`** runs the real binary and asserts on what the user's terminal,
  the OS and the **target process** actually see. The quote test works by
  registering a script that records its own `argv` and asserting the link arrives
  byte for byte as one argument.

Every CLI test runs in a throwaway config directory and a throwaway
`HOME`/XDG tree, so no test can reach a real configuration.

### CI

`.github/workflows/ci.yml` runs `cargo fmt --check`, `clippy -D warnings`,
`cargo build --release` and `cargo test` on **Linux, macOS and Windows**.

> **What the CI run proves on macOS and Windows, precisely:** the crate compiles
> there, and every test that does not need a real OS passes. The unit tests for
> the macOS plist and the Windows registry operations are pure functions over
> text, so they run and pass on all three platforms.
>
> **What it does not prove:** that `lsregister` makes macOS offer the bundle, or
> that `reg.exe` writes the Windows keys as intended. The `tests/cli.rs`
> registration round trip runs on macOS and Windows but only exercises the
> platform module that can actually perform the write, so on those two it
> verifies the CLI contract rather than the OS integration. Both need a real
> machine and a real click.
>
> This split is not hypothetical. The first CI run failed on both: the Windows
> named-pipe path was missing its `io::Write` import, which no Linux build can
> see because the function is `#[cfg(windows)]`, and would have failed to compile
> for every Windows user.

---

## Comparison to Mod Organizer 2's NXMHandler

Retained from upstream:

* NXMHandler has its own UI to configure it, which is convenient alone but does
  not integrate with other managers. NXMProxy is configured from the command line
  or by editing the configuration file, and its state is readable as JSON.
* NXMHandler is built on Qt and pulls in >20MB of DLLs when distributed
  separately. NXMProxy has no dependencies beyond a handful of small Rust crates.
* NXMHandler uses ini files. NXMProxy uses TOML.

## Named pipes

Usually forwarding a download means launching the manager with the link as an
argument; the manager then works out whether it is already running and uses some
form of IPC to reach its primary instance. Mod managers running on a JVM,
JavaScript, .NET or Python runtime pay a noticeable startup cost for that.

A manager that exposes a named pipe can be handed the link directly instead:

```sh
nxmproxy pipe vortex vortex_download
```

nxmproxy writes the link and closes the pipe. **If the write fails it falls back
to spawning the command**, on the assumption the manager is not running yet. That
also means you get no indication if you mistyped the pipe name. Pipes are a
Windows mechanism; on other platforms the attempt is skipped.

---

## Licence and dependencies

GPL-3.0-or-later. See [`LICENSE.md`](LICENSE.md) for the full text (upstream's,
retained verbatim) and [`NOTICE.md`](NOTICE.md) for attribution.

Every direct and transitive dependency is permissive, with one exception:

| Crate | Version | Licence | Via |
| --- | --- | --- | --- |
| `clap` | 4.6 | MIT OR Apache-2.0 | direct |
| `directories` | 5.0 | MIT OR Apache-2.0 | direct |
| `serde` | 1.0 | MIT OR Apache-2.0 | direct |
| `serde_json` | 1.0 | MIT OR Apache-2.0 | direct |
| `toml` | 0.8 | MIT OR Apache-2.0 | direct |
| `url` | 2.2 | MIT OR Apache-2.0 | direct |
| `option-ext` | 0.2 | **MPL-2.0** | `directories` → `dirs-sys` |
| `tinyvec` | 1.2 | Zlib OR Apache-2.0 OR MIT | `url` → `idna` |
| `memchr` | 2.8 | Unlicense OR MIT | `serde_json` |
| `unicode-ident` | 1.0 | Unicode-3.0 | `proc-macro2` |

`option-ext` is the only weak-copyleft crate, and it is unmodified: MPL-2.0
applies at file level, so distributing this binary does not impose its terms on
this project. GPL-3.0 is one-way compatible with MPL-2.0.

Upstream's `windows`, `structopt` (clap 2), `once_cell` and hand-rolled bindings
crate are gone. `once_cell` was replaced by `std::sync::OnceLock` from the
standard library.