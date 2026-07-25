libratbag
=========

<img src="https://libratbag.github.io/_images/logo.svg" alt="" width="30%" align="right">

libratbag provides **ratbagd**, a DBus daemon to configure input devices,
mainly gaming mice. The daemon provides a generic way to access the various
features exposed by these mice and abstracts away hardware-specific and
kernel-specific quirks.

**As of version 2.0, the ratbagd daemon has been rewritten in Rust and migrated to an unprivileged session daemon.** The daemon and the companion `ratbagctl` CLI tool are consolidated in a unified codebase. The daemon speaks the same `org.freedesktop.ratbag1` DBus API (version 2) and uses the same device database and `.device` files.
The old C daemon has been removed and the CLI replaced with a new Rust `ratbagctl` tool built on the same DBus API.

> ⚠️ **Breaking change: ratbagd now runs on the session bus, not the system
> bus.** This intentionally breaks compatibility with current
> [Piper](https://github.com/libratbag/piper/) releases, which connect to
> ratbagd on the **system** bus. See
> [Session Bus Migration](#session-bus-migration-breaking-change-for-piper)
> below for what changed and why.

Session Bus Migration (Breaking Change for Piper)
-------------------------------------------------

`ratbagd` no longer runs as a `root` system daemon on the **system bus**.
It now runs as an unprivileged **session daemon** on the user's **session
bus** (`zbus::Connection::session()`), spawned and managed by
`systemd --user`. Device access is granted to the physically seated user via
`udev` + `uaccess` instead of by running as root.

### What this breaks

Existing [Piper](https://github.com/libratbag/piper/) releases connect to
ratbagd over the **system bus** (`Gio.BusType.SYSTEM`). Because the daemon no
longer claims `org.freedesktop.ratbag1` on the system bus, **stock Piper can
no longer find or talk to ratbagd** and will report that the daemon is not
running. Piper would need to be patched to connect to the session bus to work
with this daemon. [Twister](#twister-desktop-gui), the GUI included in this
repository, already connects on the session bus.

The new `data/60-ratbagd.rules` udev rule (which tags raw HID interfaces with
`uaccess`) does **not** itself affect Piper — it only governs `/dev/hidraw*`
node permissions. The compatibility break comes from the bus migration, not
the udev rule.

### Why we broke compatibility

The legacy architecture ran `ratbagd` as `root` solely to bypass file
permissions on `/dev/hidraw*`. This violates the principle of least
privilege: configuring DPI, RGB, or button bindings does not warrant
administrative access. USB hardware is untrusted, and a malicious or
compromised device can send malformed HID reports crafted to exploit parsing
flaws — and any such exploit in a root daemon becomes a full system
compromise (privilege escalation).

Running as an unprivileged session daemon **contains the blast radius**: an
exploit triggered by a malicious mouse is confined to the unprivileged user's
session, leaving the host OS intact. It also models device settings correctly
as per-user preferences and behaves sensibly on multi-user systems, where the
kernel's seat management (`systemd-logind`) grants hardware access only to the
physically seated user.

Supported Devices
-----------------

libratbag supports devices from Asus, Etekcity, GSkill, Logitech (HID++ 1.0
and 2.0, G300, G600), MarsGaming, OpenInput, Roccat (including Kone Pure /
Kone EMP variants), Sinowealth (including Nubwo), and Steelseries.

See [the device files](https://github.com/niltonperimneto/libratbag-rs/tree/master/data/devices)
for a complete list of supported devices.

Users interact through a GUI like
[Twister](twister/) (a modern Tauri + Svelte desktop app included in this
repository), or the `ratbagctl` command-line tool (see below).
[Piper](https://github.com/libratbag/piper/) is **not currently compatible**
because the daemon moved to the session bus — see
[Session Bus Migration](#session-bus-migration-breaking-change-for-piper).

What Changed in the Rust Rewrite
---------------------------------

The core `ratbagd` daemon has been rewritten from C to Rust. Key changes:

- **Async, actor-based architecture** — each connected device gets its own
  Tokio task (actor) that owns the HID file descriptor and serializes all
  hardware I/O through an `mpsc` channel. DBus interface objects share
  device state via `Arc<RwLock<DeviceInfo>>`.
- **Structured driver framework** — all drivers implement a common
  `DeviceDriver` trait (`probe`, `load_profiles`, `commit`). Hardware I/O is
  abstracted behind `DeviceIo` (async hidraw read/write, feature report
  ioctls, request/response matching with timeouts and retries).
- **Full driver parity** — all 15 drivers from the C codebase have been
  ported: `asus`, `etekcity`, `gskill`, `hidpp10`, `hidpp20`,
  `logitech_g300`, `logitech_g600`, `marsgaming`, `openinput`, `roccat`
  (with Kone Pure / Kone EMP), `sinowealth`, `sinowealth_nubwo`, and
  `steelseries`.
- **Dev-hooks feature** — compile with `--features dev-hooks` to enable
  `LoadTestDevice` / `ResetTestDevice` DBus methods on the Manager
  interface, allowing integration tests to inject synthetic devices without
  real hardware.
- **Session daemon, not a root system daemon** — the daemon now runs
  unprivileged on the user's session bus, managed by `systemd --user`, with
  device access delegated via `udev` + `uaccess`. The legacy system-bus DBus
  policy and `User=root` activation are no longer used. See
  [Session Bus Migration](#session-bus-migration-breaking-change-for-piper).
- **License change** — the Rust daemon is licensed under **GPLv3**. Supporting assets (service templates, device data, docs) remain under MIT/Expat (see the License section below).

### What stays the same

- The `org.freedesktop.ratbag1` DBus API (version 2) — all interfaces
  (`Manager`, `Device`, `Profile`, `Resolution`, `Button`, `LED`) are
  wire-compatible with the C daemon (but now served on the **session** bus;
  see [Session Bus Migration](#session-bus-migration-breaking-change-for-piper)).
- The `.device` file database in `data/devices/`.

Installing libratbag from system packages
-----------------------------------------

libratbag is not yet packaged for distributions. See the
[Compiling](#compiling-libratbag) section below to build from source.

Build Requirements
------------------

- **Rust toolchain** — a stable Rust compiler (Rust 1.85+; edition 2024) and Cargo.
  Install via [rustup](https://rustup.rs/) or your distribution's package
  manager.
- **Meson** (>= 0.59) and **Ninja**.
- **System libraries**: `libudev` (required for runtime udev monitoring) and
  `systemd` (only for installing the unit file; optional if you package the
  service files yourself).
- **pkg-config** — used by Meson to locate `libudev` and `systemd`.

The Rust daemon itself depends on `tokio`, `zbus`, `nix`, `udev`, `serde`,
`tracing`, and other crates — Cargo resolves these automatically. The CLI
binary target (`ratbagctl`) depends on `clap`, `zbus`, `tokio`, `anyhow`,
`thiserror`, and `serde`/`serde_json`; its colour output is written against the
standard library rather than a terminal crate.
`Cargo.lock` files are committed for reproducible builds
(`cargo build --locked`).

Compiling libratbag
-------------------

libratbag uses the [meson build system](http://mesonbuild.com) which in
turn uses Ninja to invoke the compilers. Meson drives the Rust build
automatically via Cargo. Run the following commands to clone libratbag and
build everything:

    git clone https://github.com/niltonperimneto/libratbag-rs.git
    cd libratbag-rs
    meson setup builddir --prefix=/usr
    meson compile -C builddir
    sudo meson install -C builddir

To build or re-build after code changes:

    meson compile -C builddir
    sudo meson install -C builddir

To remove/uninstall:

    sudo ninja -C builddir uninstall

Note: `builddir` is the build output directory and can be changed to any
other directory name.

### Configure-time options

To list all options:

    meson configure builddir

Notable options:

| Option | Default | Description |
|---|---|---|
| `-Dsystemd=true` | `true` | Install the systemd unit file |
| `-Dsystemd-unit-dir=PATH` | auto | Override the systemd unit directory |
| `-Ddbus-root-dir=PATH` | auto | Override the DBus configuration directory |
| `-Ddbus-group=GROUP` | (everyone) | Restrict DBus access to a UNIX group |

### Building with dev-hooks (for testing)

To enable the synthetic test device DBus methods, edit the Cargo build
flags in `meson.build` or build the Rust crate directly:

    cargo build --release --bin ratbagd --features dev-hooks

**Never enable `dev-hooks` in production builds.**

Running ratbagd as DBus-activated systemd service
-------------------------------------------------

ratbagd is intended to run as a DBus-activated systemd service. At install
time, the following files are placed on the system:

| File | Purpose |
|---|---|
| `/usr/share/dbus-1/system.d/org.freedesktop.ratbag1.conf` | DBus policy (who can own/talk to the bus name) |
| `/usr/share/dbus-1/system-services/org.freedesktop.ratbag1.service` | DBus activation (tells the bus how to start the daemon) |
| `$unitdir/ratbagd.service` | systemd unit (`Type=dbus`, `BusName=org.freedesktop.ratbag1`) |

Both the DBus activation file and the systemd unit point `Exec`/`ExecStart`
at `$sbindir/ratbagd` — the installed Rust binary.

See also the configure-time options `-Dsystemd-unit-dir` and
`-Ddbus-root-dir`. Developers are encouraged to symlink to the files in the
git repository.

### Activating the service

After installing, reload the service manager:

    sudo systemctl daemon-reload
    sudo systemctl reload dbus.service

Enable the service (for automatic DBus activation):

    sudo systemctl enable ratbagd.service

From now on, any DBus access to `org.freedesktop.ratbag1` (for example via
`busctl introspect org.freedesktop.ratbag1 /org/freedesktop/ratbag1`) will
automatically start the Rust daemon through DBus activation.

### Verifying the Rust daemon is running

    systemctl status ratbagd
    journalctl -u ratbagd -n 20   # should show "Starting ratbagd version ..."

You can also start it directly for debugging:

    sudo ratbagd                             # production
    sudo RUST_LOG=debug ratbagd              # verbose logging via tracing

Using ratbagctl
---------------

`ratbagctl` is the command-line interface for configuring devices. It talks
to the running `ratbagd` daemon over DBus.

Every command has the same shape:

    ratbagctl [SELECTORS] <group> [INDEX] <verb> [VALUE]

Supply a value to write it, leave it out to read the current one. Selectors say
*which* object to act on, and each defaults to the obvious choice, so the
everyday commands carry no indices at all:

    ratbagctl                                   # list the connected devices
    ratbagctl show                              # everything about the device
    ratbagctl dpi 1600                          # set the active resolution
    ratbagctl dpi                               # print it
    ratbagctl rate 1000                         # set the report rate
    ratbagctl led color red                     # paint LED 0 red
    ratbagctl led mode breathing                # set the lighting effect
    ratbagctl button 4 key a                    # map button 4 to the A key
    ratbagctl button 4 special wheel-up         # …or to a device action
    ratbagctl button 4 macro +ctrl c -ctrl      # …or to Ctrl+C
    ratbagctl profile 1 activate                # switch profile
    ratbagctl -d g502 -p 1 dpi 800              # be explicit when you need to

### Selectors

| Flag | Meaning | Default |
|---|---|---|
| `-d`, `--device <SPEC>` | List index, sysname, or part of the device's name (case-insensitive) | the only connected device; an error listing the candidates if there is more than one |
| `-p`, `--profile <N>` | Profile index | the **active** profile |
| `-r`, `--resolution <N>` | Resolution index | the **active** resolution |
| `-l`, `--led <N>` | LED index | `0` |
| `-b`, `--button <N>` | Button index | none — required by the `button` write verbs |

A group's leading index is the same thing as its flag, closer to the verb:
`ratbagctl led 1 color red` and `ratbagctl -l 1 led color red` are one command.

Global options: `--color <auto|always|never>` and `--json` (see
[Output](#output) below), plus `-h/--help` and `-V/--version`.

### Subcommands

| Command | Description |
|---|---|
| **General** | |
| `list` (alias `ls`) | List connected devices; also what a bare `ratbagctl` prints |
| `show` (alias `info`) | Device overview: profiles, rates, states |
| `commit` | Write staged changes to the device |
| `dpi [DPI]` | Get or set the active resolution's DPI |
| `rate [HZ]` | Get or set the active profile's report rate |
| **Profile** — `ratbagctl profile [N] …` (alias `prof`) | |
| `profile list` | List profiles; also what a bare `profile` prints |
| `profile [N] show` | Full detail: rates, resolutions, buttons, LEDs |
| `profile [N] activate` (aliases `use`, `switch`) | Make it the active profile |
| `profile [N] name [NAME]` | Get or set the profile name |
| `profile [N] enable` / `disable` | Enable or disable the profile |
| `profile [N] rate [HZ]` | Get or set the report rate |
| `profile [N] angle-snapping [on\|off]` | Get or set angle snapping |
| `profile [N] debounce [MS]` | Get or set the debounce time |
| **Resolution** — `ratbagctl resolution [N] …` (alias `res`) | |
| `resolution list` | List resolutions; also what a bare `resolution` prints |
| `resolution [N] show` | Full detail: DPI, supported steps, capabilities |
| `resolution [N] dpi [DPI]` | Get or set the DPI |
| `resolution [N] activate` | Make it the active resolution |
| `resolution [N] default` | Make it the profile's default |
| `resolution [N] enable` / `disable` | Enable or disable the slot |
| **Button** — `ratbagctl button [N] …` (alias `btn`) | |
| `button list` | List buttons and what they do |
| `button [N] show` | One button's mapping and capabilities |
| `button [N] key <KEY>` | Send a key press |
| `button [N] click <BUTTON>` | Act as another mouse button |
| `button [N] special <ACTION>` | Perform a device-handled action |
| `button [N] macro <STEP…>` | Play a key macro |
| `button [N] disable` (aliases `off`, `none`) | Do nothing when pressed |
| **LED** — `ratbagctl led [N] …` | |
| `led list` | List LEDs; also what a bare `led` prints |
| `led [N] show` | Full detail: mode, colours, brightness, colour depth |
| `led [N] mode [MODE]` | Get or set the effect |
| `led [N] color [COLOR]` | Get or set the primary colour |
| `led [N] secondary-color [COLOR]` | Secondary colour (starlight, tricolor) |
| `led [N] tertiary-color [COLOR]` | Tertiary colour (tricolor) |
| `led [N] brightness [0-255]` | Get or set the brightness |
| `led [N] duration [MS]` | Get or set the effect duration, in ms |
| **Test / Dev** | |
| `test load-device <FILE>` | Load a test device from a JSON file |
| `test reset` | Remove all test devices |

All write commands commit to hardware immediately.

### Values

Arguments are named rather than numeric. Every one of them still accepts the
raw wire number, and `--help` lists the accepted spellings, so a typo is
rejected with the valid values before anything is sent to the device.

| Argument | Accepted |
|---|---|
| `MODE` | `off`, `solid`, `cycle`, `breathing`, `wave`, `starlight`, `tricolor` |
| `COLOR` | `red`, `blue`, `cyan`, … · `#ff0000` · `ff0000` · `#f00` · `255,0,0` |
| `KEY` | `a`, `5`, `enter`, `f1`, `ctrl`, `volumeup`, … · `KEY_A` · `code:30` for a raw evdev code |
| `BUTTON` | `left`, `right`, `middle`, `back`, `forward`, or a logical button number |
| `ACTION` | `wheel-up`, `wheel-down`, `resolution-cycle-up`, `resolution-alternate`, `profile-up`, `second-mode`, `battery-level`, … |
| `STEP` | `a` (press and release), `+ctrl` (press), `-ctrl` (release), or `30:1` / `30:0` |
| `ON_OFF` | `on`, `off` (also `true`/`false`, `yes`/`no`, `1`/`0`) |
| `DPI` | `1600`, or `800x1600` on a device with the separate-xy capability |

A key name wins over a raw code, so `key 5` is the digit-5 key; write `code:5`
for evdev code 5. Macro steps may start with `-`, so put `--` first if a step
would otherwise look like an option.

### Output

Lists are shown as aligned tables, with `[active]` in green, `[dirty]` and
`[default]` in yellow, `[disabled]` dimmed, and a colour swatch beside every
LED colour. Writes print one confirmation line showing what changed:

    $ ratbagctl led mode breathing
    ✓ LED 0 mode: solid → breathing

A write that would change nothing is skipped, and says so. Colour and the
`✓`/`→` glyphs are used only when the output is a terminal that supports them:
piping or redirecting yields plain ASCII, and `NO_COLOR`, `CLICOLOR_FORCE`,
`TERM=dumb` and `--color` are all honoured.

For scripts, a read prints the bare value on its own line, and `--json` gives
structured output on the read commands:

    ratbagctl --json led show | jq .mode

Failures print the reason, its cause, and what to do next:

    $ ratbagctl led mode starlight
    error: LED 0 does not support starlight mode
      hint: this LED supports: off, solid, cycle, wave, breathing
            run `ratbagctl led show` for its full capabilities

Exit status is `0` on success, `2` for a usage error, `3` when the device or
profile could not be found, `4` when the device does not support the value, `5`
when the commit to hardware failed, and `1` otherwise.

### Changed in 2.1

The argument grammar was reworked for readability and **is not backwards
compatible**. Indices are no longer bare positionals: what used to be
`ratbagctl resolution dpi 0 0 1 800` is now `ratbagctl -r 1 dpi 800`, and
`ratbagctl button set-key 0 0 4 30` is now `ratbagctl button 4 key a`. Rename
the verbs (`profile active` → `profile activate`, `button set-key` → `button
key`, `led get` → `led show`) and drop the device and profile indices wherever
the defaults do what you want.

Two long-standing bugs were fixed at the same time. `led mode` sent the wrong
wire codes, so `breathing` and `tricolor` failed outright and `cycle` silently
selected breathing; scripts that relied on `cycle` meaning breathing need
updating. And `resolution dpi` always sent an X/Y pair, which devices without
the separate-xy capability reject — setting a DPI now works on them.


Twister (Desktop GUI)
---------------------

Twister is a modern, desktop-agnostic graphical frontend for configuring
gaming mice. It is built with Tauri 2 and Svelte 5 and is included in this
repository under `twister/`.

Twister communicates with `ratbagd` over the same `org.freedesktop.ratbag1`
DBus interface, so it works as a drop-in replacement for Piper on any Linux
desktop environment.

**Status:** Early alpha — core features (DPI, buttons, LEDs, profiles) work.

See [twister/README.md](twister/README.md) for build instructions,
screenshots, and detailed documentation.

Testing
-------

The `test/` directory contains a Python integration test suite that exercises
the full `org.freedesktop.ratbag1` DBus API against the Rust daemon built
with the `dev-hooks` feature. Tests use `pytest` and cover the Manager,
Device, Profile, Resolution, Button, and LED interfaces.

See [test/README.md](test/README.md) for prerequisites and usage.

The DBus Interface
-------------------

Full documentation of the DBus interface to interact with devices is
available here: [ratbagd DBus Interface description](https://libratbag.github.io/).

The daemon exposes the following interfaces on the session bus under
`org.freedesktop.ratbag1`:

| Interface | Object Path | Description |
|---|---|---|
| `Manager` | `/org/freedesktop/ratbag1` | Entry point; lists connected devices |
| `Device` | `/org/freedesktop/ratbag1/device/<sysname>` | Per-device (name, model, profiles list) |
| `Profile` | `.../p<N>` | Per-profile (active profile, DPI list) |
| `Resolution` | `.../p<N>/r<N>` | Per-resolution (DPI x/y, report rate) |
| `Button` | `.../p<N>/b<N>` | Per-button (action type, mapping) |
| `LED` | `.../p<N>/l<N>` | Per-LED (mode, color, brightness, effect rate) |

Architecture
------------

### High-level data flow

    +---------+
    | Twister |--+
    +---------+  |   +------+    +-----------------+
                 +-> | DBus | -> | ratbagd (Rust)  | -> /dev/hidraw*
    +---------+  |   +------+    +-----------------+
    |  Piper  |--+                      |
    +---------+               +------+------+
                              | Device Actor | (one per mouse, owns DeviceIo)
                              +------+------+
                                     |
                              +------+------+
                              |   Driver    | (HID++, Roccat, Steelseries, …)
                              +-------------+

### Internal Rust architecture

- **`src/main.rs`** — entry point; initializes tracing, loads the device
  database, spawns the udev monitor, and starts the DBus server.
- **`src/ipc/`** — zbus interface implementations for `Manager`, `Device`,
  `Profile`, `Resolution`, `Button`, and `LED`.
- **`src/engine/actor.rs`** — per-device actor task that serializes hardware I/O.
  DBus handlers send `ActorCommand` messages; the actor executes them
  against the `DeviceDriver` + `DeviceIo`.
- **`src/hal/`** — the `DeviceDriver` trait and all protocol implementations.
  `DeviceIo` wraps async hidraw I/O with feature report ioctl support.
- **`src/engine/device.rs`** — `DeviceInfo` and its children (`ProfileInfo`,
  `ResolutionInfo`, `ButtonInfo`, `LedInfo`) — the canonical device state
  shared between DBus objects and the actor via `Arc<RwLock<…>>`.
- **`src/engine/device_database.rs`** — parser for `.device` files (INI-like config).
- **`src/udev_monitor.rs`** — monitors hidraw device add/remove events and
  sends `DeviceAction` messages to the main event loop.

Adding Devices to libratbag
---------------------------

libratbag relies on a device database to match a device with its driver.
See the [data/devices/](https://github.com/niltonperimneto/libratbag-rs/tree/master/data/devices)
directory for the set of known devices. These files are usually installed
into `$prefix/$datadir` (e.g. `/usr/share/libratbag/`).

Adding a new device can be as simple as adding a new `.device` file. This is
the case for many devices with a shared protocol (e.g. Logitech's HID++).
See the
[data/devices/device.example](https://github.com/niltonperimneto/libratbag-rs/tree/master/data/devices/device.example)
file for guidance on what information must be set. Look for existing devices
from the same vendor as guidance too.

If the device has a different protocol and doesn't work after adding the
device file, you'll have to start reverse-engineering the device-specific
protocol. Good luck :)

Source
------

    git clone https://github.com/niltonperimneto/libratbag-rs.git

Bugs
----

Bugs can be reported in [our issue tracker](https://github.com/niltonperimneto/libratbag-rs/issues)

Discussions
-----------

For questions, feature requests, or general discussion, please open an
[issue](https://github.com/niltonperimneto/libratbag-rs/issues) on GitHub.

Device-specific notes
---------------------

A number of device-specific notes and observations can be found in the
upstream project wiki:
https://github.com/libratbag/libratbag/wiki/Devices

License
-------

This project uses a **dual-license** structure:

- **ratbagd** (the Rust daemon in `src/`) is licensed under the
  **GNU General Public License v3.0 (GPLv3)**.
- **ratbagctl** (the CLI tool in `src/bin/ratbagctl/`) is licensed under the
  **GNU General Public License v3.0 or later (GPL-3.0-or-later)**.
- **Twister** (the desktop GUI in `twister/`) is licensed under the
  **GNU General Public License v3.0 or later (GPL-3.0-or-later)**.
- **Supporting assets** (service templates, device data, documentation, and
  other non-daemon content) remain licensed under the **MIT/Expat** license.

> Permission is hereby granted, free of charge, to any person obtaining a
> copy of this software and associated documentation files (the "Software"),
> to deal in the Software without restriction, including without limitation
> the rights to use, copy, modify, merge, publish, distribute, sublicense,
> and/or sell copies of the Software, and to permit persons to whom the
> Software is furnished to do so, subject to the following conditions: [...]

See the [LICENSE](LICENSE) file for the MIT license and
[Cargo.toml](Cargo.toml) for the GPLv3 declaration.
