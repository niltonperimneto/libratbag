# Keyboard support migration to clackd

ratbagd focuses on configurable pointing devices (mice, trackballs, mousepads
with configurable features). Keyboard support has been removed from this tree
and is intended to be re-implemented in **clackd**, the companion daemon for
keyboard-class devices (see `SESSION_DAEMON_PLAN.md`, which already names
clackd as the architectural model for session-scoped hardware daemons).

This document records *everything* keyboard-related that existed in
libratbag-rs at the time of removal, so that clackd can reconstruct the
support without archaeology through git history. Nothing keyboard-specific
existed in Rust driver code — keyboard support was **data-only**: seven
`.device` database entries, one enum-mapping line, and a handful of doc
comments. All seven keyboards used the generic HID++ 2.0 driver
(`src/hal/hidpp20.rs`) through runtime feature discovery, exactly like the
Logitech mice.

## 1. Device inventory

All entries were Logitech HID++ 2.0 devices (`Driver=hidpp20`):

| Device               | Match(es)                          | DeviceIndex | File (removed)                             |
|----------------------|------------------------------------|-------------|--------------------------------------------|
| Logitech G PRO Keyboard | `usb:046d:c339`                 | default     | `data/devices/logitech-g-pro-keyboard.device` |
| Logitech G413        | `usb:046d:c33a`                    | default     | `data/devices/logitech-g413.device`        |
| Logitech G513        | `usb:046d:c33c`                    | default     | `data/devices/logitech-g513.device`        |
| Logitech G815        | `usb:046d:c33f`                    | default     | `data/devices/logitech-g815.device`        |
| Logitech G910        | `usb:046d:c335`                    | `ff`        | `data/devices/logitech-g910.device`        |
| Logitech G915        | `usb:046d:c33e`                    | `1`         | `data/devices/logitech-g915.device`        |
| Logitech G915 TKL    | `usb:046d:c343`, `bluetooth:046d:b35f` | `1`     | `data/devices/logitech-g915-tkl.device`    |

### Verbatim removed `.device` files

`data/devices/logitech-g-pro-keyboard.device`:

```ini
[Device]
DeviceMatch=usb:046d:c339
DeviceType=keyboard
Driver=hidpp20
Name=Logitech G PRO Keyboard
```

`data/devices/logitech-g413.device`:

```ini
[Device]
DeviceMatch=usb:046d:c33a
DeviceType=keyboard
Driver=hidpp20
Name=Logitech G413
```

`data/devices/logitech-g513.device`:

```ini
[Device]
Name=Logitech G513
DeviceMatch=usb:046d:c33c
Driver=hidpp20
DeviceType=keyboard
```

`data/devices/logitech-g815.device`:

```ini
[Device]
Name=Logitech G815
DeviceMatch=usb:046d:c33f
Driver=hidpp20
DeviceType=keyboard
```

`data/devices/logitech-g910.device`:

```ini
[Device]
Name=Logitech G910
DeviceMatch=usb:046d:c335
Driver=hidpp20
DeviceType=keyboard

[Driver/hidpp20]
DeviceIndex=ff
```

`data/devices/logitech-g915.device`:

```ini
[Device]
Name=Logitech G915
DeviceMatch=usb:046d:c33e
Driver=hidpp20
DeviceType=keyboard

[Driver/hidpp20]
DeviceIndex=1
```

`data/devices/logitech-g915-tkl.device`:

```ini
[Device]
Name=Logitech G915 TKL
DeviceMatch=usb:046d:c343;bluetooth:046d:b35f
DeviceType=keyboard
Driver=hidpp20

[Driver/hidpp20]
DeviceIndex=1
```

## 2. How keyboard support worked

There was **no keyboard-specific driver code**. All seven keyboards were
served by the generic HID++ 2.0 driver (`src/hal/hidpp20.rs`), which is
shared with 60+ Logitech mice and contains no model or device-type branches.
Everything was discovered at runtime via the HID++ 2.0 feature protocol:

- **Feature 0x8100 (Onboard Profiles)** — profile enumeration, the profile
  directory, and EEPROM sector read/write for persisting settings. The
  profile-directory sector header carries `profile_format_id`,
  `macro_format_id` and `mechanical_layout` fields; ratbagd parses but does
  not use them (`src/hal/hidpp20.rs`, `parse_profile_directory` area, noted
  as unused around lines 598-600). `mechanical_layout` in particular is
  keyboard-relevant (it describes the physical key layout) and will matter
  to clackd; it has deliberately been **left in place** here because the
  parsing code is shared with mice.
- **Feature 0x1b04 (Special Keys / Buttons)** — reprogrammable-control
  enumeration, used by `resolve_button_count` (`src/hal/hidpp20.rs`,
  ~lines 514-534) to count configurable controls. On the G-series keyboards
  this is what exposed the G-keys.
- **DeviceIndex** — the HID++ addressing byte. `ff` (G910) addresses the
  device directly on its own hidraw node; `1` (G915/G915 TKL) addresses the
  first paired device behind a LIGHTSPEED receiver. Configured per device
  via the `[Driver/hidpp20] DeviceIndex=` key, parsed by
  `src/engine/device_database.rs`.

Because the driver is feature-driven, a clackd implementation needs:
a hidraw transport with HID feature-report ioctls (see `DeviceIo` in
`src/hal/mod.rs`), the HID++ 2.0 short/long report framing
(`src/hal/hidpp.rs`), feature-table discovery, and handlers for 0x8100 and
0x1b04. The G-keys and per-key RGB matrix features (0x8010 gkeys, 0x8071
RGB effects, etc.) were *never* implemented in ratbagd — only the generic
profile/control features above were exercised on keyboards.

## 3. Device discovery / matching pipeline (reference for clackd)

The pipeline is entirely device-type-agnostic; keyboards were filtered in
or out purely by the presence of a `.device` file:

1. **udev**: `data/60-ratbagd.rules` tags any `hidraw` node whose USB
   interface class is HID (`bInterfaceClass=="03"`). The daemon-side monitor
   (`src/udev_monitor.rs`) watches `SUBSYSTEM=hidraw` add/remove events.
2. **Database**: `src/engine/device_database.rs::load_device_database` parses
   all `*.device` INI files from `$RATBAGD_DATA_DIR` (default
   `/usr/share/libratbag`). Match key is the `bus:vid:pid` triplet from
   `DeviceMatch=` (semicolon-separated list for multi-transport devices such
   as the G915 TKL). Devices with no database entry are logged as
   "Ignoring unsupported device" (`src/ipc/mod.rs`) — this is how ordinary
   keyboards are skipped, and how the seven removed entries are now skipped
   too.
3. **Driver dispatch**: `src/hal/mod.rs::create_driver` maps the `Driver=`
   string to a driver instance; `hidpp20` is the only driver keyboards ever
   used.
4. **DBus**: each registered device exposes
   `org.freedesktop.ratbag1.Device` with a `DeviceType` property.

## 4. What was removed

Keyboard removal (see the "Remove keyboard support" commit):

- The seven `data/devices/*.device` files listed in §1.
- `src/engine/device.rs`: the `"keyboard" => 3` arm of the DeviceType
  string→u32 mapping. `DeviceType=keyboard` in a (stale, local) `.device`
  file now maps to `0` (unspecified).
- Doc comments listing `3=keyboard` in `src/engine/device.rs` and
  `src/ipc/device.rs`, and the `'keyboard'` mention in
  `data/devices/device.example`.

**DBus wire-format note:** the `DeviceType` property value `3` (keyboard) is
**not reserved**. The daemon no longer emits it, and future changes may
reuse the value. Clients (Piper, etc.) should treat `3` from this daemon as
never occurring; clackd defines its own interface and is not bound by this
enum.

## 5. Re-adding a keyboard (if ever needed here)

Not recommended — use clackd. But for completeness: drop a `.device` file
with `DeviceType=keyboard` (or `other`) into the data directory and restore
the `"keyboard" => 3` mapping in `src/engine/device.rs` if the DBus type
distinction matters. The rest of the pipeline never branches on device type,
so no other change is required.

## 6. Known issues documented here, deliberately not fixed

These were found during the cleanup audit and intentionally left as-is:

- **`sinowealth_nubwo` driver is unreachable at runtime**:
  `data/devices/nubwo-x7-spectrum.device` says `Driver=sinowealth_nubwo`
  (underscore) but `src/hal/mod.rs::create_driver` matches
  `"sinowealth-nubwo"` (hyphen), so the lookup falls through to
  "Unknown driver" and the device never probes. The driver file
  `src/hal/sinowealth_nubwo.rs` (including its unused items and compiler
  warnings) was left completely untouched by this cleanup so the eventual
  fix can be made and reviewed in isolation.
- **Stale Python test helpers**: `test/ratbag_dbus.py` calls DBus methods
  `LoadTestDeviceWithDriver` and `GetMockIoLog`, which do not exist in the
  Rust daemon (`src/ipc/manager.rs` implements only `LoadTestDevice` and
  `ResetTestDevice`). Those helpers fail against this daemon.

## 7. Dead-code cleanup (same branch)

Alongside the keyboard removal, this branch removed unused code that had
accumulated during the C→Rust port — none of it keyboard-related and none
of it reachable at runtime: the unused `src/error.rs` module, never-called
HID++ 1.0 receiver/battery/pairing/LED helpers in `src/hal/hidpp10.rs`,
helper scaffolding in the unimplemented stub drivers (gskill, etekcity,
marsgaming), assorted unused constants/enum variants, stale
`#[allow(dead_code)]` attributes on live code, and the unused
`tokio-stream` dependency. The exhaustive item-by-item list is in the
"Remove dead code" commit message. Wire-layout struct fields that mirror
device report formats were kept even where unread, to preserve the
documented on-wire layouts. `src/hal/sinowealth_nubwo.rs` was excluded
entirely (see §6).
