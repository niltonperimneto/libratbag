pub mod button;
pub mod device;
pub mod led;
pub mod manager;
pub mod profile;
pub mod resolution;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::actor::{self, ActorHandle};
use crate::device::DeviceInfo;
use crate::device_database::DeviceDb;
use crate::driver;
use crate::udev_monitor::DeviceAction;

/* Translate a numeric bustype from HID_ID into the string used in `.device` files. */
fn bustype_to_string(bustype: u16) -> String {
    match bustype {
        0x03 => "usb".to_string(),
        0x05 => "bluetooth".to_string(),
        _ => format!("{:04x}", bustype),
    }
}

/* Register a new device and its children (profiles, buttons, etc) onto the DBus bus. */
/* Returns a list of all object paths that were registered. */
async fn register_device_on_dbus(
    conn: &zbus::Connection,
    sysname: &str,
    shared_info: Arc<RwLock<DeviceInfo>>,
    actor_handle: Option<ActorHandle>,
) -> Vec<String> {
    let device_path = format!(
        "/org/freedesktop/ratbag1/device/{}",
        sysname.replace('-', "_")
    );
    let mut object_paths = vec![device_path.clone()];
    let object_server = conn.object_server();

    /* Register the Device object */
    let device_obj = device::RatbagDevice::new(
        Arc::clone(&shared_info),
        device_path.clone(),
        actor_handle,
    );

    if let Err(e) = object_server.at(device_path.as_str(), device_obj).await {
        warn!("Failed to register device {}: {}", sysname, e);
        return object_paths;
    }

    /* Register Profile, Resolution, Button, LED child objects */
    let info_snapshot = shared_info.read().await;
    for prof in &info_snapshot.profiles {
        let profile_path = format!("{}/p{}", device_path, prof.index);
        let profile_obj = profile::RatbagProfile::new(prof.clone(), device_path.clone());
        if let Err(e) = object_server.at(profile_path.as_str(), profile_obj).await {
            warn!("Failed to register profile {}: {}", profile_path, e);
        }
        object_paths.push(profile_path.clone());

        for res in &prof.resolutions {
            let res_path = format!("{}/p{}/r{}", device_path, prof.index, res.index);
            let res_obj = resolution::RatbagResolution::new(res.clone());
            if let Err(e) = object_server.at(res_path.as_str(), res_obj).await {
                warn!("Failed to register resolution {}: {}", res_path, e);
            }
            object_paths.push(res_path);
        }

        for btn in &prof.buttons {
            let btn_path = format!("{}/p{}/b{}", device_path, prof.index, btn.index);
            let btn_obj = button::RatbagButton::new(btn.clone());
            if let Err(e) = object_server.at(btn_path.as_str(), btn_obj).await {
                warn!("Failed to register button {}: {}", btn_path, e);
            }
            object_paths.push(btn_path);
        }

        for led_info in &prof.leds {
            let led_path = format!("{}/p{}/l{}", device_path, prof.index, led_info.index);
            let led_obj = led::RatbagLed::new(led_info.clone());
            if let Err(e) = object_server.at(led_path.as_str(), led_obj).await {
                warn!("Failed to register led {}: {}", led_path, e);
            }
            object_paths.push(led_path);
        }
    }
    
    object_paths
}

/* Starts the DBus server and registers all interfaces. */
/*  */
/* This function blocks until the daemon is shut down. It receives device */
/* hotplug events from the udev monitor through the `device_rx` channel. */
pub async fn run_server(
    mut device_rx: mpsc::Receiver<DeviceAction>,
    device_db: DeviceDb,
) -> Result<()> {
    let manager = manager::RatbagManager::default();

    let conn = Builder::system()?
        .name("org.freedesktop.ratbag1")?
        .serve_at("/org/freedesktop/ratbag1", manager)?
        .build()
        .await?;

    info!("DBus server ready on org.freedesktop.ratbag1");

    /* Track registered device paths so we can clean up on removal */
    let mut registered_devices: HashMap<String, Vec<String>> = HashMap::new();

    /* Track actor handles so we can shut them down on removal */
    let mut actor_handles: HashMap<String, ActorHandle> = HashMap::new();

    /* Main event loop: process udev device events */
    while let Some(action) = device_rx.recv().await {
        match action {
            DeviceAction::Add {
                sysname,
                devnode,
                name,
                bustype,
                vid,
                pid,
            } => {
                let bus_str = bustype_to_string(bustype);
                let key = (bus_str.clone(), vid, pid);

                let entry = match device_db.get(&key) {
                    Some(e) => e,
                    None => {
                        info!(
                            "Ignoring unsupported device {} ({:04x}:{:04x})",
                            sysname, vid, pid
                        );
                        continue;
                    }
                };

                info!(
                    "Matched device: {} -> {} (driver: {})",
                    sysname, entry.name, entry.driver
                );

                let device_info =
                    DeviceInfo::from_entry(&sysname, &name, bustype, vid, pid, entry);
                let device_path = format!(
                    "/org/freedesktop/ratbag1/device/{}",
                    sysname.replace('-', "_")
                );

                /* Wrap DeviceInfo in Arc<RwLock> so actor and DBus share state */
                let shared_info = Arc::new(RwLock::new(device_info));

                /* Try to create and spawn the hardware driver actor */
                let actor_handle = match driver::create_driver(&entry.driver) {
                    Some(drv) => {
                        match actor::spawn_device_actor(&devnode, drv, Arc::clone(&shared_info))
                            .await
                        {
                            Ok(handle) => {
                                info!("Driver {} active for {}", entry.driver, sysname);
                                Some(handle)
                            }
                            Err(e) => {
                                warn!(
                                    "Driver {} probe failed for {}: {e:#}",
                                    entry.driver, sysname
                                );
                                None
                            }
                        }
                    }
                    None => None,
                };

                let object_paths = register_device_on_dbus(
                    &conn,
                    &sysname,
                    Arc::clone(&shared_info),
                    actor_handle.clone(),
                ).await;


                /* Update the manager's device list */
                let object_server = conn.object_server();
                let iface_ref = object_server
                    .interface::<_, manager::RatbagManager>("/org/freedesktop/ratbag1")
                    .await?;
                iface_ref.get_mut().await.add_device(device_path.clone()).await;
                iface_ref
                    .get()
                    .await
                    .devices_changed(iface_ref.signal_emitter())
                    .await?;

                if let Some(handle) = actor_handle {
                    actor_handles.insert(sysname.clone(), handle);
                }
                registered_devices.insert(sysname.clone(), object_paths);

                info!(
                    "Device {} registered at {} ({} child objects)",
                    entry.name,
                    device_path,
                    registered_devices[&sysname].len() - 1
                );
            }
            DeviceAction::Remove { sysname } => {
                /* Shut down the actor if one is running */
                if let Some(handle) = actor_handles.remove(&sysname) {
                    handle.shutdown().await;
                }

                if let Some(paths) = registered_devices.remove(&sysname) {
                    let object_server = conn.object_server();

                    /* Remove child objects first (reverse order), then the device */
                    for path in paths.iter().rev() {
                        /* Try removing each interface type — only one will succeed per path */
                        let _ = object_server
                            .remove::<device::RatbagDevice, _>(path.as_str())
                            .await;
                        let _ = object_server
                            .remove::<profile::RatbagProfile, _>(path.as_str())
                            .await;
                        let _ = object_server
                            .remove::<resolution::RatbagResolution, _>(path.as_str())
                            .await;
                        let _ = object_server
                            .remove::<button::RatbagButton, _>(path.as_str())
                            .await;
                        let _ = object_server
                            .remove::<led::RatbagLed, _>(path.as_str())
                            .await;
                    }

                    /* Update manager device list */
                    let device_path = &paths[0];
                    let iface_ref = object_server
                        .interface::<_, manager::RatbagManager>(
                            "/org/freedesktop/ratbag1",
                        )
                        .await?;
                    iface_ref
                        .get_mut()
                        .await
                        .remove_device(device_path)
                        .await;
                    iface_ref
                        .get()
                        .await
                        .devices_changed(iface_ref.signal_emitter())
                        .await?;

                    info!("Device {} removed ({} objects)", sysname, paths.len());
                } else {
                    info!("Device removed: {} (was not registered)", sysname);
                }
            }
        }
    }

    info!("udev monitor channel closed, shutting down");
    Ok(())
}

use zbus::connection::Builder;
