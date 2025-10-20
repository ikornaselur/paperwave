#[cfg(target_os = "linux")]
pub mod displays;

#[cfg(target_os = "linux")]
pub use displays::{
    EepromInfo, I2cBusReport, I2cProbeStatus, InkyUc8159, InkyUc8159Config, Pins, ProbeInfo,
    Rotation, Uc8159Spec, probe_system, uc8159_resolution_from_probe,
};

#[cfg(target_os = "linux")]
pub use displays::uc8159::{self as uc8159, Result as Uc8159Result};
