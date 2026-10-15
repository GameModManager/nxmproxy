# Changelog

## 0.3.0

### Added

* **Any mod manager, found by its own declaration.** A manager is detected
  because its desktop entry carries `MimeType=x-scheme-handler/nxm`, the XDG
  declaration of "I take nxm links", published by the application itself.
  Until now only the one entry hardcoded into `src/defaults.rs` could ever be
  found, so a third-party manager such as HD2Arsenal was undetectable from any
  input. Detection scans `XDG_DATA_HOME/applications`, every
  `XDG_DATA_DIRS` entry, `~/.local/share/applications` and
  `XDG_CONFIG_HOME/applications`.
* **`discover`.** Finds every manager this machine declares and remembers each
  one as assigned to no game. Found is not chosen: a remembered manager routes
  nothing until `assign`, is never offered as one of the choices when the answer
  is ambiguous, and `resolve` refuses rather than guessing. Safe to re-run - an
  id that is already known is left exactly as it is, and nothing is ever removed.
* `list` reports which managers were found and which were chosen. `list --json`
  is version 3 and carries `managers[].discovered`.

### Fixed

* `Exec=` is read from the `[Desktop Entry]` group and nowhere else. An action
  group carries its own `Exec=` - HD2Arsenal's `[Desktop Action Uninstall]`
  runs `AppManager uninstall` - and delivering an nxm link to that asks a
  package manager to delete the user's mod manager.
* `Exec=` is parsed quote-aware, so an executable path containing a space is one
  argument rather than two.
* `MimeType` is read from any group, because AppImage packaging puts it in an
  action group rather than in `[Desktop Entry]`, and a strict group-only reader
  did not see HD2Arsenal at all. The asymmetry is safe: the argv still comes
  from `[Desktop Entry]`, so a stray `MimeType` can never make an uninstall
  action something nxmproxy spawns.
* A duplicated `x-scheme-handler/nxm` is one handler, not two candidates.
* nxmproxy never detects itself, which `install` would otherwise cause by
  publishing its own entry before detection runs.
* `~/.local/bin` is searched when resolving an executable and reported by
  `list`, but is never a detection source: what is in it says a program runs,
  not that it takes a link.
* A manager whose application is uninstalled stays in the list, reported
  unavailable, instead of silently disappearing from it.

### Changed

* Output trimmed to state. `list` prints the config path, the managers, and the
  games under each manager. The local-bin inventory line, the per-manager
  provenance notes, the closing lesson, the advice under the candidate list and
  the trailing hints on `install`, `register` and `test` are gone: each was
  telling a user something the command had already done.
* `install` points at `discover` rather than running it. It never extends a
  configuration it did not create, and a second hidden write path inside
  `install` is how that rule gets lost.

## 0.2.0

First tagged release of this fork of
[NXMProxy](https://github.com/TanninOne/nxmproxy) by Tanner Parnell,
Copyright (c) 2021 Tanner Parnell, GPL-3.0-or-later. Upstream's four commits are
preserved in this repository's history. See `NOTICE.md` for what was kept, what
was fixed and what was removed, and for the complete corresponding source. The
licence text is `LICENSE.md`.

### Command surface

    nxmproxy <nxm://url>
    nxmproxy install
    nxmproxy uninstall
    nxmproxy test
    nxmproxy list [--json]
    nxmproxy register <name> <exec> [args...]
    nxmproxy unregister <name>
    nxmproxy assign <manager> <game>
    nxmproxy pipe <manager> <pipe>

New here: `uninstall`, and `list` with its `--json` output. `install` now claims
the `nxm://` scheme rather than only registering, and exits 0 only when nxmproxy
really is the handler and that was verified from the same process.

Exit codes: 0 done and verified, 1 failed, 2 bad command line, 3 the OS leaves one
step to the user. macOS and Windows always exit 3.

### Fixed

* A URL containing a space or a quote reached the manager as two arguments, the
  last of them wrong. The command is an argv vector now and the field code is
  substituted inside a single element. `%1`, `%u` and `%U` all accepted.
* A link for a game nobody configured panicked. It is an ordinary message and a
  non-zero exit now.
* A corrupt config panicked the handler for every link on the machine. One bad
  entry degrades to a warning, and writes are refused while the file is
  unparseable, so the file you need to repair is never overwritten.
* `install` wrote `HKEY_CLASSES_ROOT` and re-launched itself elevated to get
  there. It is a per-user ProgID under `HKEY_CURRENT_USER` now. No elevation.
* The Windows named pipe path was opened on every platform, where it is a
  relative file name and dropped a stray file into whatever directory launched
  it.
* `test_installed` compared the handler against the literal `nxmproxy.exe`, so it
  only ever matched an install with that exact name.

### Platform support

* Linux x86_64: one static musl binary, no shared library, runs on any Linux
  distribution.
* macOS and Windows: compile only.

Linux and macOS support are both new here; upstream was Windows-only.

README.md states exactly what is and is not executed on each platform.