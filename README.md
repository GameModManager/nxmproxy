[![CI](https://github.com/GameModManager/nxmproxy/actions/workflows/ci.yml/badge.svg)](https://github.com/GameModManager/nxmproxy/actions/workflows/ci.yml)
[![Release](https://github.com/GameModManager/nxmproxy/actions/workflows/release.yml/badge.svg)](https://github.com/GameModManager/nxmproxy/actions/workflows/release.yml)
![OS](https://img.shields.io/badge/OS-Linux_%7C_macOS-34d058?labelColor=373f46)
![License](https://img.shields.io/badge/License-GPLv3-blue?labelColor=373f46)
[![Stars](https://img.shields.io/github/stars/GameModManager/nxmproxy)](https://github.com/GameModManager/nxmproxy/stargazers)

# nxmproxy

Forwards Nexus Mods `nxm://` download links to the mod manager you've assigned for that game.

## What it does

When you click Download on nexusmods.com, your browser hands an `nxm://` link
to the operating system. nxmproxy is the program that OS hands it to.

nxmproxy reads the game out of the link, starts the mod manager you assigned to
that game with the link, and exits. Once per link, in milliseconds. There is no
daemon, nothing running in the background, and nothing to keep alive: the `nxm://`
registration is the only thing that persists.

## Features

- Routes each link to the mod manager assigned to that game
- Works with any mod manager that declares it handles `nxm://`, so nothing is hardcoded
- On Linux, finds managers by reading the declaration the manager publishes itself, including third-party ones
- No daemon, no background process, nothing resident between clicks
- Links containing spaces and quotes arrive at the manager intact, as exactly one argument
- Hands the link to a manager that is already running, and falls back to starting it
- One plain-text config file, plus a log to read when a link arrives nowhere

## Use cases

- Two games, two different mod managers, chosen per game
- Everything routed to one manager, with no per-game setup at all
- Moving one game to another manager without reinstalling anything
- Using a mod manager nxmproxy has never heard of

## Install

Download the binary from the
[releases page](https://github.com/GameModManager/nxmproxy/releases). Both
assets hold the executable and nothing else, and each release records a sha256
digest of them.

### Linux

Static, so it runs on any distribution of any age, glibc or musl:

```sh
VERSION=0.4.0
curl -LO https://github.com/GameModManager/nxmproxy/releases/latest/download/nxmproxy-$VERSION-linux-x86_64
chmod +x nxmproxy-$VERSION-linux-x86_64
sudo install -m 755 nxmproxy-$VERSION-linux-x86_64 /usr/local/bin/nxmproxy
```

### macOS

One file that runs on Apple silicon and on Intel:

```sh
VERSION=0.4.0
curl -LO https://github.com/GameModManager/nxmproxy/releases/latest/download/nxmproxy-$VERSION-macos-universal.tar.gz
tar xzf nxmproxy-$VERSION-macos-universal.tar.gz
sudo install -m 755 nxmproxy /usr/local/bin/nxmproxy
```

The release is not code signed and not notarized, so if macOS refuses to run it,
that is Gatekeeper and not a broken download. Clear the quarantine flag once, or
right-click the file and choose **Open** once:

```sh
xattr -dr com.apple.quarantine /usr/local/bin/nxmproxy
```

### Windows

Not shipped. Build it yourself with
`cargo build --release --target x86_64-pc-windows-msvc`. It compiles and its
tests pass, but nothing has exercised it against a real Windows registry, so
treat `install` there as unproven.

Put the binary somewhere permanent before running `install` on any platform: it
records the absolute path you ran it from, so an install out of `~/Downloads`
keeps working only while that file stays put.

## Quick start

1. Install the binary (above).

2. Claim `nxm://` for nxmproxy:

   ```sh
   nxmproxy install
   ```

   This exits **3** on macOS and Windows on purpose. See
   [Troubleshooting](#troubleshooting) for the one step you do by hand.

3. Register your mod manager and say which game goes to it:

   ```sh
   nxmproxy discover                       # what this machine can prove it has
   nxmproxy assign hd2arsenal helldivers2

   nxmproxy register gamemodmanager /path/to/gamemodmanager --handle-nxm %1
   nxmproxy assign gamemodmanager skyrimse
   ```

   `%1`, `%u` and `%U` all mean "the link goes here". With exactly one manager
   registered, every game routes to it and `assign` can be skipped; with two or
   more, nxmproxy refuses to guess rather than put a download in the wrong queue.

4. Check it: `nxmproxy test` (exit 0 if `nxm://` links really open in nxmproxy),
   `nxmproxy list` (your routing table).

The only requirement is the mod manager itself: anything that accepts an `nxm://`
link on its command line can be registered, and nxmproxy does not depend on any
particular one.

## Commands

| Command | What it does |
| --- | --- |
| `nxmproxy <nxm://link>` | Forward one link. This is the form your OS invokes. |
| `nxmproxy url <nxm://link>` | The same, spelled as a subcommand. |
| `install` | Publish nxmproxy to the OS and ask it to hand over `nxm://`. |
| `uninstall` | Remove nxmproxy as the handler, symmetric with `install`. |
| `test` | Report whether nxmproxy is registered and who the OS says opens `nxm://`. |
| `list` | The routing table. `list --json` is a versioned machine-readable form for a GUI. |
| `discover` | Remember every mod manager this machine declares. Assigned to nothing until you assign it. Safe to re-run. |
| `register <name> <exec> [args...]` | Add a mod manager. |
| `unregister <name>` | Remove a mod manager and its game assignments. |
| `assign <manager> <domain>` | Route a game. `<domain>` is the nexusmods.com subdomain, or `_` for every game with no assignment. |
| `pipe <manager> <address>` | Hand links to an already-running manager instead of starting it. A Unix socket path, or a bare pipe name on Windows. |

Links are routed by three rules, in order: an exact assignment for the game, then
the `_` catch-all, then the single manager you registered by hand. A manager
`discover` found never wins the third rule on its own, because found is not
chosen.

### Delivering to an already-running manager

A manager that listens on a local socket can be handed the link directly instead
of being started, which saves the startup cost of a JVM or Electron app:

```sh
nxmproxy pipe vortex "$XDG_RUNTIME_DIR/gamemodmanager.sock"   # Linux, macOS
nxmproxy pipe vortex vortex_download                          # Windows
```

This is public contract, version 1, for anyone implementing a listener:

- **One write, then close.** The whole message is the URL's bytes exactly as they
  arrived: not re-encoded, not normalised, no length prefix, no trailing newline.
- **No acknowledgement**, and no reply is read. nxmproxy never waits for one.
- **Falls back to spawning** if the socket is not there, so a crashed manager
  degrades to a normal launch rather than a dead link.

Inert until you set it. With no pipe configured, nothing opens a socket.

## Configuration

One TOML file, in the standard location for your platform:

| Platform | Path |
| --- | --- |
| Linux | `$XDG_CONFIG_HOME/nxmproxy/config.toml`, or `~/.config/nxmproxy/config.toml` |
| macOS | `~/Library/Application Support/nxmproxy/config.toml` |
| Windows | `%APPDATA%\nxmproxy\config.toml` |

```toml
discovered = ["hd2arsenal"]   # found by `discover`, not chosen by you

[games]
skyrimse = "gamemodmanager"
_ = "vortex"                  # catch-all for every game with no assignment

[managers.vortex]
exec = "/opt/vortex/Vortex"   # a path, or a bare name resolved through PATH
args = ["%1"]                 # the link goes here
pipe = "vortex_download"      # optional; a bare pipe name on Windows
```

The link is substituted **inside one argument**, so a URL containing a space or a
quote still arrives correctly and a path containing a space is not a quoting
problem. Writes are atomic, and nxmproxy refuses to write at all while the file
is unparseable, so a corrupt file is never overwritten by a repair.

`nxm.log` is written beside it. nxmproxy is launched by the OS with no terminal
and exits in milliseconds, so when a link does not arrive the reason has to
already be in that file. `NXMPROXY_CONFIG_DIR` overrides the location.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Done, and verified. |
| 1 | Not delivered, or the command failed. Never a panic. |
| 2 | Bad command line. |
| 3 | Registered, but one step is left that you must do by hand. Nothing failed; nxmproxy did everything the platform allows. The message says which step. |

Code 3 is what makes `install` scriptable, and it is deliberately neither 0 nor
1: a normal macOS or Windows install must not abort a wrapper script, and 0 would
automate an install that routes nothing. It also covers "no mod manager to forward
to" and "a browser decides `nxm://` for itself".

## Troubleshooting

**`install` exits 3 on macOS and Windows.** By design, and it always will. The
claim is one-way on both platforms and cannot be read back, so nxmproxy prints
the step you have to do:

- macOS: Finder, Get Info on `NXM Proxy.app`, **Open with** > NXM Proxy >
  **Change All**. macOS may also ask you to confirm by itself.
- Windows: `install` opens the Default Apps page for you; choose NXM Proxy there.

**A downloaded binary will not run on macOS.** Unsigned and un-notarized, so
Gatekeeper assesses it as a refusal. `xattr -dr com.apple.quarantine nxmproxy`,
or right-click > **Open** once.

**Nothing happens when you click Download.** Run `nxmproxy test`. It prints who
the OS says opens `nxm://`, in precedence order, with the file each answer came
from - so a second mod manager that outranked nxmproxy is named rather than left
for you to guess. `test` also reports browsers that decide `nxm://` themselves
and never ask the OS; Firefox does this per profile, and `test` names the file to
open. nxmproxy will not edit another application's configuration for you.

**Nothing was found, or `install` printed a `register` command.** Auto-detection
reads desktop entries, which is a Linux mechanism. macOS and Windows have no
equivalent file, so there nxmproxy prints the command to type rather than
guessing at a path. One command is the whole setup there.

**A link arrives but the manager does nothing.** Check the manager's arguments
hold `%1`, `%u` or `%U`, and that the executable is where you said it was.
`nxmproxy list` marks both: `(no link code)` and `(unavailable)`.

## About this project

A fork of [NXMProxy](https://github.com/TanninOne/nxmproxy) by Tanner Parnell,
the author of Mod Organizer 2, first released in 2021 and GPL-3.0-or-later. The
routing model is his. This fork added Linux and macOS support, a release pipeline
and tests, and fixed the crashes and link-corruption that upstream had.
[NOTICE.md](NOTICE.md) records what was kept, what was fixed and what was
removed.

## Contributing

Issues and pull requests are welcome. Run `cargo test` before you open one; the
whole suite takes seconds.

## License

GPL-3.0-or-later. [LICENSE.md](LICENSE.md) is upstream's licence text, retained
verbatim; [NOTICE.md](NOTICE.md) carries the attribution.