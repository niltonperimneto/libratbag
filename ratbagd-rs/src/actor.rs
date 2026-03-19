/* Device Actor — manages the lifecycle of a single connected device.
 *
 * Each physical device gets its own actor task (`tokio::spawn`), which
 * owns the `DeviceIo` file handle and the protocol driver instance.
 * DBus interface objects communicate with this actor through an
 * `mpsc` channel, ensuring that all hardware I/O is serialized. */

use std::fmt;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::{mpsc, oneshot, RwLock};
use tracing::{debug, info, warn};

use crate::device::DeviceInfo;
use crate::driver::{DeviceDriver, DeviceIo, DriverError};

/* Structured error type for commit failures, preserving the failure
 * category across the actor channel boundary so that DBus clients
 * and GUI frontends can display context-appropriate recovery
 * instructions rather than opaque error strings. */
#[derive(Debug)]
pub enum CommitError {
    /* The device actor task has exited (device removed or crashed). */
    ActorGone,

    /* A hardware I/O failure occurred (read/write on hidraw). */
    Io { detail: String },

    /* The device did not respond within the retry budget. */
    Timeout { attempts: u8 },

    /* Sector CRC verification failed after writing. */
    ChecksumMismatch { computed: u16, received: u16 },

    /* The device returned a protocol-level error. */
    ProtocolError { detail: String },

    /* Any other driver failure not covered above. */
    Other { detail: String },
}

impl fmt::Display for CommitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ActorGone => write!(f, "Device actor is no longer running"),
            Self::Io { detail } => write!(f, "I/O failure: {detail}"),
            Self::Timeout { attempts } => {
                write!(f, "Hardware timed out after {attempts} attempt(s)")
            }
            Self::ChecksumMismatch { computed, received } => write!(
                f,
                "Checksum mismatch: computed {computed:#06x}, received {received:#06x}"
            ),
            Self::ProtocolError { detail } => write!(f, "Protocol error: {detail}"),
            Self::Other { detail } => write!(f, "{detail}"),
        }
    }
}

impl CommitError {
    /* Convert an `anyhow::Error` (which may wrap a `DriverError`) into the
     * appropriate `CommitError` variant, preserving structured information
     * when the root cause is a known driver error. */
    fn from_anyhow(err: &anyhow::Error) -> Self {
        if let Some(de) = err.downcast_ref::<DriverError>() {
            match de {
                DriverError::Io { device, source } => Self::Io {
                    detail: format!("{device}: {source}"),
                },
                DriverError::IoctlFailed(e) => Self::Io {
                    detail: format!("ioctl: {e}"),
                },
                DriverError::Timeout { attempts } => Self::Timeout {
                    attempts: *attempts,
                },
                DriverError::ChecksumMismatch { computed, received } => Self::ChecksumMismatch {
                    computed: *computed,
                    received: *received,
                },
                DriverError::ProtocolError { sub_id, error } => Self::ProtocolError {
                    detail: format!("sub_id={sub_id:#04x}, error={error:#04x}"),
                },
                DriverError::Hidpp20Error {
                    error_name,
                    error_code,
                    feature_index,
                    function,
                } => Self::ProtocolError {
                    detail: format!(
                        "HID++ 2.0 {error_name} (0x{error_code:02X}) \
                         feature 0x{feature_index:02X} fn={function}"
                    ),
                },
                DriverError::BufferTooSmall { expected, actual } => Self::Other {
                    detail: format!("buffer too small: expected {expected}, got {actual}"),
                },
                DriverError::Hidpp20ProbeFailure { indices } => Self::Other {
                    detail: format!("probe failed (indices: {indices:02X?})"),
                },
            }
        } else {
            Self::Other {
                detail: format!("{err:#}"),
            }
        }
    }
}

/* Commands that DBus interface objects can send to the device actor. */
#[derive(Debug)]
pub enum ActorMessage {
    /* Commit all pending changes to hardware and report success/failure. */
    Commit {
        reply: oneshot::Sender<Result<(), CommitError>>,
    },
    /* Gracefully shut down the actor (e.g., on device removal). */
    Shutdown,
}

/* Handle used by DBus objects to send commands to the device actor. */
#[derive(Clone, Debug)]
pub struct ActorHandle {
    tx: mpsc::Sender<ActorMessage>,
}

impl ActorHandle {
    /* Request the actor to shut down gracefully. */
    pub async fn shutdown(&self) {
        let _ = self.tx.send(ActorMessage::Shutdown).await;
    }

    /* Request the actor to commit pending changes to hardware.
     * Returns `Ok(())` on success, or a structured `CommitError` on failure. */
    pub async fn commit(&self) -> Result<(), CommitError> {
        let (reply_tx, reply_rx) = oneshot::channel();

        self.tx
            .send(ActorMessage::Commit { reply: reply_tx })
            .await
            .map_err(|_| CommitError::ActorGone)?;

        reply_rx.await.map_err(|_| CommitError::ActorGone)?
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
                    /* Clone a snapshot of the device state and release the
                     * lock immediately.  This prevents write-starvation:
                     * if the commit takes a long time (wireless retries,
                     * EEPROM writes), concurrent DBus writers are not
                     * blocked waiting for the read-lock to be released.
                     * The ~1.6 µs clone cost is negligible compared to the
                     * multi-millisecond hardware I/O that follows. */
                    let snapshot = self.info.read().await.clone();
                    let result = self.driver.commit(&mut self.io, &snapshot).await;

                    if result.is_ok() {
                        /* Clear dirty flags under a brief write-lock. */
                        let mut info = self.info.write().await;
                        for profile in &mut info.profiles {
                            profile.is_dirty = false;
                        }
                    }

                    /* Process any unsolicited hardware events (e.g. profile
                     * switch notifications) that arrived during the commit's
                     * I/O calls.  These were buffered by DeviceIo::request()
                     * because they didn't match the pending command. */
                    let events = self.io.drain_events();
                    if !events.is_empty() {
                        let mut info = self.info.write().await;
                        for event in &events {
                            match self.driver.handle_event(event, &mut info).await {
                                Ok(true) => {
                                    debug!(
                                        "Unsolicited event updated device state: {:02x?}",
                                        event
                                    );
                                }
                                Ok(false) => { /* event was recognised but no state change */ }
                                Err(e) => {
                                    warn!("Error handling unsolicited event: {e}");
                                }
                            }
                        }
                    }

                    let response = result.map_err(|e| CommitError::from_anyhow(&e));
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

/* Spawn a device actor from an already-constructed DeviceIo (dev-hooks mock path).
 *
 * Unlike `spawn_device_actor`, this does NOT open a hidraw node or run
 * probe/load_profiles — the caller has already done those steps with
 * the mock I/O backend. It just creates the channel + actor + task. */
#[cfg(feature = "dev-hooks")]
pub fn spawn_device_actor_with_io(
    driver: Box<dyn DeviceDriver>,
    io: DeviceIo,
    info: Arc<RwLock<DeviceInfo>>,
) -> ActorHandle {
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

    ActorHandle { tx }
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
