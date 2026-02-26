/* Device Actor — manages the lifecycle of a single connected device.
 *
 * Each physical device gets its own actor task (`tokio::spawn`), which
 * owns the `DeviceIo` file handle and the protocol driver instance.
 * DBus interface objects communicate with this actor through an
 * `mpsc` channel, ensuring that all hardware I/O is serialized. */

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info};

use crate::device::DeviceInfo;
use crate::driver::{DeviceDriver, DeviceIo};

/* Commands that DBus interface objects can send to the device actor. */
#[derive(Debug)]
pub enum ActorMessage {
    /* Commit all pending changes to hardware and report success/failure. */
    Commit {
        reply: oneshot::Sender<Result<(), String>>,
    },
    /* Gracefully shut down the actor (e.g., on device removal). */
    Shutdown,
}

/* Handle used by DBus objects to send commands to the device actor. */
#[derive(Clone)]
pub struct ActorHandle {
    tx: mpsc::Sender<ActorMessage>,
}

impl ActorHandle {
    /* Request the actor to shut down gracefully. */
    pub async fn shutdown(&self) {
        let _ = self.tx.send(ActorMessage::Shutdown).await;
    }

    /* Request the actor to commit pending changes to hardware.
     * Returns `Ok(())` on success, or an error string on failure. */
    pub async fn commit(&self) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(ActorMessage::Commit { reply: reply_tx })
            .await
            .map_err(|_| "Device actor is no longer running".to_string())?;

        reply_rx
            .await
            .map_err(|_| "Device actor dropped the reply channel".to_string())?
    }
}

/* The device actor itself. Owns the I/O handle and driver instance. */
struct DeviceActor {
    driver: Box<dyn DeviceDriver>,
    io: DeviceIo,
    info: Arc<RwLock<DeviceInfo>>,
    rx: mpsc::Receiver<ActorMessage>,
}

impl DeviceActor {
    /* Main actor loop: process messages until shutdown or channel close. */
    async fn run(mut self) {
        info!(
            "Device actor started for {} (driver: {})",
            self.info.read().await.sysname,
            self.driver.name()
        );

        while let Some(msg) = self.rx.recv().await {
            match msg {
                ActorMessage::Commit { reply } => {
                    let info = self.info.read().await;
                    let result = self.driver.commit(&mut self.io, &info).await;
                    let response = result.map_err(|e| format!("{e:#}"));
                    let _ = reply.send(response);
                }
                ActorMessage::Shutdown => {
                    info!(
                        "Device actor shutting down for {}",
                        self.info.read().await.sysname
                    );
                    break;
                }
            }
        }

        debug!("Device actor loop exited");
    }
}

/* Spawn a device actor for the given hardware device.
 *
 * This function:
 * 1. Opens the `/dev/hidraw` device node.
 * 2. Probes the device with the protocol driver.
 * 3. Reads the full device state (profiles, DPIs, LEDs).
 * 4. Spawns the actor task and returns a handle for DBus objects.
 *
 * Returns `None` if probing or profile loading fails (unsupported device). */
pub async fn spawn_device_actor(
    devnode: &Path,
    mut driver: Box<dyn DeviceDriver>,
    info: Arc<RwLock<DeviceInfo>>,
) -> Result<ActorHandle> {
    let mut io = DeviceIo::open(devnode)
        .await
        .with_context(|| format!("Opening {}", devnode.display()))?;

    /* Probe: confirm the device speaks this protocol */
    driver
        .probe(&mut io)
        .await
        .with_context(|| format!("Probing {} with {}", devnode.display(), driver.name()))?;

    /* Load the full device state from hardware */
    {
        let mut device_info = info.write().await;
        driver
            .load_profiles(&mut io, &mut device_info)
            .await
            .with_context(|| {
                format!(
                    "Loading profiles from {} with {}",
                    devnode.display(),
                    driver.name()
                )
            })?;
    }

    /* Create the message channel and spawn the actor */
    let (tx, rx) = mpsc::channel(16);

    let actor = DeviceActor {
        driver,
        io,
        info,
        rx,
    };

    tokio::spawn(async move {
        actor.run().await;
    });

    Ok(ActorHandle { tx })
}
