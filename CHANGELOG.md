# Changelog

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