/* Library root — re-exports modules for benchmarks and integration tests.
 *
 * The daemon binary is `main.rs`; this `lib.rs` exists so that Criterion
 * benchmarks and external integration tests can `use ratbagd_rs::driver`
 * etc. without duplicating module declarations. */

pub mod actor;
pub mod dbus;
pub mod device;
pub mod device_database;
pub mod driver;
pub mod error;
#[cfg(feature = "dev-hooks")]
pub mod test_device;
pub mod udev_monitor;
