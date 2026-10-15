# NOTICE

## Upstream

This project is a fork of:

**NXMProxy** - https://github.com/TanninOne/nxmproxy
Copyright (c) 2021 Tanner Parnell
Upstream HEAD at time of forking: `f9f72e0`, tag `0.1.0`
Licence: GNU General Public License v3 or later (see `LICENSE.md`, retained
verbatim from upstream)

Upstream's four commits are preserved in this repository's history. Everything
after them is this fork.

### What was kept

Upstream's design and its own documentation, including:

* the routing model: a mod manager is an executable plus an argument vector, and
  the routing table maps a `nexusmods.com` subdomain to a manager;
* the `_` catch-all domain key;
* the pipe-first-then-spawn dispatch order;
* the temp-file-plus-rename config write;
* the comparison with Mod Organizer 2's `nxmhandler`, reproduced in `README.md`;
* the note about `CommandLineToArgvW` and why the token is substituted into a
  single argv element rather than into a command string.

This fork additionally accepts `%u` and `%U` beside `%1`, because the token is
read out of an `Exec` line where those are the spellings that occur, and
`--` before the argument vector of `register` is optional for the same reason:
a user copies the vector out of a desktop entry and does not type a separator.

### What changed

See "Divergences from upstream" in `README.md` for the full table with reasons.
In summary: upstream crashed on an unconfigured link and on a corrupt config,
mangled any link containing a quote, seized the `nxm://` scheme through
`HKEY_CLASSES_ROOT` with self-elevation, and compiled only on Windows.

This fork fixes all of those, adds Linux and macOS support, and adds tests and
CI. It is a genuine broker and does not require GameModManager or any other
particular mod manager: any program that accepts an `nxm://` link on its command
line can be registered.

## Mod Organizer 2

Upstream's README compares itself to Mod Organizer 2's `nxmhandler`. That
comparison is retained. Mod Organizer 2 is a separate project under its own
licence and is not a dependency, a vendored source, or a build input of this one.

## GameModManager

GameModManager is an adopter of this tool, not a dependency. Nothing in this
repository links against, vendors, or requires GameModManager, and the JSON
output of `list` is intentionally not coupled to any GameModManager schema.

## Third-party dependencies

See the "Licence and dependencies" table in `README.md`. Every crate is
permissive except `option-ext` (MPL-2.0), which is used unmodified and
transitively through `directories`.