"""
Pytest configuration and shared fixtures for the libratbag test harness.

Requires a running ratbagd daemon built with --features dev-hooks.
Uses the system DBus to inject synthetic test devices and validate
the org.freedesktop.ratbag1 DBus API.
"""

import json
import os
import subprocess
import time

import pytest

from .ratbag_dbus import RatbagDBusClient

# ---------------------------------------------------------------------------
# Environment helpers
# ---------------------------------------------------------------------------

RATBAGD_BUS_NAME = "org.freedesktop.ratbag1"
DBUS_TYPE = os.environ.get("RATBAG_TEST_BUS", "system")


def _wait_for_daemon(client: RatbagDBusClient, timeout: float = 5.0) -> bool:
    """Block until the daemon is reachable or *timeout* seconds elapse."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            _ = client.manager_api_version()
            return True
        except Exception:
            time.sleep(0.2)
    return False


# ---------------------------------------------------------------------------
# Session-scoped fixtures
# ---------------------------------------------------------------------------


@pytest.fixture(scope="session")
def dbus_client():
    """Session-wide DBus connection to ratbagd."""
    client = RatbagDBusClient(bus_type=DBUS_TYPE)
    if not _wait_for_daemon(client):
        pytest.skip(
            "ratbagd daemon not reachable – start it with --features dev-hooks"
        )
    return client


# ---------------------------------------------------------------------------
# Function-scoped fixtures (reset between tests)
# ---------------------------------------------------------------------------


@pytest.fixture(autouse=True)
def _reset_test_device(dbus_client: RatbagDBusClient):
    """Ensure each test starts with a clean slate by resetting test devices."""
    yield
    try:
        dbus_client.reset_test_device()
    except Exception:
        pass


# ---------------------------------------------------------------------------
# Reusable JSON device specs
# ---------------------------------------------------------------------------

MINIMAL_DEVICE_JSON = json.dumps({})  # falls back to built-in defaults

SIMPLE_DEVICE_JSON = json.dumps(
    {
        "profiles": [
            {
                "is_active": True,
                "rate": 1000,
                "report_rates": [125, 250, 500, 1000],
                "resolutions": [
                    {
                        "xres": 800,
                        "yres": 800,
                        "dpi_min": 100,
                        "dpi_max": 16000,
                        "is_active": True,
                        "is_default": True,
                    },
                    {
                        "xres": 1600,
                        "yres": 1600,
                        "dpi_min": 100,
                        "dpi_max": 16000,
                        "is_active": False,
                        "is_default": False,
                    },
                ],
                "buttons": [
                    {"action_type": "button", "button": 0x110},
                    {"action_type": "button", "button": 0x111},
                    {"action_type": "button", "button": 0x112},
                ],
                "leds": [
                    {
                        "mode": 1,
                        "color": [255, 0, 0],
                        "brightness": 200,
                        "duration": 1000,
                    }
                ],
            }
        ]
    }
)

MULTI_PROFILE_DEVICE_JSON = json.dumps(
    {
        "profiles": [
            {
                "is_active": True,
                "rate": 1000,
                "resolutions": [
                    {"xres": 800, "yres": 800, "is_active": True, "is_default": True}
                ],
                "buttons": [{"action_type": "button", "button": 0x110}],
                "leds": [],
            },
            {
                "is_active": False,
                "rate": 500,
                "resolutions": [
                    {
                        "xres": 1600,
                        "yres": 1600,
                        "is_active": True,
                        "is_default": True,
                    }
                ],
                "buttons": [{"action_type": "button", "button": 0x110}],
                "leds": [{"mode": 1, "color": [0, 255, 0], "brightness": 100}],
            },
            {
                "is_active": False,
                "is_disabled": True,
                "rate": 250,
                "resolutions": [
                    {
                        "xres": 3200,
                        "yres": 3200,
                        "is_active": True,
                        "is_default": True,
                    }
                ],
                "buttons": [{"action_type": "none"}],
                "leds": [],
            },
        ]
    }
)

SEPARATE_DPI_DEVICE_JSON = json.dumps(
    {
        "profiles": [
            {
                "is_active": True,
                "rate": 1000,
                "resolutions": [
                    {
                        "xres": 800,
                        "yres": 1600,
                        "is_active": True,
                        "is_default": True,
                    }
                ],
                "buttons": [{"action_type": "button", "button": 0x110}],
                "leds": [],
            }
        ]
    }
)
