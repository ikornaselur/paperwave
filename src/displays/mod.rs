#[cfg(target_os = "linux")]
pub mod detect;

#[cfg(target_os = "linux")]
pub mod uc8159;

#[cfg(target_os = "linux")]
pub use detect::{
    EepromInfo, I2cBusReport, I2cProbeStatus, ProbeInfo, Uc8159Spec, probe_system,
    uc8159_resolution_from_probe,
};

#[cfg(target_os = "linux")]
pub use uc8159::{InkyUc8159, InkyUc8159Config, Pins, Rotation};
