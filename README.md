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
| `command.replace("%1", url)` **then** `CommandLineToArgvW` (`src/main.rs:98-100`) | The command is held as an **argv vector** and the field code is substituted inside a single element, with `%1`, `%u` and `%U` all accepted | Upstream substituted into a string and then split it, so a URL containing `"` produced a different and wrong argument vector. See the failure below. The extra spellings are not an improvement over upstream so much as the completion of the fix: upstream read its token out of a Windows `Exec` command line, and a desktop entry spells that `%u` or `%U`. |
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
* **Detection by declaration, not by a known filename.** A manager is found
  because its own desktop entry says `MimeType=x-scheme-handler/nxm`, so any
  third-party manager works without being added to this project. Before this,
  the only thing auto-detection could do was look for *our own* entry filename.
  See "Finding the mod manager".
* **`discover`: a manager can be known without being chosen.** Upstream's
  `register` was the only way a manager got into the database, so the user had
  to type the whole command before the list showed anything. A desktop entry
  that declares nxm handling is stored as a **known, unassigned** manager
  instead, and routes nothing until an `assign` says so. See "Remembering a
  mod manager without choosing it".
* **Tests and CI.** Upstream had no tests at all.
* **A release pipeline** producing one static Linux binary, the licence and the
  checksum. See "The Linux binary".

### Changed: `install` now claims the scheme, and says whether it worked

| Was | Now | Why |
| --- | --- | --- |
| "register, never seize": `install` published a desktop entry / bundle / ProgID and wrote **no** default association on any platform, so opening an `nxm://` link gave the user a chooser | `install` publishes **and** claims, as hard as each platform allows, and exits 3 when it could not finish | nxmproxy's whole job is to take `nxm://` and route it. A user who installed it and still got a chooser had a **broken install**, and the old behaviour could not tell them that. |

What is still forbidden everywhere, and is enforced by test: **touching another
mod manager's entry.** Claiming is for nxmproxy. Nothing here edits another
manager's `[Added Associations]`, its entry for any other scheme, or its
registration on any platform.

### Changed: the Linux claim writes every row it owns, not the one `xdg-mime` writes

| Was | Now | Why |
| --- | --- | --- |
| `xdg-mime default nxmproxy.desktop x-scheme-handler/nxm`, which writes **one** file - `$XDG_CONFIG_HOME/mimeapps.list` - and **replaces** that file's value for the key | A text edit that **prepends** `nxmproxy.desktop` into the `[Default Applications]` value of **ten** files: `$XDG_CONFIG_HOME/$desktop-mimeapps.list` and `$XDG_CONFIG_HOME/mimeapps.list` for every `$desktop` it can find, plus the same two rows under `$XDG_DATA_HOME/applications`. Each write is read back | `$XDG_CONFIG_HOME/$desktop-mimeapps.list` sits **above** `$XDG_CONFIG_HOME/mimeapps.list`. GameMod Manager writes the upper one, so on a KDE session the single file `xdg-mime` touches is worth nothing and the desktop opened GMM every time, on every install. |
| Reading back the file it had just written | Reading back the **whole chain** in the specification's order, through the same rule `test` uses, and applying the specification's second step: an id whose `.desktop` file is missing or does not declare the type is skipped, and the search carries on | Reading one's own file back is how `test` once printed `nxmproxy handles nxm://: yes` and exited 0 over a scheme GMM held. |
| One line in the output | A count, and a named line for every location that **refused** the write | `xdg-mime` exits 0 and writes nothing when the target directory is missing, so exit status is not evidence. A location that could not be claimed is named, or it is silently skipped. |
| Nothing on `uninstall` beyond deleting the desktop entry | The inverse edit, in every file `install` wrote. Prepending is invertible, so the previous bytes come back exactly; a file that held nothing but nxmproxy's own line is removed rather than left with an empty group | `xdg-mime default` replaces rather than appends, so nothing recorded what was there before and `uninstall` could not put the user's previous handler back. |

**The rows deliberately not written: `$XDG_CONFIG_DIRS` and `$XDG_DATA_DIRS`.**
They are read, never written. They sit *below* `$XDG_CONFIG_HOME`, the
specification stops at the first file that names a valid handler, and both
`$XDG_CONFIG_HOME` rows are written - so no entry below can outrank what was
just written. Writing them would buy nothing and cost three things that are not
nothing: root-owned directories an ordinary user cannot write, a
`~/.local/share/flatpak/exports/share` directory that *is* writable and is
flatpak's, and - the moment anyone runs `sudo nxmproxy install` - a silent
reassignment of the scheme for every user on the machine.

**Where a claim genuinely cannot be defended, and is not defended.** Another
application that writes the *same* row afterwards wins; there is no precedence
left to appeal to and no program may write another program's entry out of it.
What holds is that `test` then reports the loss, names the file it was written
to, and exits 3. A rival writing a *lower* row does lose, and that is the case
the ten rows buy.

The other half of that change is honesty about the exit code. `install` exits 0
only when nxmproxy really is the handler *and that was verified from this
process*. On macOS and on Windows it cannot be, so it exits **3** and prints the
exact click. Reporting success there is how a broken install gets automated.

### Changed: the human output says the state and nothing else

Upstream had no `list`, so this is not a fix to something of theirs - it is a
rule this fork holds itself to, and the "Was" column is **this fork's own
first version** of each line.

| Was | Now | Why |
| --- | --- | --- |
| `gamemodmanager: /path/gm --handle-nxm %1`<br>`&nbsp;&nbsp;&nbsp;&nbsp;(registered by you; no games assigned)`, a `local bin:` inventory line with a paragraph explaining what it is not, a closing "a discovered manager routes nothing until a game is assigned to it", and a separate `games:` section | `gamemodmanager: /path/gm --handle-nxm %1`<br>`&nbsp;&nbsp;&nbsp;&nbsp;* skyrimse` | The table had four sentences of prose per manager saying things the layout already shows. A manager with no `*` line under it routes nothing; that **is** the "assigned to nothing" signal, and the parenthetical restated it. The games section restated the same assignments a second time. |
| Provenance as a parenthetical: `(discovered - you have not chosen this one)` vs `(registered by you)` | Nothing in the human output; `managers[].discovered` in `--json` | Two of the notes explained a difference the user cannot act on differently, and the one that matters (nothing routes) was already visible as the absence of games. The flag is still in the contract, which is what a GUI reads. |
| `discover` ended with the assignment recipe, `install`/`list` ended with "run this other command", `test` annotated `none` with what the OS would do about it | The command that changed something, or the state that resulted | A user has `--help`. Repeating the recipe after every command meant reading the same three lines to find the one line that was news. |

`local bin:` went entirely rather than being shortened: nothing in it is
actionable, and it was never a detection source.

The one command still printed is `nxmproxy register <id> <exec> <args>` beside a
detected candidate. That is not advice - it is this machine's own path and argv,
which exist in no file the user has to go and read.

### The rule all three platforms obey: ask, then say whether it worked

| Platform | Can it claim? | How | What `install` reports |
| --- | --- | --- | --- |
| Linux | **Yes** | Ten `mimeapps.list` files under `$XDG_CONFIG_HOME` and `$XDG_DATA_HOME/applications`, each `[Default Applications]` value prepended rather than replaced, then the whole chain read back | exit 0 once the chain, resolved the way the desktop resolves it, says nxmproxy |
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

**What is copied from GMM, and what is not.** GameMod Manager's Linux
registration calls `xdg-mime default` *and* `set_mimeapps_defaults`, which forces
the same default into three files: `kde-mimeapps.list`, `mimeapps.list`, and the
data-level `mimeapps.list`. That propagation is the mechanism that works, and
this fork writes the same three plus the `$desktop-mimeapps` rows GMM misses -
see the divergence table above. What is **not** copied is the edit itself: GMM's
`set_mimeapps_scheme` calls `set_entry`, which **replaces** the value for the
key, so a second mod manager's choice is deleted from those files with nothing
recording what it was, and `unregister` resets to Nautilus rather than to
whatever the user had. nxmproxy **prepends** instead, so every other id keeps
its place in the ranked list and comes back byte for byte on `uninstall`.

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

`install` writes the desktop entry, then the scheme into every row it owns:

```
~/.local/share/applications/nxmproxy.desktop
    MimeType=x-scheme-handler/nxm;

~/.config/{kde,gnome,xfce,lxqt,…}-mimeapps.list     <- every $desktop it finds
~/.config/mimeapps.list
~/.local/share/applications/{kde,gnome,xfce,lxqt,…}-mimeapps.list
~/.local/share/applications/mimeapps.list
    [Default Applications]
    x-scheme-handler/nxm=nxmproxy.desktop;<every id that was already there>
```

The desktop entry makes nxmproxy *eligible*; the rows make it what the desktop
opens. Both the desktop-specific rows and the plain ones are written, because
`$desktop-mimeapps.list` outranks `mimeapps.list` and the plain one alone is
worth nothing. Each value is **prepended**, so an existing mod manager stays in
the list one rank down rather than being deleted. Each write is read back, and
the claim is then verified by resolving the whole chain the way the desktop
resolves it - not by reading nxmproxy's own file back.

`$XDG_CONFIG_DIRS` and `$XDG_DATA_DIRS` are **read, never written**: they cannot
outrank `$XDG_CONFIG_HOME`, which is written in full, and writing into root's and
flatpak's directories is not a per-user tool's business. A location that refuses
the write is named in the output, never silently skipped.

`install` does not shell out to `xdg-mime` at all. `xdg-mime default` exits 0
and **writes nothing** when `$XDG_CONFIG_HOME` does not exist, which it does not
on a fresh account, and it **replaces** a key's value rather than prepending to
it. The sweep is one pure text function instead: every file is created if
missing, written, read back, and the `[Default Applications]` value re-checked.
Nothing is reported as claimed on the strength of an exit code. There is no
fallback path that depends on `xdg-mime` being installed, which is why the
end-to-end test removes it from `PATH` and still passes.

`uninstall` removes nxmproxy's id from every one of those keys and leaves every
other id, every other key, and every other group alone, in every file. A file
that held nothing but nxmproxy's own line is removed rather than left with an
empty `[Default Applications]` header. Because the claim is a prepend, the
inverse is exact: a file's previous bytes come back byte for byte.

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
> * The one integration test that still runs the real `install` on macOS
>   accepts a process killed by a signal as "needs user", never as success, and
>   prints a note when it happens. Every other assertion that would have needed
>   a post-install state was moved to a test that runs on Linux and Windows
>   instead, or re-expressed through `list --json`, which never calls `install`.
>   So **the macOS install path is not covered end to end**, and this README
>   does not claim it is. The assertion that still runs on macOS is the one that
>   matters most: nxmproxy never reports a claim it cannot verify.

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
| **Auto-detect from the application's own desktop entry** | **Chosen** - keyed on its `x-scheme-handler/nxm` declaration, not on a filename known in advance |

The chosen approach asks the applications themselves. A desktop entry that
declares `MimeType=x-scheme-handler/nxm` is saying, in the XDG sense, "I take
nxm links" - that is the key the specification defines for exactly this, it is
what the desktop itself uses to route a link, and MO2 and Vortex publish it the
same way. It is a **declaration, not a heuristic**, and it is why a manager this
project has never heard of can be found without a single line of code naming it.

The directories scanned, all read-only:

| Where | Why |
| --- | --- |
| `$XDG_DATA_HOME/applications` | The specification's first application directory. |
| every `$XDG_DATA_DIRS/applications` | System-wide. Defaults to `/usr/local/share` and `/usr/share` when unset, as the specification says. |
| `~/.local/share/applications` | Where per-user entries actually land. |
| `$XDG_CONFIG_HOME/applications`, `~/.config/applications` | Listed by the specification, and where a user who hand-dropped an entry means it. |

`~/.local/bin` (`$XDG_BIN_HOME`) is searched when **resolving** an executable,
and is reported by `list`, but it is **never a detection source**. What is in it
says a program runs; it says nothing about whether the program takes an nxm
link. It is worth knowing because installer-managed apps drop a symlink there
for every app they own - on the machine this was written on,
`~/.local/bin` holds 60 entries, 42 of them symlinks, including
`hd2arsenal -> ~/Applications/HD2Arsenal`.

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
* **Only when the parsed argv contains `%1`, `%u` or `%U`.** A manager that never
  receives the link would be started for every download and handed nothing.
* **Never nxmproxy itself.** `install` publishes our own entry *before* it runs
  detection, and the router is not its own destination. Matching is by
  executable path **and** by entry name, because `install` writes
  `current_exe()` and an installed older build is a different file while being
  exactly as much the router as we are.
* Otherwise: **refuse, and print the exact command to type**, quoted where the
  path has a space in it. A refusal with instructions beats a guess, and it
  exits 3 so a script knows.

### Two traps in one real entry, both handled

The entry this was written for, HD2Arsenal 0.36.2, unmodified, contains both of
the things that break a naive reader. `src/xdg.rs` carries it verbatim as a test
fixture for exactly this reason.

**A quoted `Exec=`.** `Exec="/home/petrica/Applications/HD2Arsenal" --no-sandbox %U`
split on whitespace yields `"/home/petrica/Applications/HD2Arsenal"` as argv[0],
quotes attached, so the file does not exist and the manager silently never
receives a link. It is parsed into a real argv:

```text
["/home/petrica/Applications/HD2Arsenal", "--no-sandbox", "%1"]
```

**An `Exec=` in the wrong group.** The entry also has a
`[Desktop Action Uninstall]` group, and that group carries its own `Exec=`:

```ini
Exec=/home/petrica/Applications/AppManager uninstall "/home/petrica/Applications/HD2Arsenal"
```

Reading `Exec=` by scanning the file for the key rather than by group picks that
one up, and delivering a link to it asks a package manager to **uninstall the
mod manager**. `Exec=` is therefore read from `[Desktop Entry]` and nowhere else.

The asymmetry is deliberate and is the other trap: in that same entry, the
`MimeType` line is **not** under `[Desktop Entry]` at all - it is under
`[Desktop Action Uninstall]`, twice listing `x-scheme-handler/nxm`. A strict
group-only reader does not see HD2Arsenal at all, which defeats the entire
purpose. So `MimeType` is read from any group, while `Exec=` is read only from
`[Desktop Entry]`. That is safe in the one direction that matters: a stray
`MimeType` under an action group can never make `AppManager uninstall` into
something we would spawn, because the argv still comes from `[Desktop Entry]`.

`Categories=` is deliberately **not** used. HD2Arsenal ships `Categories=Utility;`,
the same as a text editor, so category matching would have taken a calculator.

### Remembering a mod manager without choosing it

Detection above is **transient**: it prints candidates and the user types
`register`. `discover` is the other half - it puts what it found into the
database, so the manager is there, usable, and waiting.

```sh
nxmproxy discover          # remember every manager this machine declares
nxmproxy assign hd2arsenal helldivers2   # and now route this game to it
```

**Found is not chosen.** A discovered manager is stored with **no assignment** -
not the game, not the `_` catch-all - and it routes nothing. That is the entire
point of storing it separately, and it is why routing rule 3 counts only ids
the user registered: a remembered manager that won rule 3 by default would be
the exact failure this tool exists to prevent, arriving through the one rule
that was written to be safe for a user who had nothing configured. `list` says
which is which, and a GUI reads `managers[].discovered`.

**Where it runs, and why not in `install`.** It is a separate command, and
`install` only *points* at it:

* `install` already has a documented rule - it never extends a configuration it
  did not create - and a second, hidden write path inside `install` is exactly
  how that rule gets lost. A user who runs `install` to claim a scheme should
  not silently acquire a database full of suggestions.
* `install` is the command a wrapper script and a package post-install hook
  call. A database write that nobody asked for is much worse in that context
  than in one a person typed.
* It is therefore **re-runnable on demand**, which is what users need anyway:
  games and mod managers get installed after setup, and a one-shot scan at
  install time has nothing to say about them.

It is safe to run as often as you like:

* **It never clobbers.** An id already known is left exactly as it is, flag,
  pipe and assignments included. A scan cannot undo a `register` or an `assign`.
* **It never removes anything.** A manager whose application has been
  uninstalled stays in the database and `list` marks it `unavailable: ... it was
  uninstalled`. Forgetting an entry because a file went missing is how a list
  silently loses rows, and the user would have to find and retype the whole
  thing.
* **Registering one by hand clears its `discovered` flag.** Having answered the
  question by hand is the answer; leaving the flag would keep a manager the user
  chose out of routing for ever.
* **Every guard detection has still applies.** An entry whose executable does
  not resolve is named and refused, never stored broken. And remembering *every*
  candidate does not resolve an ambiguity between them - two remembered managers
  are two unassigned managers, and the link still stops.

`discover` reads **only** the declaration source, never the curated list. The
curated list is a set of guesses about executable names; writing guesses into
the user's database is precisely what declaration-based detection exists to
avoid.

### What a refusal looks like

Two managers is the common case once detection works, because the whole point is
that third-party managers are now visible. On the machine this was written on:

```text
2 applications on this machine declare nxm:// links:
nxmproxy will not choose between them for you. Register the one you want:

  GameMod Manager
    nxmproxy register gamemodmanager /home/u/.../gamemodmanager --handle-nxm %1

  HD2Arsenal
    nxmproxy register hd2arsenal /home/u/Applications/HD2Arsenal --no-sandbox %1

then pick who owns which game:

    nxmproxy assign <name> <game>
```

An entry that declares nxm handling but points at a binary that is not installed
is reported rather than silently dropped - "nothing found" for a user who can
see the desktop entry would be a lie:

```text
skipped 1 application that declared nxm:// handling but nxmproxy cannot start:
  Amethyst Mod Manager (NXM Handler) (/home/u/Applications/Amethyst Mod Manager) - not on this machine
```

With exactly one registered manager, routing rule 3 sends every unassigned
domain to it, so a fresh install routes with no further configuration.

> **Where this does not work, stated plainly.** The desktop-entry signal is a
> Linux mechanism. On macOS and Windows there is no equivalent file, so unless
> the manager is on `PATH`, `install` finds nothing, prints the `register`
> command, and exits 3. That is a real asymmetry, not a claim of parity: on those
> two platforms an out-of-the-box install still needs one typed command. The
> alternative - guessing at a `Program Files` path or scanning an `.app` bundle -
> would be a guess, and this tool does not guess.

`nxmproxy list --json` reports the same judgement two ways, so a GUI can offer
the same one-click register: `present` per entry in the curated
`known_managers`, and the whole `declared_managers` list for applications found
by declaration.

## Command line

| Command | Meaning |
| --- | --- |
| `install` | Install nxmproxy as the `nxm://` handler: full configuration **and** claim. |
| `uninstall` | Remove nxmproxy as the handler (symmetric with `install`). |
| `register <name> <exec> [<args>...]` | Add a mod manager. |
| `unregister <name>` | Remove a mod manager, and its game assignments. |
| `assign <manager> <domain>` | Routing. `<domain>` is the `nexusmods.com` subdomain, or `_` for every game with no assignment. |
| `discover` | Find every mod manager this machine declares and **remember** them, assigned to nothing. Re-runnable. |
| `list [--json]` | The routing table. `--json` is the machine-readable contract. |
| `test` | Report whether nxmproxy is registered and who the OS says handles `nxm://`. Exit 0 if registered, 1 if not. |
| `pipe <manager> <pipe>` | Give a manager a named pipe to try before spawning it. |
| `url <nxm url>`, or bare `<nxm url>` | Forward a link. The bare form is one argument and is what a hand-written or third-party `Exec=nxmproxy %u` hands over; the `url` form is what nxmproxy's own desktop entry invokes. |

`list` prints the routing table and nothing else:

```
mod managers:
  gamemodmanager: /usr/bin/gamemodmanager --handle-nxm %1
  vortex: /opt/vortex/Vortex %1
    * skyrimse
    * fallout4
```

A `* <game>` line, one tab in, for every game assigned to that manager. **A
manager with no `*` line under it is assigned nothing and routes nothing** -
which is why there is no "discovered" or "not chosen" marker next to it.
`every unassigned game` is the `_` catch-all. A trailing note says what the
command line cannot: `(unavailable)` when the executable is not on this machine,
`(no link code)` when the argv has no `%1`/`%u`/`%U`, and the pipe name when one
is set.

### The link field code: `%1`, `%u`, `%U`

Inside any argument, the place the link goes is marked with `%1`, `%u` or
`%U`. All three mean the same thing and all three substitute. `%1` is
nxmproxy's own token; `%u` and `%U` are the desktop entry specification's, and
they are what an Electron, AppImage or Flatpak entry writes, so a manager
registered with the argument vector copied out of its own desktop entry works:

```sh
# This is the line a user types, and it is the one the test asserts on.
nxmproxy register "HD2Arsenal" /home/petrica/Applications/HD2Arsenal --no-sandbox %U
```

The substitution happens **inside one argument**, never by splitting a string,
so a URL containing a space or a quote arrives as exactly one argument. An
executable path containing a space is equally not a problem: the command is
stored as an argv vector, not a string. That is the bug upstream had, and it is
the reason `register` takes an exec plus a vector rather than a command string.

`--` before the argument vector is **optional**. It still works and still means
the same thing; it is not required because a user reading `Exec=` off a desktop
entry does not type it, and rejecting that with exit 2 was a refusal of the
ordinary case rather than of a mistake.

`list --json` reports `takes_url` for every entry, which is true when **any**
argument holds one of the three codes. A GUI can show it before the user clicks
a link.

### Names: upstream, and this fork

Upstream (`TanninOne/nxmproxy`, `src/main.rs`, HEAD `f9f72e0`) had
`Url`, `Register`, `Deregister`, `Assign`, `Pipe`, `Install` and `Test`. The
mapping, verified against that source:

| Upstream | Here | Change |
| --- | --- | --- |
| `Url` | `url` | - |
| `Register { manager, command }` | `register <name> <exec> [<args>...]` | **Same name, different arguments.** Upstream's took a command *string*; this takes an exec plus an argv vector |
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
| 3 | needs-user | **Registered, but one step is left that you must do by hand.** Either the OS refused or hid the claim, or a browser decides `nxm://` for itself. Nothing failed; nxmproxy did everything the platform allows |

Code 3 is what makes `install` scriptable. It is deliberately neither 1 nor 0:

* not **1**, because nothing went wrong - a wrapper that aborts on 1 would treat
  a normal macOS or Windows install as a failure;
* not **0**, because on macOS and Windows nxmproxy cannot verify the claim, and
  a wrapper that reads 0 would automate an install that routes nothing.

The same code also covers "no mod manager to forward to" and "a browser overrides
the OS", because typing `nxmproxy register ...` and editing a browser's own
configuration are exactly such steps. The message always says which.

### Routing rules

1. An exact assignment for the domain.
2. The `_` catch-all.
3. **The only manager the user chose**, if there is exactly one.

Rule 3 is why an empty configuration works out of the box: a user with one mod
manager needs nothing configured. It deliberately does **not** fire when two or
more managers are registered. With two, there is no obvious owner, and a wrong
guess is a link landing in someone else's mod queue, so nxmproxy says so and
exits non-zero rather than guessing.

**"Chose" means registered by hand.** A manager that `discover` found is
registered too, and rule 3 deliberately does not count it: it counts only ids
that are **not** in `discovered`. Letting a remembered manager win by default
would be the same misroute arriving through the back door, wearing a shortcut
that reads as "nothing is configured, so anything goes". With only discovered
managers configured, nxmproxy refuses and says which it knows and what to type
(`Route::Unassigned`) rather than guessing. An `assign` does route a discovered
manager - that is a user typing a command, and it is the whole point of
remembering one.

---

## `list --json`

Stable, versioned, and byte-stable across runs (`BTreeMap` plus declared field
order). Unknown keys are ignored by readers.

```json
{
  "version": 3,
  "config_path": "/home/u/.config/nxmproxy/config.toml",
  "config_writable": true,
  "managers": [
    {
      "id": "vortex",
      "exec": "/opt/vortex/Vortex",
      "args": ["%1"],
      "pipe": null,
      "takes_url": true,
      "exec_exists": true,
      "discovered": false
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
  "declared_managers": [
    {
      "id": "hd2arsenal",
      "name": "HD2Arsenal",
      "entry": "/home/u/.local/share/applications/hd2arsenal.desktop",
      "mime_types": [
        "x-scheme-handler/rsnl",
        "x-scheme-handler/nxm",
        "application/x-hd2a-project"
      ],
      "exec": "/home/u/Applications/HD2Arsenal",
      "args": ["--no-sandbox", "%1"]
    }
  ],
  "warnings": []
}
```

| Field | Why it is there |
| --- | --- |
| `version` | A GUI must be able to refuse a shape it does not understand instead of misreading it. **2** added `declared_managers`; a reader that only knows 1 should refuse rather than ignore the new managers and present a GUI that cannot register HD2Arsenal. **3** added `managers[].discovered`; a reader that only knows 2 must refuse rather than present a manager the user never chose as one that is set up. |
| `managers[].discovered` | `true` when `discover` found this manager on this machine and nobody has chosen it. **Found is not chosen**: nxmproxy itself refuses to route to one of these until a game is assigned to it. A GUI must present it as a suggestion, never as the manager that is already handling links. |
| `config_writable` | `false` when the file exists but does not parse. A GUI must **not** offer to save in that state, because saving would overwrite the file the user still needs to repair. |
| `managers[].takes_url` | `false` when no element of `args` contains `%1`, i.e. the manager would be launched and handed nothing. A GUI can show this before the user clicks a link. |
| `managers[].exec_exists` | Whether the executable is actually present on this machine, so a GUI can flag a broken entry instead of letting the user discover it when a link silently fails. |
| `known_managers` | The curated list, so a GUI need not hardcode it. **Data only, never registered.** Each entry carries a `note` saying what is and is not verified about it. |
| `declared_managers` | Applications found by their **own** `x-scheme-handler/nxm` declaration. Not in `known_managers` because we cannot have known about them in advance - that is the whole point. **A live scan of the desktop directories, independent of the config**: it reports what is there now, including managers `discover` already stored under that id, and `list` does not store anything. Each entry carries the `entry` it was read from, so a GUI can say where it found it rather than presenting a command as magic. |
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
discovered = ["hd2arsenal"]   # found by `discover`, not chosen by you

[games]
skyrimse = "vortex"
_ = "vortex"          # catch-all for every game with no assignment

[managers.vortex]
exec = "/opt/vortex/Vortex"   # a path, or a bare name resolved through PATH
args = ["%1"]                 # the link goes here
pipe = "vortex_download"      # optional, Windows only
```

`discovered` is a list of manager ids that `discover` found and the user has not
registered by hand. **It is written before any table header** - TOML would read
it as a key of the next table instead, ignore it as unknown, and silently drop
the provenance. It is omitted entirely when empty, so a user who never runs
`discover` sees no trace of it, and a stale id whose manager failed validation
is dropped on load.

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

`.github/workflows/ci.yml` **builds**. It runs `cargo fmt --check`,
`clippy -D warnings` and `cargo build --release` on **Linux, macOS and
Windows**, and that is all. It does not run the tests: the suite lives in the
repo and `cargo test` runs all of it, but running it on every commit on three
platforms costs minutes and tells the author things they can see in five
seconds on their own machine. Local verification is the author's job; CI exists
for the one thing a single machine cannot do, which is proving the crate
compiles everywhere at once.

**The trade, stated plainly.** This job used to end with `cargo test` and a CLI
smoke test, and both of those *executed the built binary* - on macOS and Windows
as well as on Linux. That was the only run-check nxmproxy's binary ever got on
those two platforms, and it is given up on purpose. macOS and Windows are
**compile-only** from here: a change that compiles there but misbehaves when
run is not caught here. That is the accepted cost of not spending minutes per
commit, and it is why the claims below are marked unverified rather than
tested.

> **What the CI build proves on macOS and Windows, precisely:** the crate
> compiles there, and every load-bearing decision in it is pure - the platform
> module text, the macOS `LSSetDefaultHandlerForURLScheme` decision, the
> Windows registry operation list and the Windows Default Apps URI. That is why
> the rule that **macOS and Windows never report a verified claim** is enforced
> from a Linux build rather than only on the platform it concerns.
>
> **What it does not prove:** that `LSSetDefaultHandlerForURLScheme` makes
> macOS offer the bundle, that the prompt appears, or that `reg.exe` writes the
> Windows keys as intended. Those need a real machine and a real click, and are
> called out as unverified above.
>
> Nothing that the tests caught is unchecked, only run by hand. For the record,
> what they caught while they were being written - and what no single-platform
> build could have seen:
>
> * macOS: the unit tests made a real LaunchServices call and the binary died
>   with SIGTRAP before printing a test name.
> * macOS: two integration tests died because the real binary aborts on a
>   headless runner.
> * Windows: tests that run `install` share one real `HKCU` and the sandbox
>   cannot isolate it, so cargo's parallel threads overwrote each other and
>   `test` reported registered before anything had been installed.
> * Linux: the argv-recording test waited for a file to *exist* rather than to
>   be finished, so on a loaded runner it read a half-written argv and reported
>   a link that had lost its URL. It had been green locally for months.
> * Windows: the named-pipe path was missing its `io::Write` import, which no
>   Linux build can see because the function is `#[cfg(windows)]`, and would
>   have failed to compile for every Windows user.

### The Linux binary

`.github/workflows/release.yml` builds a release artifact. It is triggered by
pushing a tag matching `v*`; it does not create tags, and a manual run
(`workflow_dispatch`) builds the same artifact and uploads it **without**
publishing a release, so the pipeline can be proved before anyone cuts a
version.

```sh
cargo build --release --target x86_64-unknown-linux-musl
```

**Static, musl, not glibc.** The glibc build of this crate links
`GLIBC_2.39` and will not start on Ubuntu 22.04 (2.35), Debian 12 (2.36), or
anything older: `version 'GLIBC_2.39' not found`, for a program that would have
run. nxmproxy needs no C library facility musl lacks - no NSS, no locale
collation, no `dlopen`, no host lookup beyond `$HOME` and `$XDG_*` - so the
release binary links nothing and runs on any Linux distribution, of any age,
glibc or musl. The cost is about 400 KB.

The job refuses to publish a binary that links a shared library, and runs the
artifact inside a container that has **no glibc at all** (`alpine`,
`--network=none`) before it will attach it. A binary that compiled is not a
binary that runs, and the whole point of shipping one is that it runs on a
machine the author has never seen.

Each release carries `LICENSE.md` and `NOTICE.md` beside the binary and states
the licence and upstream attribution in its notes, because GPL-3 obligations
travel with a distributed binary and upstream's credit is part of this fork.

### Negative controls

Every load-bearing test has a mutation control that proves it can actually go
red. The controls are mutation-and-observe, not assertion-counting:

| # | Mutation | Test that catches it |
| --- | --- | --- |
| 1 | the claim writes one file, as `xdg-mime` does, and never the desktop-specific rows | `linux_install_claims_the_scheme_in_every_file_and_buries_nobody` |
| 2 | the claim **replaces** the value instead of prepending, deleting the other managers | `linux_install_claims_the_scheme_in_every_file_and_buries_nobody`, `the_claim_needs_no_third_party_tool_and_buries_nobody` |
| 3 | `uninstall` reverses one file and leaves nxmproxy in the rest | `linux_uninstall_restores_every_file_it_claimed` |
| 4 | the file editor drops other lines | `setting_a_default_leaves_every_other_line_byte_for_byte` |
| 5 | a claim that needs the user exits 0 | `only_a_verified_claim_exits_zero` |
| 6 | `Claim::needs_user` reports a verified claim | `windows_never_reports_a_claim_it_could_not_make`, `a_claim_is_never_reported_as_verified` |
| 7 | the "no manager, so nothing is routed" guard is removed | `install_never_calls_a_routing_setup_done` |
| 8 | `register-manager` survives as an alias | `the_command_names_are_the_final_surface_and_the_old_ones_are_gone` |
| 9 | the `$XDG_CONFIG_HOME` fix is removed, restoring `xdg-mime`'s silent no-op | `install_and_uninstall_round_trip` |
| 10 | `present` is satisfied by a desktop entry that merely exists | `a_manager_is_detected_only_when_it_can_be_proven_present` |
| 11 | detection accepts an `Exec` line with no `%1` | `install_refuses_a_manager_it_could_not_use` |
| 12 | detection fires on a configuration the user built | `install_refuses_a_manager_it_could_not_use` |
| 13 | detection registers a manager whose executable does not resolve | `install_refuses_a_manager_it_could_not_use` |
| 14 | `%u` and `%U` are dropped from the field codes, leaving `%1` alone | `the_desktop_entry_spellings_of_the_field_code_all_substitute`, `a_manager_registered_from_a_desktop_entry_receives_the_link` |
| 15 | substitution re-scans the text it just wrote, so a URL containing `%u` is substituted into a second time | `substitution_is_one_pass_so_a_url_containing_a_field_code_survives` |
| 16 | `--` before the argument vector is made mandatory again | `a_manager_registered_from_a_desktop_entry_receives_the_link` |
| 17 | a discovered manager counts toward rule 3, so it wins by default | `a_remembered_manager_never_routes_a_link_the_user_did_not_ask_for`, `a_discovered_manager_never_owns_a_game_until_it_is_assigned` |
| 18 | the `Ambiguous` list offers a manager the user never chose | `a_discovered_manager_is_never_one_of_the_choices_offered` |
| 19 | `discover_manager` overwrites an id that is already known | `a_rescan_does_not_clobber_a_registered_manager_or_an_assignment`, `a_discovered_manager_is_remembered_without_being_chosen` |
| 20 | `register` no longer clears the `discovered` flag | `registering_a_discovered_manager_by_hand_makes_it_a_choice` |
| 21 | load keeps a `discovered` id whose manager failed validation | `a_stale_discovered_id_is_dropped_on_load` |
| 22 | `list` reverts to "executable not found on this machine" | `an_uninstalled_application_is_marked_unavailable_and_not_forgotten` |
| 23 | `list` stops printing where each manager came from | `list_shows_each_game_under_the_manager_that_owns_it` |
| 24 | `install` runs discovery itself | `install_never_writes_a_discovered_manager_on_its_own` |
| 25 | `discover` also assigns the catch-all to what it found | `discover_remembers_the_real_arsenal_entry_and_assigns_it_nothing`, `a_remembered_manager_never_routes_a_link_the_user_did_not_ask_for` |
| 26 | `declared_candidates` accepts an `Exec` that does not resolve | `discover_keeps_every_candidate_and_stores_nothing_it_can_be_start` |
| 27 | a re-scan claims "remembered" when it remembered nothing | `a_rescan_does_not_clobber_a_registered_manager_or_an_assignment` |
| 28 | the Arsenal fixture is tidied - the duplicate scheme, or the `MimeType` moved into `[Desktop Entry]` | `the_arsenal_fixture_is_still_the_file_a_user_has` |
| 29 | the `local bin:` inventory line is printed by `list` again | `list_is_exactly_the_documented_shape`, `no_command_prints_lesson_text` |
| 30 | `test` annotates `nxm:// handler: none` with what the OS will do about it | `no_command_prints_lesson_text` |
| 31 | `discover` ends with the assignment recipe again | `no_command_prints_lesson_text` |
| 32 | the `* <game>` line loses its tab, so a game sits at the same indent as its manager | `list_shows_each_game_under_the_manager_that_owns_it`, `list_is_exactly_the_documented_shape` |
| 33 | `mimeapps_files` resolves one file instead of the whole chain, the original bug | `the_desktop_itself_names_nxmproxy_after_install`, `list_json_carries_the_resolution_the_terminal_prints`, `test_explains_a_scheme_two_applications_both_declare` |
| 34 | the chain is built in the wrong order, so the plain row outranks the `$desktop` row | `the_chain_is_the_specifications_eight_rows_in_order`, `the_users_own_file_wins_over_the_system_one`, `the_desktop_itself_names_nxmproxy_after_install` |
| 35 | a write that fails is reported as `Ok`, the `xdg-mime` silent-no-op | `a_location_that_cannot_be_claimed_is_named_with_the_reason` |
| 36 | the on-disk discovery of a `$desktop-mimeapps.list` is disabled | `a_desktop_file_that_already_exists_is_claimed_while_another_desktop_runs` |
| 37 | a browser override stops being reported and `test` exits 0 over it | `test_reports_a_browser_that_decides_nxm_for_itself` |

Controls 29-32 are the ones that matter for the output-shape rules, and all four
were run before this table was written. 29, 30 and 31 each came back **red on
the first attempt**, quoting the sentence that had come back; 32 came back red
with the `left` and `right` blocks showing the two-space line against the
four-space one. 29 and 32 fail **two** tests each, deliberately: one compares
the whole stream and cannot pass on prose nobody wrote an assertion for, the
other reads the per-manager block and cannot pass on a shape change. Neither
test alone would have caught both kinds.

One control is deliberately reported as **green**, and that is the point of
running them at all. Breaking `Claim::exit_code` to always return 0 did **not**
turn `install` green, because the "no manager, so nothing is routed" guard sets
exit 3 independently. Removing that routing guard instead *does* turn `install`
green at 0, so the two are genuinely independent and either one holds the line
alone. The same mutation is not harmless overall: it still fails five unit tests
and the two `uninstall` tests, which assert `needs_user` with no routing guard to
back them up. That is defence in depth working, and it is also a reminder that a
control only proves what the test it targets actually asserts - control 6 above
exists for the exit code itself.

A second control came back green and had to be redesigned rather than reported.
Control 18 changes what the `Ambiguous` branch puts in its list, and the test
written for it - "a discovered manager does not make a single chosen manager
ambiguous" - **never reached that branch at all**. With one manager the user
chose, `resolve` returns from the `chosen.len() == 1` arm and builds no list
whatsoever, so the mutated line did not execute and the suite was right to stay
green. The test name promised a property its body could not observe.

That is the second time in this project a control has been green on the first
attempt, and it is the same failure both times: a test that asserts the
*outcome* of a decision without ever reaching the decision. The test now builds
**two** configurations - one chosen manager plus one discovered (which does
return early, and which is caught by control 17), and two chosen plus one
discovered (which does reach the list, and fails with
`["hd2arsenal", "mo2", "vortex"]` against `["mo2", "vortex"]`).

The rest found real bugs while they were being written:

* Control 9 reproduced a live defect: `xdg-mime default` exits **0 and writes
  nothing** when `$XDG_CONFIG_HOME` is missing, which is the state of a fresh
  account - its only complaint goes to stderr. `install` claimed a scheme it had
  not claimed. Fixed by creating the directory first, and the read-back now
  catches it if that regresses.
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

### Two controls that came back green, and what they exposed

Both are recorded here because the failure is the useful part, and because it is
the **third** and **fourth** time in this project a control has been green on the
first attempt - always for the same reason: a test that could not observe the
thing it claimed to observe.

**Control 33 came back green in the direction that does not hurt.** The read side
narrowed to a single file made `test` report a **loss** while the desktop claimed
a **win** - false negative, the direction a user can see and correct. The
direction that costs a session is the opposite: `nxmproxy handles nxm://: yes`
over a scheme the desktop had already given to GMM. No test compared `test`
against `xdg-mime` at the point in `the_desktop_itself_names_nxmproxy_after_install`
where GMM holds the *higher* row and nxmproxy the lower one - the mutation was
simply not on that path's result. The comparison is now asserted there, and with
the mutation reapplied the test goes red at exactly the false green:

```
nxmproxy handles nxm://: yes
  Default Applications   nxmproxy.desktop  <- .../home/.config/mimeapps.list
  winner: nxmproxy.desktop - the only declaration on this machine
```

**Control 36 came back green across the whole suite** - all 161 tests - with the
discovery of a `$desktop-mimeapps.list` that already exists on disk completely
disabled. The fixture used `kde`, and `kde` is one of the four names the
specification's own text offers, so the row was written anyway and the discovery
did nothing observable. `existing_desktop_lists` was dead code wearing a test.
Rewritten around `x-cinnamon` - a name that only walking the four roots can find -
it goes red immediately.

A third control is reported here for the same reason. An early attempt at
control 2 "replaced the prepend with a replace" came back **green** because the
mutation removed a duplicate-collapse filter and added an equivalent
`retain` after it: two different spellings of the same function. The control was
not a control. Re-stated as `set_default_ids(text, key, &[id])` it turns four
tests red with the other managers deleted from the file.

### Known coverage gap

`unclaim` does its own per-id filtering to decide which other managers survive on
the `[Default Applications]` line, and **that filter has no test**. Breaking it
so it keeps nothing leaves all 121 tests green, because the `uninstall` fixtures
never put a second manager on the same `[Default Applications]` nxm key - the
shared-key case is only exercised one level down, against `set_default_ids`
directly, which is not the code path `unclaim` uses. Reaching it needs a
`[Default Applications]` line reading `nxm=vortex.desktop;nxmproxy.desktop;`, which
`xdg-mime` never writes (it replaces rather than appends) but a hand-edited file
can contain. The code is correct by inspection; it is simply not defended by a
control.

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