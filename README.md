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
* **`register` / `unregister`**: a custom manager as `{exec, argv vector}`.
  Upstream's `register` took a command *string*, and its own README example
  (`README.md:11`, `c:\program files\...\vortex.exe`) was **unquoted with spaces
  in it**, so it could not have worked. The names are upstream's; only the
  argument handling is new.
* **A curated list of known managers**, reported by `list --json`, each with a
  `present` flag saying whether it is actually on this machine.
* **Empty configuration works out of the box**: with exactly one manager
  registered, every domain routes to it with nothing assigned.
* **`uninstall`**, which upstream did not have.
* **`install` claims the scheme**, not just registers. See below.
* **Auto-registration on a fresh install**, when a manager can be proven present.
  See "Finding the mod manager".
* **Tests and CI.** Upstream had no tests at all.

### Changed: `install` now claims the scheme, and says whether it worked

| Was | Now | Why |
| --- | --- | --- |
| "register, never seize": `install` published a desktop entry / bundle / ProgID and wrote **no** default association on any platform, so opening an `nxm://` link gave the user a chooser | `install` publishes **and** claims, as hard as each platform allows, and exits 3 when it could not finish | nxmproxy's whole job is to take `nxm://` and route it. A user who installed it and still got a chooser had a **broken install**, and the old behaviour could not tell them that. |

What is still forbidden everywhere, and is enforced by test: **touching another
mod manager's entry.** Claiming is for nxmproxy. Nothing here edits another
manager's `[Added Associations]`, its entry for any other scheme, or its
registration on any platform.

The other half of that change is honesty about the exit code. `install` exits 0
only when nxmproxy really is the handler *and that was verified from this
process*. On macOS and on Windows it cannot be, so it exits **3** and prints the
exact click. Reporting success there is how a broken install gets automated.

### The rule all three platforms obey: ask, then say whether it worked

| Platform | Can it claim? | How | What `install` reports |
| --- | --- | --- | --- |
| Linux | **Yes** | `xdg-mime default nxmproxy.desktop x-scheme-handler/nxm`, then read the file back | exit 0 once the file confirms it |
| macOS | **Yes, but the OS asks** | `LSSetDefaultHandlerForURLScheme`; macOS 12+ prompts | exit 3, always. The call is one-way and cannot be read back |
| Windows | **No** | It opens `ms-settings:defaultapps` and stops | exit 3, always |

**Why Windows is not worked around.** The effective default lives under
`HKCU\...\UserChoice`, whose `Hash` is a keyed digest Windows recomputes on
write and validates on read. No application may write it; `reg add` against it
fails. This is documented platform policy ("apps cannot change default apps
without user interaction in system UI"), not an obstacle to route around, and
nothing here tries. What Windows *does* permit is a correct ProgID plus a
`RegisteredApplications` entry, which is what makes the application appear in
Default Apps - and then the user finishes in one click.

**Why GMM's Linux pattern is not copied.** GameMod Manager's own registration
calls `xdg-mime default` *and* `set_mimeapps_defaults`, which forces the same
default into three files (`kde-mimeapps.list`, `mimeapps.list`, and the
data-level `mimeapps.list`). That is how a user with two mod managers ends up
with one of them buried, un-reversible without hand editing. nxmproxy writes one
line, in one file, and takes it back out.

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

`nxmproxy install` is three steps in one command: publish nxmproxy to the OS,
take the `nxm://` scheme, and make sure a link has somewhere to go. Each step
fails independently and the exit code is the worst of them.

### Linux

`install` writes exactly two files:

```
~/.local/share/applications/nxmproxy.desktop
    MimeType=x-scheme-handler/nxm;

~/.config/mimeapps.list
    [Default Applications]
    x-scheme-handler/nxm=nxmproxy.desktop
```

The first makes nxmproxy *eligible*. The second makes it what the desktop
opens. `xdg-mime default` writes that line and only that line: no
`kde-mimeapps.list`, no data-level `mimeapps.list`, nothing forced. That is
asserted on content, for all three files, by a test that pre-seeds them with
another mod manager's entries and checks every other line survives.

The exit code is not trusted. `xdg-mime default` exits 0 and **writes nothing**
when `$XDG_CONFIG_HOME` does not exist, which it does not on a fresh account, so
`install` creates the directory first and then reads the file back. If the file
does not say nxmproxy, `install` says so and exits 3.

When `xdg-mime` is not installed at all, nxmproxy writes the same one line
itself, says that it did so, and still reports the truth. That hand-written path
has its own end-to-end test, with `xdg-mime` removed from `PATH`, because on a
normal machine it never runs.

`uninstall` removes nxmproxy's id from that one key and leaves every other id,
every other key, and every other group alone. If our id was the only one, the
key is removed rather than left empty, because `key=;` is a claim on nothing,
not the absence of a claim.

> What is restored, and what is not: `xdg-mime default` **replaces** the value
> for a key rather than appending to it, so uninstall cannot put a previous
> default back - nothing recorded what was there. What survives is the other
> manager's own `[Added Associations]` entry, which is how the desktop learns it
> can still offer them. `uninstall` says this rather than implying a full
> restore.

### macOS - UNVERIFIED, and it says so

`install` publishes a minimal `.app` bundle whose `Info.plist` declares `nxm`,
then calls **`LSSetDefaultHandlerForURLScheme`**, which on macOS 12 and later
**prompts the user**. The OS asking is the consent step; there is no other
supported route to a working install.

An earlier version of this fork deliberately refused that call and only ran
`lsregister`, on the principle that assigning the scheme is the greed this tool
exists to stop. That was right about the method and wrong about the outcome: it
left the bundle eligible and unchosen, so `install` reported success over an
install that routed nothing.

> **Honest status - read before trusting the macOS path.** The FFI was written
> against Apple's documentation, **not on a Mac**. What CI proves is that it
> compiles against the SDK and links against CoreFoundation and CoreServices.
> What nothing proves is what it does: whether the prompt appears, whether it
> takes the claim when the user agrees, and what `test` reports on a machine
> where something else already owns `nxm://`. Because the call cannot be read
> back, `install` **never** reports a verified claim on macOS and always exits 3.
>
> **CI found the API is not merely unverified - it is hostile.** On the headless
> `macos-latest` runner, `LSSetDefaultHandlerForURLScheme` **aborts the process**
> (killed by a signal, not an error return). LaunchServices needs a GUI session
> that a CI runner does not have, and the first CI run proved it: two integration
> tests that run the real binary died, and a unit test that called the API
> directly took the whole test binary down with SIGTRAP.
>
> Two consequences, both permanent and neither hidden:
>
> * The unit tests target `decide_claim(status)`, a pure function of the
>   `OSStatus`, so the suite makes **no** system call. The FFI is a three-line
>   shell around it. This is the same split `windows.rs` already uses to keep
>   `reg.exe` out of the tests.
> * The two integration tests that run the real `install` accept a process
>   killed by a signal as "needs user", never as success, and print a note when
>   it happens. So **the macOS install path is not covered end to end**, and this
>   README does not claim it is. The assertion those tests still carry on macOS
>   is the one that matters most: nxmproxy never reports a claim it cannot
>   verify.

`nxmproxy test` cannot report the current default handler without
`LSCopyDefaultHandlerForURLScheme`, which this fork does not bind, so `test`
says so and points at System Settings rather than inventing a boolean.

`uninstall` removes the bundle and then states plainly that macOS has no
supported way to *unset* a scheme handler; pointing it elsewhere is the user's
click.

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
| `HKCU\...\CurrentVersion\RegisteredApplications` | Puts us in Default Apps so the shell can offer a choice |

`SupportedTypes` plus `RegisteredApplications` plus `Capabilities` is the
documented shape that makes Windows **offer** the application. That part works,
and it is the whole of what Windows permits.

`install` then opens `ms-settings:defaultapps?registeredAppUser=GameModManager%20NXM%20Proxy`
- Microsoft's documented URI for the Default Apps page, with the `registeredAppUser`
parameter added in Windows 11 21H2 that jumps straight to one app's entry. User,
not machine, because the app is registered under `HKCU` - a machine-scoped
lookup would not find it. The exact string is asserted by a test on every
platform, because no Linux build can otherwise see it.

**Nothing under `HKEY_CLASSES_ROOT` is ever written**, and nothing writes
`UserChoice`, `FileExts` or `App Paths`; a test scans production source for
those three names. `reg.exe` is used rather than the Win32 API, which removes
the `windows` crate and its build script and leaves the risky part (exact key
names and quoting) as plain argument vectors unit tested on every platform.

`uninstall` deletes the ProgID and the `RegisteredApplications` value, then
states that Windows will not let nxmproxy take the default back: `UserChoice`
keeps pointing at a ProgID that no longer exists and Windows falls back to its
own prompt. It does **not** claim to have undone it.

> **Honest status: the Windows path has never been executed against a Windows
> registry by this fork's author.** It compiles on `x86_64-pc-windows-msvc` and
> its tests pass on `windows-latest`, and key names, argument construction and
> per-user scope are asserted on every platform. What is **not** verified: that
> `reg.exe` accepts every key as intended, that the settings page opens, and
> that Windows then offers nxmproxy in the `nxm://` chooser. Those need a real
> machine.

---

## Finding the mod manager

`install` owns `nxm://`. If nothing forwards it, every link goes nowhere, and
that is a broken install - so `install` does not stop at claiming the scheme.

**The problem is that GMM's binary location is not fixed and there is no stable
install path on any of the three platforms.** GameMod Manager ships through
QtIFW to an install prefix that is **not** on `PATH`, is not registered in a
well-known location on macOS, and on Windows sits under `Program Files`. So
guessing a path produces a manager that is registered, cannot be started, and
silently eats every download.

What was considered:

| Option | Verdict |
| --- | --- |
| Ship GMM pre-registered in the default config | **Rejected.** It hardcodes a path that is wrong on most machines, so the default is broken more often than it works - and a broken default is worse than no default, because it fails at click time rather than at install time |
| Require the user to run `register` first | **Rejected as the whole answer.** It is the correct *fallback* and remains one, but an out-of-the-box install that routes nothing is a broken install |
| Ask the OS who currently handles `nxm://` | **Rejected.** On Windows that returns a command line, and parsing it means reintroducing the `CommandLineToArgvW` quoting this fork deleted upstream's use of. On macOS it is not readable at all |
| **Auto-detect from the manager's own desktop entry** | **Chosen** |

The chosen approach reads the XDG desktop entry the manager writes for itself.
GameMod Manager writes
`~/.local/share/applications/gamemodmanager-nxm.desktop` when the user enables
its nxm handling, and that file contains its real `Exec=` line. So the path is
not guessed: it is read from the only place it is recorded. This is the signal
that works when `PATH` does not, which is the case that actually occurs.

The rules are deliberately narrow, because every wider rule is worse:

* **Only when the configuration has no managers at all.** A user who configured
  something has already answered this question, and `install` never extends a
  configuration it did not create.
* **Only when exactly one candidate is provably present.** Two managers is
  ambiguous, and a wrong guess is a download in someone else's mod queue. That
  is `Route::Ambiguous`'s whole argument, and `install` must not invent the very
  ambiguity it refuses to resolve at click time.
* **Only when the executable really resolves.** Registering a manager that cannot
  be started is the broken install this exists to prevent.
* **Only when the parsed argv contains `%1`.** A manager that never receives the
  link would be started for every download and handed nothing.
* Otherwise: **refuse, and print the exact command to type.** A refusal with
  instructions beats a guess, and it exits 3 so a script knows.

With exactly one registered manager, routing rule 3 sends every unassigned
domain to it, so a fresh install routes with no further configuration.

> **Where this does not work, stated plainly.** The desktop-entry signal is a
> Linux mechanism. On macOS and Windows there is no equivalent file, so unless
> the manager is on `PATH`, `install` finds nothing, prints the `register`
> command, and exits 3. That is a real asymmetry, not a claim of parity: on those
> two platforms an out-of-the-box install still needs one typed command. The
> alternative - guessing at a `Program Files` path or scanning an `.app` bundle -
> would be a guess, and this tool does not guess.

`nxmproxy list --json` reports the same judgement per entry as `present`, so a
GUI can offer the same one-click register.

## Command line

| Command | Meaning |
| --- | --- |
| `install` | Install nxmproxy as the `nxm://` handler: full configuration **and** claim. |
| `uninstall` | Remove nxmproxy as the handler (symmetric with `install`). |
| `register <name> <exec> [-- <args>...]` | Add a mod manager. |
| `unregister <name>` | Remove a mod manager, and its game assignments. |
| `assign <manager> <domain>` | Routing. `<domain>` is the `nexusmods.com` subdomain, or `_` for every game with no assignment. |
| `list [--json]` | The routing table. `--json` is the machine-readable contract. |
| `test` | Report whether nxmproxy is registered and who the OS says handles `nxm://`. Exit 0 if registered, 1 if not. |
| `pipe <manager> <pipe>` | Give a manager a named pipe to try before spawning it. |
| `url <nxm url>` | Forward a link. This is what the OS invokes. |

`%1` inside any element of `args` is where the link goes. Use `--` before the
argument vector so a link or flag-looking argument is never parsed as an option.

### Names: upstream, and this fork

Upstream (`TanninOne/nxmproxy`, `src/main.rs`, HEAD `f9f72e0`) had
`Url`, `Register`, `Deregister`, `Assign`, `Pipe`, `Install` and `Test`. The
mapping, verified against that source:

| Upstream | Here | Change |
| --- | --- | --- |
| `Url` | `url` | - |
| `Register { manager, command }` | `register <name> <exec> [-- <args>...]` | **Same name, different arguments.** Upstream's took a command *string*; this takes an exec plus an argv vector |
| `Deregister { manager }` | `unregister <name>` | Renamed. It removes rather than deregisters |
| `Assign` | `assign` | - |
| `Pipe` | `pipe` | - |
| `Install` | `install` | **Same name, much more.** Upstream wrote `HKCR\nxm` and elevated itself; this publishes a per-user ProgID, then claims the scheme |
| `Test` | `test` | - |
| *(none)* | `uninstall` | Added. Upstream could not uninstall: its `deregister` removed the manager from its config and left the registry key behind |
| *(none)* | `list` | Added |

So there was no collision to resolve: upstream already used `register` for a mod
manager, and `Install` for the scheme. `register-manager` was this fork's own
interim name, and it is **removed outright** rather than kept as an alias -
a command that keeps working after being renamed is a second surface nobody
documents. `deregister` and `unregister-manager` are gone for the same reason.
A test asserts each of them now exits 2.

### Exit codes

| Code | Name | Meaning |
| --- | --- | --- |
| 0 | ok | Delivered, or the command succeeded and the result was verified. |
| 1 | failed | Not delivered, or the command failed. **Never a panic.** |
| 2 | usage | Bad command line (from `clap`). |
| 3 | needs-user | **Registered, but the OS requires one step you must do by hand.** Nothing failed; nxmproxy did everything the platform allows |

Code 3 is what makes `install` scriptable. It is deliberately neither 1 nor 0:

* not **1**, because nothing went wrong - a wrapper that aborts on 1 would treat
  a normal macOS or Windows install as a failure;
* not **0**, because on macOS and Windows nxmproxy cannot verify the claim, and
  a wrapper that reads 0 would automate an install that routes nothing.

The same code also covers "no mod manager to forward to", because typing
`nxmproxy register ...` is exactly such a step. The message always says which.

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
> there, and every test that does not need a real OS passes. The macOS plist, the
> macOS `LSSetDefaultHandlerForURLScheme` decision, the Windows registry
> operation list and the Windows Default Apps URI are all pure functions over
> text and decisions, so they compile and pass on all three platforms - which is
> why the rule that **macOS and Windows never report a verified claim** is
> enforced from a Linux build rather than only on the platform it concerns.
>
> **What it does not prove:** that `LSSetDefaultHandlerForURLScheme` makes macOS
> offer the bundle, that the prompt appears, or that `reg.exe` writes the Windows
> keys as intended. The `tests/cli.rs` round trip runs on all three platforms but
> only performs the write where the platform allows it, so on macOS and Windows
> it verifies the CLI contract and the exit codes rather than the OS
> integration. Those need a real machine and a real click.
>
> This split is not hypothetical. The **first CI run of the claiming work failed
> on all three platforms**, and every failure was one a Linux build could not
> see:
>
> * macOS: the unit tests made a real LaunchServices call and the binary died
>   with SIGTRAP before printing a test name.
> * macOS: two integration tests died because the real binary aborts on a
>   headless runner.
> * Windows: tests that run `install` share one real `HKCU` and the sandbox
>   cannot isolate it, so cargo's parallel threads overwrote each other and `test`
>   reported registered before anything had been installed.
> * Linux: the argv-recording test waited for a file to *exist* rather than to be
>   finished, so on a loaded runner it read a half-written argv and reported a
>   link that had lost its URL. It had been green locally for months.
>
> An earlier run failed the same way: the Windows named-pipe path was missing its
> `io::Write` import, which no Linux build can see because the function is
> `#[cfg(windows)]`, and would have failed to compile for every Windows user.

### Negative controls

Every load-bearing test has a mutation control that proves it can actually go
red. The controls are mutation-and-observe, not assertion-counting:

| # | Mutation | Test that catches it |
| --- | --- | --- |
| 1 | `install` force-propagates the claim into all three mimeapps files (GMM's anti-pattern) | `linux_install_claims_the_scheme_in_one_file_and_buries_nobody` |
| 2 | the claim deletes another mod manager's lines | `the_fallback_claim_path_buries_nobody_either` |
| 3 | `uninstall` deletes another manager's line | `linux_uninstall_removes_only_our_line` |
| 4 | the file editor drops other lines | `set_default_ids_leaves_every_other_line_byte_for_byte` |
| 5 | a claim that needs the user exits 0 | `only_a_verified_claim_exits_zero` |
| 6 | `Claim::needs_user` reports a verified claim | `windows_never_reports_a_claim_it_could_not_make`, `a_claim_is_never_reported_as_verified` |
| 7 | the "no manager, so nothing is routed" guard is removed | `install_never_calls_a_routing_setup_done` |
| 8 | `register-manager` survives as an alias | `the_command_names_are_the_final_surface_and_the_old_ones_are_gone` |
| 9 | the `$XDG_CONFIG_HOME` fix is removed, restoring `xdg-mime`'s silent no-op | `install_and_uninstall_round_trip` |
| 10 | `present` is satisfied by a desktop entry that merely exists | `a_manager_is_detected_only_when_it_can_be_prove_present` |
| 11 | detection accepts an `Exec` line with no `%1` | `install_refuses_a_manager_it_could_not_use` |
| 12 | detection fires on a configuration the user built | `install_refuses_a_manager_it_could_not_use` |
| 13 | detection registers a manager whose executable does not resolve | `install_registers_a_manager_it_can_prove_is_present` |

One control is deliberately reported as **green**, and that is the point of
running them at all. Breaking `Claim::exit_code` to always return 0 did **not**
turn `install` green, because the "no manager, so nothing is routed" guard sets
exit 3 independently. That is defence in depth working, and it is also a
reminder that a control only proves what the test it targets actually asserts -
control 6 above exists for the exit code itself.

The rest found real bugs while they were being written:

* Control 7 reproduced a live defect: `xdg-mime default` exits **0 and writes
  nothing** when `$XDG_CONFIG_HOME` is missing, which is the state of a fresh
  account. `install` claimed a scheme it had not claimed. Fixed by creating the
  directory first, and the read-back now catches it if that regresses.
* Control 2 came back **green** the first time, which proved the test was not
  running where the logic lives: with `xdg-mime` installed, `install` delegates
  the edit entirely and nxmproxy's own editor never executes. That left the
  hand-written fallback path - the one used on minimal systems, and the one this
  project wrote itself - with no end-to-end coverage at all.
  `the_fallback_claim_path_buries_nobody_either` was added to close it, and
  control 2 goes red against it now.
* Control 10 caught `list --json` and `install` **disagreeing** about what
  "present" means: the JSON was satisfied by a desktop entry merely existing,
  while `install` also required the executable to resolve. Both now share one
  helper and one bar.

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