# libratbag Python Test Harness

Integration test suite for the `org.freedesktop.ratbag1` DBus API, targeting
the Rust daemon (`ratbagd-rs`) built with the `dev-hooks` feature.

## Prerequisites

- **Python 3.7+**
- **dbus-python** (`pip install dbus-python` or system package `python3-dbus`)
- **pytest** (`pip install pytest`)
- **ratbagd** built with dev-hooks:
  ```sh
  cd ratbagd-rs && cargo build --features dev-hooks
  ```

## Quick Start

1. **Start the daemon** (in a separate terminal, as root or with DBus policy):
   ```sh
   sudo RUST_LOG=debug ./ratbagd-rs/target/debug/ratbagd
   ```

2. **Run all tests**:
   ```sh
   pytest test/ -v
   ```

3. **Run a specific test file**:
   ```sh
   pytest test/test_manager_device.py -v
   pytest test/test_profile_resolution.py -v
   pytest test/test_button_led.py -v
   pytest test/test_integration.py -v
   ```

4. **Run a specific test class or method**:
   ```sh
   pytest test/test_manager_device.py::TestManager::test_api_version -v
   pytest test/test_button_led.py::TestLed -v
   ```

## Using a session bus (development)

If you run `ratbagd` on the session bus during development, set:

```sh
RATBAG_TEST_BUS=session pytest test/ -v
```

## Architecture

```
test/
├── __init__.py               # Package marker
├── conftest.py               # Pytest fixtures, JSON device specs, session setup
├── ratbag_dbus.py            # DBus client helper (wraps dbus-python)
├── test_manager_device.py    # Manager + Device interface tests
├── test_profile_resolution.py # Profile + Resolution interface tests
├── test_button_led.py        # Button + LED interface tests
└── test_integration.py       # Cross-object mutation & round-trip tests
```

### DBus Client (`ratbag_dbus.py`)

Wraps all `org.freedesktop.ratbag1.*` DBus property reads/writes and method
calls behind a clean Python API. Tests never deal with raw DBus plumbing.

### Test Device Specs (`conftest.py`)

Pre-built JSON strings matching the `ratbagd-json.c`-compatible format:

| Constant                    | Description                                      |
|-----------------------------|--------------------------------------------------|
| `MINIMAL_DEVICE_JSON`       | Empty `{}` — daemon fills in sane defaults       |
| `SIMPLE_DEVICE_JSON`        | 1 profile, 2 resolutions, 3 buttons, 1 LED      |
| `MULTI_PROFILE_DEVICE_JSON` | 3 profiles (active, inactive, disabled)          |
| `SEPARATE_DPI_DEVICE_JSON`  | Separate X/Y DPI (800×1600)                      |

### Fixtures

- **`dbus_client`** (session-scoped) — single `RatbagDBusClient` instance,
  skips all tests if the daemon is unreachable.
- **`_reset_test_device`** (autouse, function-scoped) — calls
  `ResetTestDevice` after each test to ensure isolation.

## Test Coverage

| Interface   | Properties tested                                                   | Methods tested        |
|-------------|---------------------------------------------------------------------|-----------------------|
| Manager     | APIVersion, Devices                                                 | LoadTestDevice, Reset |
| Device      | Name, Model, FirmwareVersion, Profiles                              | Commit                |
| Profile     | Index, Name, IsActive, Disabled, IsDirty, ReportRate, ReportRates,  | SetActive             |
|             | AngleSnapping, Debounce, Resolutions, Buttons, Leds                 |                       |
| Resolution  | Index, Resolution, IsActive, IsDefault, IsDisabled, Capabilities,   | SetActive, SetDefault |
|             | Resolutions (DPI list)                                              |                       |
| Button      | Index, Mapping, ActionTypes                                         | (via set_mapping)     |
| LED         | Index, Mode, Modes, Color, SecondaryColor, TertiaryColor,          | (via set_* props)     |
|             | ColorDepth, Brightness, EffectDuration                              |                       |
