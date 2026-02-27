pub mod asus;
pub mod hidpp;
pub mod hidpp10;
pub mod hidpp20;

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{debug, warn};

use crate::device::DeviceInfo;

/* Maximum HID++ report length (long report = 20 bytes) */
const MAX_REPORT_LEN: usize = 20;

/* Timeout per individual read attempt */
const READ_TIMEOUT: Duration = Duration::from_millis(500);

/* Maximum number of reads to attempt per single request retry */
const MAX_READS_PER_ATTEMPT: usize = 10;

/* Async wrapper around a `/dev/hidraw` file descriptor. */
/*  */
/* All hardware I/O goes through this struct so that drivers never */
/* touch raw file handles directly. */
pub struct DeviceIo {
    file: tokio::fs::File,
    path: std::path::PathBuf,
}

impl DeviceIo {
    /* Open the hidraw device node at `path`. */
    pub async fn open(path: &Path) -> Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .await
            .with_context(|| format!("Failed to open hidraw device {}", path.display()))?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /* Write a raw HID report to the device. */
    pub async fn write_report(&mut self, buf: &[u8]) -> Result<()> {
        self.file
            .write_all(buf)
            .await
            .with_context(|| format!("Write failed on {}", self.path.display()))?;
        debug!("TX {} bytes: {:02x?}", buf.len(), buf);
        Ok(())
    }

    /* Read a single HID report from the device (blocks until data arrives). */
    pub async fn read_report(&mut self, buf: &mut [u8]) -> Result<usize> {
        let n = self
            .file
            .read(buf)
            .await
            .with_context(|| format!("Read failed on {}", self.path.display()))?;
        debug!("RX {} bytes: {:02x?}", n, &buf[..n]);
        Ok(n)
    }

    /* Send a report and wait for a matching response. */
    /*  */
    /* The `matcher` closure receives each incoming report and returns */
    /* `Some(T)` when the expected response has arrived, or `None` to */
    /* keep waiting. Retries up to `max_attempts` times. */
    pub async fn request<T, F>(
        &mut self,
        report: &[u8],
        max_attempts: u8,
        mut matcher: F,
    ) -> Result<T>
    where
        F: FnMut(&[u8]) -> Option<T>,
    {
        for attempt in 1..=max_attempts {
            self.write_report(report).await?;

            let mut buf = [0u8; MAX_REPORT_LEN];
            for _ in 0..MAX_READS_PER_ATTEMPT {
                match tokio::time::timeout(READ_TIMEOUT, self.read_report(&mut buf)).await {
                    Ok(Ok(n)) => {
                        if let Some(result) = matcher(&buf[..n]) {
                            return Ok(result);
                        }
                    }
                    Ok(Err(e)) => {
                        warn!("Read error on attempt {attempt}: {e}");
                        break;
                    }
                    Err(_elapsed) => {
                        debug!("Timeout on attempt {attempt}");
                        break;
                    }
                }
            }
        }

        bail!(
            "No response from {} after {max_attempts} attempts",
            self.path.display()
        );
    }
}

/* The universal driver interface for all hardware protocols. */
/*  */
/* Every supported protocol (HID++ 1.0, HID++ 2.0, Roccat, etc.) */
/* implements this trait. The daemon calls these methods from the */
/* device actor loop. */
#[async_trait]
pub trait DeviceDriver: Send + Sync {
    /* Returns the driver name for logging purposes. */
    fn name(&self) -> &str;

    /* Probe the device to confirm it speaks this protocol. */
    /*  */
    /* For HID++ this sends a version ping; for other protocols it */
    /* will send an equivalent handshake. Returns `Ok(())` if the */
    /* device responded correctly. */
    async fn probe(&mut self, io: &mut DeviceIo) -> Result<()>;

    /* Read the full device state (profiles, DPIs, buttons, LEDs) */
    /* from hardware into the `DeviceInfo` struct. */
    async fn load_profiles(&mut self, io: &mut DeviceIo, info: &mut DeviceInfo) -> Result<()>;

    /* Write the modified device state back to hardware. */
    /*  */
    /* Only dirty fields should be transmitted; the driver should */
    /* diff the `DeviceInfo` against its internal cached state. */
    async fn commit(&mut self, io: &mut DeviceIo, info: &DeviceInfo) -> Result<()>;
}

/* Instantiate the correct driver based on the driver name from the */
/* `.device` file database. */
pub fn create_driver(driver_name: &str) -> Option<Box<dyn DeviceDriver>> {
    match driver_name {
        "asus" => Some(Box::new(asus::AsusDriver::new())),
        "hidpp10" => Some(Box::new(hidpp10::Hidpp10Driver::new())),
        "hidpp20" => Some(Box::new(hidpp20::Hidpp20Driver::new())),
        _ => {
            warn!("Unknown driver: {driver_name}");
            None
        }
    }
}
