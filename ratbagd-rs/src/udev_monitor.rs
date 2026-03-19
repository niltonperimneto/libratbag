/* udev hotplug monitor: enumerates existing hidraw devices and dispatches
 * add/remove (and dev-hook test inject/remove) actions to the main DBus
 * loop from a blocking thread.
 *
 * The `udev` crate types contain raw pointers and are not `Send`, so all
 * udev operations run synchronously inside `spawn_blocking`.  The blocking
 * thread cooperates with the async runtime by treating a closed `mpsc`
 * channel as the shutdown signal — when the DBus server drops its receiver
 * the monitor exits cleanly without requiring an extra cancellation
 * primitive. */
use std::os::unix::io::AsRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::mpsc;
use tracing::{debug, info};

/* Actions dispatched from the udev monitor to the DBus server. */
#[derive(Debug)]
pub enum DeviceAction {
    Add {
        sysname: String,
        devnode: std::path::PathBuf,
        name: String,
        bustype: u16,
        vid: u16,
        pid: u16,
        /* Physical parent connecting path (e.g. `usb-0000:02:00.0-5`).
         * Used to deduplicate multiple hidraw nodes from the same physical mouse. */
        parent_phys: String,
        /* `true` when the HID report descriptor contains at least one
         * vendor-defined usage page (0xFF00..=0xFFFF).  Vendor protocol
         * interfaces (HID++, SteelSeries, etc.) use these pages, while
         * standard mouse/keyboard interfaces use Generic Desktop (0x01).
         * The DBus layer uses this hint to skip probing interfaces that
         * cannot carry the vendor protocol. */
        has_vendor_usage: bool,
    },
    Remove {
        sysname: String,
    },
    /* Result of a background probe task.  Sent back to the event loop
     * by the `tokio::spawn`-ed probe future so that D-Bus registration
     * happens on the main loop without blocking it during the slow
     * probe + load_profiles I/O. */
    ProbeComplete {
        sysname: String,
        device_path: String,
        entry_name: String,
        phys_key: (crate::device_database::BusType, u16, u16, String),
        shared_info: std::sync::Arc<tokio::sync::RwLock<crate::device::DeviceInfo>>,
        actor_handle: crate::actor::ActorHandle,
    },
    /* Inject a synthetic test device directly into the DBus layer.
     * Only constructed when the `dev-hooks` feature is enabled. */
    #[cfg(feature = "dev-hooks")]
    InjectTest {
        sysname: String,
        device_info: crate::device::DeviceInfo,
    },
    /* Remove a previously-injected test device.
     * Only constructed when the `dev-hooks` feature is enabled. */
    #[cfg(feature = "dev-hooks")]
    RemoveTest {
        sysname: String,
    },
    /* Inject a synthetic test device that runs through the real driver's
     * probe/load_profiles/commit path using a mock I/O backend.
     * Only constructed when the `dev-hooks` feature is enabled. */
    #[cfg(feature = "dev-hooks")]
    InjectTestWithDriver {
        sysname: String,
        driver_name: String,
        device_info: crate::device::DeviceInfo,
        io_script_json: String,
    },
}

impl DeviceAction {
    /* Extract sysname from any variant for logging. */
    fn sysname(&self) -> &str {
        match self {
            Self::Add { sysname, .. }
            | Self::Remove { sysname }
            | Self::ProbeComplete { sysname, .. } => sysname,
            #[cfg(feature = "dev-hooks")]
            Self::InjectTest { sysname, .. }
            | Self::RemoveTest { sysname }
            | Self::InjectTestWithDriver { sysname, .. } => sysname,
        }
    }
}

/* Run the udev monitor: enumerate existing hidraw devices, then watch
 * for hotplug events indefinitely.
 *
 * Returns `Ok(())` when the channel receiver is dropped (clean shutdown)
 * or an `Err` if a udev syscall fails.  The caller in `main.rs` joins
 * this future inside `tokio::select!` so that either outcome surfaces. */
pub async fn run(tx: mpsc::Sender<DeviceAction>, shutdown: Arc<AtomicBool>) -> Result<()> {
    info!("udev monitor started, watching for hidraw devices");

    let result = tokio::task::spawn_blocking(move || run_blocking(tx, shutdown)).await;

    match result {
        Ok(Ok(())) => {
            info!("udev monitor shutting down normally");
            Ok(())
        }
        Ok(Err(e)) => Err(e),
        Err(join_err) => Err(anyhow::anyhow!("udev monitor task panicked: {join_err}")),
    }
}

/* Synchronous udev monitor implementation that runs inside a blocking
 * thread.  Returns `Ok(())` when the channel is closed (receiver dropped)
 * or `Err` on a udev/poll failure. */
#[allow(clippy::needless_pass_by_value)] /* Owned values required: moved into spawn_blocking closure. */
fn run_blocking(tx: mpsc::Sender<DeviceAction>, shutdown: Arc<AtomicBool>) -> Result<()> {
    /* Enumerate existing devices first. */
    enumerate_existing(&tx)?;

    /* Set up the hotplug monitor. */
    let monitor = udev::MonitorBuilder::new()
        .context("MonitorBuilder::new")?
        .match_subsystem("hidraw")
        .context("match_subsystem(hidraw)")?
        .listen()
        .context("MonitorSocket::listen")?;

    info!("udev hotplug monitor listening on hidraw subsystem");

    /* Use poll(2) to wait for events on the udev monitor fd.  The
     * one-second timeout lets us re-enter the loop and detect a closed
     * channel without requiring an extra cancellation primitive. */
    let fd = monitor.as_raw_fd();

    /* Safety: `fd` was obtained from `monitor.as_raw_fd()` above.
     * `monitor` is owned by this stack frame and is not moved or
     * dropped until the function returns, so the raw fd remains
     * valid for the entire lifetime of the `BorrowedFd`. */
    let borrowed_fd = unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) };

    /* Helper: send a DeviceAction, returning false if the channel is closed. */
    let send = |action: DeviceAction| -> bool {
        tx.blocking_send(action).is_ok()
    };

    loop {
        let mut pollfd = [nix::poll::PollFd::new(
            borrowed_fd,
            nix::poll::PollFlags::POLLIN,
        )];

        match nix::poll::poll(&mut pollfd, nix::poll::PollTimeout::from(1000u16)) {
            /* Timeout or EINTR — check the shutdown flag before looping.
             * EINTR can be delivered by the signal that sets the flag. */
            Ok(0) | Err(nix::errno::Errno::EINTR) => {
                if shutdown.load(Ordering::Relaxed) {
                    info!("Shutdown flag set, stopping udev monitor");
                    return Ok(());
                }
                continue;
            }
            Ok(_) => {}
            Err(e) => return Err(e).context("poll(2) on udev monitor fd"),
        }

        /* `MonitorSocket::iter()` calls `receive_device()` on each
         * `next()`.  When poll(2) signals POLLIN, at least one event is
         * ready; the iterator will yield it and any further events that
         * the kernel has already queued.  Events arriving between the
         * last `next()` and the subsequent `poll` are picked up in the
         * next iteration. */
        for event in monitor.iter() {
            match event.event_type() {
                udev::EventType::Add => {
                    if let Some(action) = build_add_action(&event.device()) {
                        info!("Hotplug add: {}", action.sysname());
                        if !send(action) {
                            info!("Channel closed, stopping udev monitor");
                            return Ok(());
                        }
                    }
                }
                udev::EventType::Remove => {
                    let sysname = event.device().sysname().to_string_lossy().into_owned();
                    info!("Hotplug remove: {sysname}");
                    if !send(DeviceAction::Remove { sysname }) {
                        info!("Channel closed, stopping udev monitor");
                        return Ok(());
                    }
                }
                _ => { /* Ignore bind/unbind/change events */ }
            }
        }
    }
}

/* Enumerate all currently-connected hidraw devices and send `Add` actions.
 * Returns `Ok(())` on success, including the case where the channel is
 * already closed (the caller will detect that in the poll loop). */
fn enumerate_existing(tx: &mpsc::Sender<DeviceAction>) -> Result<()> {
    let mut enumerator =
        udev::Enumerator::new().context("udev Enumerator::new")?;
    enumerator
        .match_subsystem("hidraw")
        .context("enumerator match_subsystem(hidraw)")?;

    let devices = enumerator
        .scan_devices()
        .context("enumerator scan_devices")?;

    for device in devices {
        if let Some(action) = build_add_action(&device) {
            debug!("Enumerated existing device: {}", action.sysname());
            if tx.blocking_send(action).is_err() {
                /* Receiver dropped before enumeration finished — the
                 * daemon is shutting down.  Return Ok(()) and let the
                 * caller discover the closed channel in the poll loop. */
                break;
            }
        }
    }

    Ok(())
}

/* Build a `DeviceAction::Add` from a udev device, extracting HID properties. */
fn build_add_action(device: &udev::Device) -> Option<DeviceAction> {
    let sysname = device.sysname().to_string_lossy().into_owned();
    let devnode = device.devnode()?.to_path_buf();

    /* Walk up to the parent HID device to find HID_ID and HID_NAME. */
    let hid_parent = find_hid_parent(device)?;

    let name = match hid_parent.property_value("HID_NAME") {
        Some(v) => v.to_string_lossy().into_owned(),
        None => "Unknown".to_owned(),
    };

    /* Extract the physical USB port path, truncating at the final `/`
     * (e.g. `usb-0000:02:00.0-5/input0` → `usb-0000:02:00.0-5`).
     * Falls back to sysname if HID_PHYS is absent or empty. */
    let parent_phys = hid_parent
        .property_value("HID_PHYS")
        .map(|v| v.to_string_lossy())
        .and_then(|phys| {
            let s = phys.rsplit_once('/').map_or(&*phys, |(prefix, _)| prefix);
            if s.is_empty() { None } else { Some(s.to_owned()) }
        })
        .unwrap_or_else(|| sysname.clone());

    let (bustype, vid, pid) = parse_hid_id(&hid_parent)?;
    let has_vendor_usage = has_vendor_usage_page(&hid_parent);

    Some(DeviceAction::Add {
        sysname,
        devnode,
        name,
        bustype,
        vid,
        pid,
        parent_phys,
        has_vendor_usage,
    })
}

/* Walk up the device tree to find the parent with subsystem "hid",
 * delegating to libudev's native `udev_device_get_parent_with_subsystem_devtype`. */
fn find_hid_parent(device: &udev::Device) -> Option<udev::Device> {
    device.parent_with_subsystem("hid").ok().flatten()
}

/* Parse the `HID_ID` property (format: `BBBB:VVVV:PPPP`) into (bustype, vid, pid).
 * Uses zero-allocation iterator destructuring instead of collecting into a Vec. */
fn parse_hid_id(device: &udev::Device) -> Option<(u16, u16, u16)> {
    let hid_id = device.property_value("HID_ID")?;
    let s = hid_id.to_string_lossy();
    let mut parts = s.splitn(4, ':');
    let bustype = u16::from_str_radix(parts.next()?, 16).ok()?;
    let vid = u16::from_str_radix(parts.next()?, 16).ok()?;
    let pid = u16::from_str_radix(parts.next()?, 16).ok()?;
    /* Reject malformed IDs with more than three fields. */
    if parts.next().is_some() {
        return None;
    }
    Some((bustype, vid, pid))
}

/* Check whether the HID report descriptor contains a vendor-defined usage
 * page (0xFF00..=0xFFFF).  These pages are used by vendor protocol
 * interfaces (Logitech HID++, SteelSeries, etc.) and distinguish them
 * from standard mouse/keyboard HID interfaces.
 *
 * The report descriptor is read from sysfs at
 * `/sys/…/hid-device/report_descriptor`.  A Usage Page item with a 2-byte
 * value is encoded as `06 lo hi` in the HID descriptor; we check whether
 * `hi >= 0xFF` which covers the entire vendor-defined range.
 *
 * The parser handles both short items (HID spec 6.2.2.2) and long items
 * (6.2.2.3, prefix 0xFE) to maintain correct cursor alignment even if
 * a descriptor contains reserved long-item encodings. */
fn has_vendor_usage_page(hid_device: &udev::Device) -> bool {
    /* Short-item data sizes indexed by the 2-bit size code (prefix & 0x03).
     * Code 3 encodes 4 bytes, not 3 (HID spec 6.2.2.2). */
    const SHORT_ITEM_DATA_SIZE: [usize; 4] = [0, 1, 2, 4];

    let syspath = hid_device.syspath();
    let rd_path = syspath.join("report_descriptor");
    let Ok(data) = std::fs::read(&rd_path) else {
        return false;
    };

    let mut i = 0;
    while i < data.len() {
        let prefix = data[i];

        /* Long items (HID spec 6.2.2.3): prefix byte is 0xFE.
         * Format: 0xFE <data_size:u8> <long_item_tag:u8> <data…>
         * Total length = 3 + data_size.  No standard usage pages
         * appear in long items, so we simply skip them. */
        if prefix == 0xFE {
            if i + 1 >= data.len() {
                break;
            }
            let long_data_size = data[i + 1] as usize;
            i += 3 + long_data_size;
            continue;
        }

        /* Short item: check for 2-byte Usage Page (tag 0x06)
         * with vendor-defined high byte (>= 0xFF). */
        if prefix == 0x06 && i + 2 < data.len() && data[i + 2] == 0xFF {
            return true;
        }

        /* Advance past this short item. */
        let data_size = SHORT_ITEM_DATA_SIZE[(prefix & 0x03) as usize];
        i += 1 + data_size;
    }
    false
}
