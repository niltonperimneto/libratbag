# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

libratbag-rs is a Rust rewrite of **ratbagd**, a DBus daemon for configuring gaming mice, plus the **ratbagctl** CLI. A single Cargo crate (`ratbagd`, edition 2024) builds both binaries: `ratbagd` (from `src/main.rs`) and `ratbagctl` (from `src/bin/ratbagctl/main.rs`). The daemon serves the `org.freedesktop.ratbag1` DBus API (version 2) on the **session bus** — it runs unprivileged, with device access granted via udev `uaccess` rules, not root. This intentionally breaks stock Piper, which expects the system bus. Do not reintroduce system-bus assumptions.

Keyboard support was deliberately removed (migrated to the companion `clackd` project — see `docs/keyboard-migration-clackd.md`). Don't add keyboard devices or keyboard-specific code here. The README references a `twister/` GUI directory that is not present in this repository.

## Build and test

Building requires the `libudev` development headers found via `pkg-config` (`apt install libudev-dev`). Without them, `libudev-sys`'s build script panics.

```sh
cargo build                                  # debug build of both binaries
cargo build --features dev-hooks             # enables LoadTestDevice/ResetTestDevice DBus methods
cargo test --features dev-hooks              # unit tests (~114, all in #[cfg(test)] modules)
cargo test hal::steelseries::tests::commit_v2_dispatches_full_report_sequence   # single test
cargo clippy --all-targets --features dev-hooks
```

Never enable `dev-hooks` in production/release builds — it exists solely so tests can inject synthetic devices.

**Do not run `cargo fmt` across the tree** — the codebase is not rustfmt-clean (it uses deliberate alignment in constants and comments), and a blanket format would produce a huge unrelated diff.

Release/packaged builds go through Meson (`meson setup builddir && meson compile -C builddir`), but Meson just shells out to `cargo build --release` and installs binaries, the device database, udev rules, the systemd **user** unit, and the DBus **session** activation file. For code changes, plain `cargo` is all you need.

### Running the daemon locally

```sh
RUST_LOG=debug RATBAGD_DATA_DIR=$PWD/data/devices cargo run --bin ratbagd --features dev-hooks
```

`RATBAGD_DATA_DIR` overrides the device database directory (default `/usr/share/libratbag`); the daemon refuses to start if the directory doesn't exist.

### Python integration tests

`test/` holds a pytest suite exercising the full DBus API against a **running** daemon built with `dev-hooks` (it injects synthetic devices via `LoadTestDevice`, so no hardware is needed). Requires `pytest` and `dbus-python`, plus a session DBus. Tests are skipped wholesale if the daemon is unreachable.

```sh
RATBAG_TEST_BUS=session pytest test/ -v
pytest test/test_manager_device.py::TestManager::test_api_version -v   # single test
```

Python code is linted with ruff (config in `pyproject.toml`, py37 target).

## Architecture

Data flow: DBus client → zbus interface object (`src/ipc/`) → per-device actor (`src/engine/actor.rs`) → protocol driver (`src/hal/`) → `/dev/hidraw*`.

- **`src/main.rs`** — entry point. `tokio::select!` multiplexes the DBus server, the udev monitor task, and Ctrl-C.
- **`src/udev_monitor.rs`** — blocking udev thread watching hidraw hotplug; sends `DeviceAction` messages over an mpsc channel to the server event loop.
- **`src/ipc/`** — zbus implementations of the six DBus interfaces (`Manager`, `Device`, `Profile`, `Resolution`, `Button`, `LED`). `ipc::mod::run_server` owns the main event loop: it handles hotplug actions, dev-hooks test-device injection, and re-probe requests for "parked" devices (wireless devices that answered a probe with `DeviceAsleep`).
- **`src/engine/actor.rs`** — one Tokio task per connected device. The actor exclusively owns the `DeviceIo` file handle and driver instance; DBus objects talk to it through an `ActorHandle` (mpsc + oneshot replies), so all hardware I/O is serialized. Messages: `Commit`, `Shutdown`.
- **`src/engine/device.rs`** — `DeviceInfo` and children (`ProfileInfo`, `ResolutionInfo`, `ButtonInfo`, `LedInfo`): the canonical device state, shared between DBus objects and the actor via `Arc<RwLock<…>>`. Also defines the `special_action` constants that mirror the C libratbag enum — drivers must translate hardware bytecodes to/from these.
- **`src/engine/device_database.rs`** — parser for the INI-style `.device` files in `data/devices/`. A device is matched to a driver by the `Driver=` key; `hal::create_driver` maps that name to a driver instance.
- **`src/engine/test_device.rs`** — JSON-spec synthetic devices for the dev-hooks path.
- **`src/hal/`** — the driver framework. `mod.rs` defines the `DeviceDriver` trait (`probe`, `load_profiles`, `commit`, optional `handle_event`/`wants_unsolicited_events`), the `DeviceIo` async hidraw wrapper (feature-report ioctls, request/response matching with timeouts, retries, and noise filtering), the structured `DriverError` enum, and the `create_driver` factory. Each protocol lives in its own module (`hidpp10`, `hidpp20`, `roccat`, `steelseries`, `asus`, …).

Adding support for a new mouse often requires **no Rust code**: add a `.device` file in `data/devices/` (see `device.example`) if the device speaks an existing protocol.

## Conventions and invariants

- **Error handling**: no `unwrap()`/`expect()`/`panic!` in daemon code paths — failures are encoded as `Result` with domain-specific error enums (see `DriverError`). Shared state goes through `Arc<RwLock<…>>`/`Arc<Mutex<…>>`; keep new dependencies to a minimum. (These rules are formalized in `.agents/rules/rust-coding.md`.) `expect` is acceptable inside `#[cfg(test)]` code.
- **Comment style**: daemon code uses C-style `/* … */` block comments that explain *rationale* (why a constant has its value, what invariant a branch preserves). Match this style and density when editing.
- **Coupled timing constants**: `PROBE_TIMEOUT` in `src/engine/actor.rs` must stay ≥ (probe indices) × (probe attempts) × `READ_TIMEOUT_PER_ATTEMPT` in `src/hal/mod.rs` — the comments at each site cross-reference the other.
- **API version sync**: `ipc::manager::API_VERSION` and `ratbagd_api_version` in `meson.build` must both stay at 2 (wire-compatible with the C daemon's DBus API).
- **Unit test patterns**: hardware-free driver tests build a `DeviceIo` over a `UnixStream` socketpair (`fake_device()` in `src/hal/mod.rs` tests) so the real `AsyncFd` code paths are exercised; timeout paths use `#[tokio::test(start_paused = true)]` (the `test-util` dev-dependency) to avoid real waits.
- **Licensing**: daemon and CLI code is GPL-3.0-or-later; supporting assets (service templates, device data, docs) are MIT. Keep new `src/` files under the GPL side.
