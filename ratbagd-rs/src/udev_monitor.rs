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
}

/* Run the udev monitor: enumerate existing hidraw devices, then watch
 * for hotplug events indefinitely.
 *
 * Returns `Ok(())` when the channel receiver is dropped (clean shutdown)
 * or an `Err` if a udev syscall fails.  The caller in `main.rs` joins
 * this future inside `tokio::select!` so that either outcome surfaces. */
pub async fn run(tx: mpsc::Sender<DeviceAction>, shutdown: Arc<AtomicBool>) -> Result<()> {
// ...
// The rest is identically unmodified until build_add_action:

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

    loop {
        let mut pollfd = [nix::poll::PollFd::new(
            /* Safety: `fd` was obtained from `monitor.as_raw_fd()` above.
             * `monitor` is owned by this stack frame and is not moved or
             * dropped until the function returns, so the raw fd remains
             * valid for the entire lifetime of the `BorrowedFd`.  The
             * borrow is consumed by `poll` before the next loop iteration,
             * ensuring it does not outlive `monitor`. */
            unsafe { std::os::unix::io::BorrowedFd::borrow_raw(fd) },
            nix::poll::PollFlags::POLLIN,
        )];

        match nix::poll::poll(&mut pollfd, nix::poll::PollTimeout::from(1000u16)) {
            Ok(0) => {
                /* Timeout — check if the daemon is shutting down. */
                if shutdown.load(Ordering::Relaxed) {
                    info!("Shutdown flag set, stopping udev monitor");
                    return Ok(());
                }
                continue;
            }
            Ok(_) => {}
            Err(nix::errno::Errno::EINTR) => {
                /* EINTR can be delivered by the signal that sets the  */
                /* shutdown flag, so re-check before looping.          */
                if shutdown.load(Ordering::Relaxed) {
                    info!("Shutdown flag set (EINTR), stopping udev monitor");
                    return Ok(());
                }
                continue;
            }
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
                        info!("Hotplug add: {}", action_sysname(&action));
                        if tx.blocking_send(action).is_err() {
                            info!("Channel closed, stopping udev monitor");
                            return Ok(());
                        }
                    }
                }
                udev::EventType::Remove => {
                    let sysname = event
                        .device()
                        .sysname()
                        .to_string_lossy()
                        .to_string();
                    info!("Hotplug remove: {}", sysname);
                    if tx.blocking_send(DeviceAction::Remove { sysname }).is_err() {
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
            debug!("Enumerated existing device: {}", action_sysname(&action));
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
    let sysname = device.sysname().to_string_lossy().to_string();
    let devnode = device.devnode()?.to_path_buf();

    /* Walk up to the parent HID device to find HID_ID and HID_NAME */
    let hid_parent = find_hid_parent(device)?;

    let name = hid_parent
        .property_value("HID_NAME")
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_else(|| "Unknown".to_string());

    /* Extract the physical USB port (e.g. `usb-0000:02:00.0-5/input0` -> `usb-0000:02:00.0-5`) */
    let mut parent_phys = hid_parent
        .property_value("HID_PHYS")
        .map(|v| v.to_string_lossy().to_string())
        .unwrap_or_default();
    if let Some(slash_idx) = parent_phys.rfind('/') {
        parent_phys.truncate(slash_idx);
    }
    /* Fallback to something if HID_PHYS is entirely missing */
    if parent_phys.is_empty() {
        parent_phys = sysname.clone();
    }

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

/* Walk up the device tree to find the parent with subsystem "hid". */
fn find_hid_parent(device: &udev::Device) -> Option<udev::Device> {
    let mut current = device.parent()?;
    loop {
        if let Some(subsystem) = current.subsystem() {
            if subsystem == "hid" {
                return Some(current);
            }
        }
        current = current.parent()?;
    }
}

/* Parse the `HID_ID` property (format: `BBBB:VVVV:PPPP`) into (bustype, vid, pid). */
fn parse_hid_id(device: &udev::Device) -> Option<(u16, u16, u16)> {
    let hid_id = device.property_value("HID_ID")?;
    let s = hid_id.to_string_lossy();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }

    let bustype = u16::from_str_radix(parts[0], 16).ok()?;
    let vid = u16::from_str_radix(parts[1], 16).ok()?;
    let pid = u16::from_str_radix(parts[2], 16).ok()?;

    Some((bustype, vid, pid))
}

/* Helper to extract sysname from a DeviceAction for logging. */
fn action_sysname(action: &DeviceAction) -> &str {
    match action {
        DeviceAction::Add { sysname, .. } => sysname,
        DeviceAction::Remove { sysname } => sysname,
        #[cfg(feature = "dev-hooks")]
        DeviceAction::InjectTest { sysname, .. } => sysname,
        #[cfg(feature = "dev-hooks")]
        DeviceAction::RemoveTest { sysname } => sysname,
    }
}

/* Check whether the HID report descriptor contains a vendor-defined usage
 * page (0xFF00..=0xFFFF).  These pages are used by vendor protocol
 * interfaces (Logitech HID++, SteelSeries, etc.) and distinguish them
 * from standard mouse/keyboard HID interfaces.
 *
 * The report descriptor is read from sysfs at
 * `/sys/…/hid-device/report_descriptor`.  A Usage Page item with a 2-byte
 * value is encoded as `06 lo hi` in the HID descriptor; we check whether
 * `hi >= 0xFF` which covers the entire vendor-defined range. */
fn has_vendor_usage_page(hid_device: &udev::Device) -> bool {
    let syspath = hid_device.syspath();
    let rd_path = syspath.join("report_descriptor");
    let data = match std::fs::read(&rd_path) {
        Ok(d) => d,
        Err(_) => return false,
    };

    /* Scan for Usage Page items with 2-byte values (tag 0x06).
     * Format: 0x06 <lo_byte> <hi_byte>
     * Vendor pages have hi_byte >= 0xFF, i.e. page in 0xFF00..=0xFFFF. */
    let mut i = 0;
    while i + 2 < data.len() {
        if data[i] == 0x06 && data[i + 2] >= 0xFF {
            return true;
        }
        /* Advance past this item.  Bits 0..1 of the prefix byte encode
         * the data size: 0→0, 1→1, 2→2, 3→4 bytes.  The prefix byte
         * itself is 1 byte, so total item size = 1 + data_size. */
        let size_code = data[i] & 0x03;
        let data_size = match size_code {
            0 => 0,
            1 => 1,
            2 => 2,
            3 => 4,
            _ => unreachable!(),
        };
        i += 1 + data_size;
    }
    false
}
