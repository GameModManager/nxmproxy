# Changelog

## Unreleased

**macOS is now shipped.** A release carries a
`nxmproxy-<version>-macos-universal.tar.gz` next to the Linux binary: one Mach-O
with **both** `arm64` and `x86_64` in it, built from the same commit, same
lockfile and same profile, joined with `lipo`. Intel users are not left to
rebuild from source, and a user does not have to know which Mac they have
before choosing what to download.

The tarball holds the bare binary and nothing else - no `.dmg`, no installer, no
`Info.plist` - because nxmproxy is a command-line tool and `install` builds the
`NXMProxy.app` bundle itself. That also means **the archive is not code signed
and not notarized**, which has a cost worth naming: Gatekeeper assesses an
unsigned binary as a refusal (`spctl --assess --type execute` returns
`rejected`, exit 3), and a copy that came from a browser arrives with
`com.apple.quarantine` set. After unpacking, either

```sh
xattr -dr com.apple.quarantine nxmproxy
```

or right-click the file and choose **Open** once. Notarization would need a paid
Apple Developer ID; a repository that ships a GPL command-line tool does not
have one.

### Changed

* **CI now runs the macOS binary instead of only building it.** `--version`,
  `--help` and `list` are executed on every push, which is the cheapest check
  that is honest: a binary that links is not a binary that starts. `install` is
  still never run in CI and never will be - `LSSetDefaultHandlerForURLScheme`
  aborts the process on a runner with no GUI session - and the workflow says so
  in a comment aimed at the next person.
* The CI trade, restated honestly: running the **test suite** on three
  platforms per commit is still given up, because it costs minutes and the
  author sees failures in seconds. What is no longer given up is executing the
  binary at all, now that macOS is a platform users download rather than one
  that merely has to compile.

### Verified

Checked on a real GUI Mac (macOS 15.7, arm64), not inferred:

* `cargo build --release` and `cargo test` are clean: **136/136** (119 unit, 17
  integration).
* The universal binary is `arm64` + `x86_64` (`lipo -info`), and **both slices
  execute**: `arch -x86_64` and `arch -arm64` on the same fat binary both print
  the version. The arm64 slice is ad-hoc signed by the linker, the x86_64 slice
  carries no signature at all, and both run.
* It links only what macOS ships: CoreFoundation, CoreServices, `libiconv`,
  `libSystem` - nothing from Homebrew, nothing relative.
* `install` end to end on a real session: it creates
  `~/Library/Application Support/nxmproxy/NXMProxy.app`, registers it through
  `lsregister -f`, and a link opened through the bundle arrives at the manager
  **byte for byte**, spaces and `&` query strings intact.

### Still unverified, and still not claimed

* **The default handler cannot be read back.** `LSCopyDefaultHandlerForURLScheme`
  is not bound, so `test` reports unknown rather than guessing, and `install`
  **still exits 3** on macOS. That has not changed and is not changing here.
* Whether the `LSSetDefaultHandlerForURLScheme` prompt appears on macOS 12+ and
  takes the claim when the user agrees is still not something this repository
  can observe. It was **not** observed on the real Mac either: that run
  exercised the bundle and the registration, and the claim was finished by hand
  through Finder, which is the documented route and the one `install` prints.
* The Windows named-pipe fast path has still never been executed anywhere.

## 0.3.0

Second tagged release of this fork of
[NXMProxy](https://github.com/TanninOne/nxmproxy) by Tanner Parnell,
Copyright (c) 2021 Tanner Parnell, GPL-3.0-or-later. The licence text is
`LICENSE.md`; `NOTICE.md` states what was kept, what was fixed and what was
removed.

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
  is version 5 and carries `managers[].discovered`, `scheme.declarations`,
  `handler` and `owns_scheme`.
* **`test` reports a browser that decides `nxm://` for itself.** A Gecko profile
  keeps its own per-scheme table and consults it *before* the OS, so once `nxm`
  has an entry there no change to any `mimeapps.list` ever reaches that profile
  again. Read-only: it names the file and the setting inside it, and the edit is
  the user's. Chromium has no such list - it only records that a *site* may
  launch a *scheme* and names no application - so it is not reported.
* **The `pipe` fast path works on Linux and macOS.** It was documented and did
  nothing there: the only implementation was upstream's
  `File::create(r"\\.\pipe\{name}")`, which is Windows-only and returned false
  everywhere else, so a pipe configured on Linux silently took the spawn path
  for every link. It is now a **Unix domain socket**, and the whole contract -
  one write of the URL, then close, no framing, no acknowledgement, the
  configured address is the whole discovery - is published in README as a
  versioned thing a third-party manager can implement a listener against.
* **The fallback is now provable.** Every failure to deliver falls back to
  spawning, and on Linux a no-listener dispatch measures **1.4 ms** end to end:
  the socket `connect` to an absent peer fails immediately rather than waiting,
  which is why this is a socket and not a FIFO - a FIFO's `open(O_WRONLY)`
  blocks in the kernel until a reader appears, and the OS runs nxmproxy once per
  clicked link. A crashed manager leaves a stale socket file that reports
  `ECONNREFUSED`, so liveness needs no heartbeat.

### Fixed

* **`nxmproxy <nxm://url>` works.** The command surface has documented the bare
  form since 0.2.0 and it did not run: upstream made the link a *subcommand*
  named `url`, so `nxmproxy nxm://helldivers2/...` was parsed as a subcommand
  called `nxm://helldivers2/...` and refused with `error: unrecognized
  subcommand`. Both spellings work now - the bare form, and `url <url>`, which
  is what the desktop entry nxmproxy publishes invokes and what the surface has
  always named. Nothing else about the command moved.
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
* The output no longer prints one line per claimed file. The count says how many
  rows were written; only the refusals get a line each.
* **The Vortex entry no longer states a pipe as fact.** It read "Also exposes a
  named pipe: vortex_download", which is upstream's README *example*, not
  anything verified about Vortex - no evidence was found that Vortex binds one.
  It now says which of the two it is.

### Changed

* **`install` claims `nxm://` in every file it owns, not the one `xdg-mime`
  writes.** `xdg-mime default` writes `$XDG_CONFIG_HOME/mimeapps.list` and
  nothing else - and `$XDG_CONFIG_HOME/$desktop-mimeapps.list` sits **above**
  that file, so GameMod Manager holding the upper row meant the desktop opened
  GMM on every install, however many times nxmproxy was reinstalled. `install`
  now writes ten files: both rows under `$XDG_CONFIG_HOME` and both under
  `$XDG_DATA_HOME/applications`, for every `$desktop` it can find - the running
  session, every `DesktopNames=` in an installed `xsessions/` or
  `wayland-sessions/` entry, the stem of every `$desktop-mimeapps.list` already
  present, and the four the specification's text names. Each write is read back.
* **The claim prepends; it never replaces.** nxmproxy's id goes to the front of
  the `[Default Applications]` value and every other id keeps its rank among the
  others. GMM's own `set_mimeapps_scheme` replaces, which deletes the user's
  previous choice with nothing recording what it was; that is not repeated here.
* **`test` resolves the whole chain, the way a desktop does.** Both groups, every
  file in the specification's order, and the specification's second step: an id
  whose `.desktop` file is missing or does not declare the type is skipped and
  the search carries on. `registered: true` can no longer print while another
  application holds the scheme. Verified against `xdg-mime query default` on
  identical fixtures.
* **`uninstall` reverses every write, exactly.** Prepending is invertible, so a
  file's previous bytes come back byte for byte - including `kde-mimeapps.list`.
  A file that held nothing but nxmproxy's own line is removed rather than left
  with an empty `[Default Applications]` header.
* **A location that refused the write is named**, with its path and the reason,
  and the header counts the loss. `xdg-mime` exits 0 and writes nothing when the
  target directory is missing, so exit status is not evidence of anything.
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