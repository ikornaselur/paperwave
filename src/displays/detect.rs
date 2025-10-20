use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use gpio_cdev::Chip;
use i2cdev::core::I2CDevice;
use i2cdev::linux::{LinuxI2CDevice, LinuxI2CError};

const EEPROM_ADDRESS: u16 = 0x50;
const EEPROM_LENGTH: usize = 29;

const DISPLAY_VARIANT_NAMES: [&str; 25] = [
    "Unknown",
    "Red pHAT (High-Temp)",
    "Yellow wHAT",
    "Black wHAT",
    "Black pHAT",
    "Yellow pHAT",
    "Red wHAT",
    "Red wHAT (High-Temp)",
    "Red wHAT",
    "Unknown",
    "Black pHAT (SSD1608)",
    "Red pHAT (SSD1608)",
    "Yellow pHAT (SSD1608)",
    "Unknown",
    "7-Colour (UC8159) 600x448",
    "7-Colour 640x400 (UC8159)",
    "7-Colour 640x400 (UC8159)",
    "Black wHAT (SSD1683)",
    "Red wHAT (SSD1683)",
    "Yellow wHAT (SSD1683)",
    "7-Colour 800x480 (AC073TC1A)",
    "Spectra 6 13.3 1600x1200 (EL133UF1)",
    "Spectra 6 7.3 800x480 (E673)",
    "Red/Yellow pHAT (JD79661)",
    "Red/Yellow wHAT (JD79668)",
];

#[derive(Clone, Copy, Debug)]
pub struct EepromInfo {
    pub width: u16,
    pub height: u16,
    pub color: u8,
    pub pcb_variant: u8,
    pub display_variant: u8,
}

impl fmt::Display for EepromInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}x{} colour={} pcb_variant={:.1} display_variant={} ({})",
            self.width,
            self.height,
            self.color,
            self.pcb_variant as f32 / 10.0,
            self.display_variant,
            self.variant_name()
        )
    }
}

impl EepromInfo {
    pub fn variant_name(&self) -> &'static str {
        DISPLAY_VARIANT_NAMES
            .get(self.display_variant as usize)
            .copied()
            .unwrap_or("Unknown")
    }

    pub fn display_spec(&self) -> Option<DisplaySpec> {
        match self.display_variant {
            14 => Some(DisplaySpec::Uc8159 {
                width: 600,
                height: 448,
                variant: self.display_variant,
            }),
            16 => Some(DisplaySpec::Uc8159 {
                width: 640,
                height: 400,
                variant: self.display_variant,
            }),
            21 => Some(DisplaySpec::El133Uf1 {
                width: self.width,
                height: self.height,
            }),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum DisplaySpec {
    Uc8159 {
        width: u16,
        height: u16,
        variant: u8,
    },
    El133Uf1 {
        width: u16,
        height: u16,
    },
}

impl fmt::Display for DisplaySpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DisplaySpec::Uc8159 {
                width,
                height,
                variant,
            } => write!(f, "UC8159 variant {} ({}x{})", variant, width, height),
            DisplaySpec::El133Uf1 { width, height } => {
                write!(f, "Spectra 6 EL133UF1 ({}x{})", width, height)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct I2cBusReport {
    pub path: PathBuf,
    pub status: I2cProbeStatus,
}

#[derive(Clone, Debug)]
pub enum I2cProbeStatus {
    Found(EepromInfo),
    Blank,
    Invalid(String),
    Unavailable,
    Error(String),
}

#[derive(Debug, Default)]
pub struct ProbeInfo {
    pub eeprom: Option<EepromInfo>,
    pub eeprom_error: Option<String>,
    pub display: Option<DisplaySpec>,
    pub eeprom_bus: Option<PathBuf>,
    pub spi_devices: Vec<PathBuf>,
    pub gpio_chips: Vec<PathBuf>,
    pub gpio_chip_labels: Vec<String>,
    pub i2c_buses: Vec<PathBuf>,
    pub i2c_bus_results: Vec<I2cBusReport>,
}

pub fn probe_system() -> ProbeInfo {
    let mut info = ProbeInfo::default();

    info.spi_devices = list_matching("/dev", "spidev");
    info.gpio_chips = list_matching("/dev", "gpiochip");
    info.i2c_buses = list_matching("/dev", "i2c-");
    info.gpio_chip_labels = list_gpio_chip_labels(&info.gpio_chips);

    for bus in &info.i2c_buses {
        let status = read_eeprom(bus);
        info.i2c_bus_results.push(I2cBusReport {
            path: bus.clone(),
            status: status.clone(),
        });

        match status {
            I2cProbeStatus::Found(eeprom) => {
                if info.eeprom.is_none() {
                    info.display = eeprom.display_spec();
                    info.eeprom = Some(eeprom);
                    info.eeprom_bus = Some(bus.clone());
                    info.eeprom_error = None;
                }
            }
            I2cProbeStatus::Invalid(reason) => {
                if info.eeprom_error.is_none() {
                    info.eeprom_error = Some(format!("invalid data: {reason}"));
                }
            }
            I2cProbeStatus::Error(err) => {
                if info.eeprom_error.is_none() {
                    info.eeprom_error = Some(err);
                }
            }
            _ => {}
        }
    }

    info
}

pub fn read_eeprom<P: AsRef<Path>>(path: P) -> I2cProbeStatus {
    let path_ref = path.as_ref();
    let mut device = match LinuxI2CDevice::new(path_ref, EEPROM_ADDRESS) {
        Ok(dev) => dev,
        Err(err) => return handle_i2c_open_error(err),
    };

    if let Err(err) = device.write(&[0x00, 0x00]) {
        return map_i2c_error(err);
    }

    let mut buf = [0u8; EEPROM_LENGTH];
    if let Err(err) = device.read(&mut buf) {
        return map_i2c_error(err);
    }

    if is_blank_eeprom(&buf) {
        return I2cProbeStatus::Blank;
    }

    match parse_eeprom(&buf) {
        Ok(parsed) => I2cProbeStatus::Found(parsed),
        Err(reason) => I2cProbeStatus::Invalid(reason),
    }
}

fn parse_eeprom(data: &[u8]) -> Result<EepromInfo, String> {
    let width = u16::from_le_bytes([data[0], data[1]]);
    let height = u16::from_le_bytes([data[2], data[3]]);
    let color = data[4];
    let pcb_variant = data[5];
    let display_variant = data[6];

    if width == 0 || height == 0 || width == u16::MAX || height == u16::MAX {
        return Err(format!(
            "width/height out of range (width={width}, height={height})"
        ));
    }

    if display_variant == u8::MAX {
        return Err("display variant invalid (255)".to_string());
    }

    Ok(EepromInfo {
        width,
        height,
        color,
        pcb_variant,
        display_variant,
    })
}

fn list_matching(dir: &str, prefix: &str) -> Vec<PathBuf> {
    let mut entries = Vec::new();
    if let Ok(read_dir) = fs::read_dir(dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if name.starts_with(prefix) {
                    entries.push(path);
                }
            }
        }
    }
    entries.sort();
    entries
}

fn list_gpio_chip_labels(chips: &[PathBuf]) -> Vec<String> {
    let mut labels = Vec::new();
    for path in chips {
        if let Ok(chip) = Chip::new(path.to_string_lossy().as_ref()) {
            labels.push(format!(
                "{} -> {} ({})",
                path.display(),
                chip.name(),
                chip.label()
            ));
        }
    }
    labels
}

fn map_i2c_error(err: LinuxI2CError) -> I2cProbeStatus {
    match err {
        LinuxI2CError::Io(io_err) => handle_io_error(io_err),
        LinuxI2CError::Errno(code) => handle_errno(code),
    }
}

pub fn uc8159_resolution_from_probe(probe: &ProbeInfo) -> Option<(u16, u16)> {
    match probe.display {
        Some(DisplaySpec::Uc8159 { width, height, .. }) => Some((width, height)),
        _ => None,
    }
}

fn handle_i2c_open_error(err: LinuxI2CError) -> I2cProbeStatus {
    match err {
        LinuxI2CError::Io(io_err) => handle_io_error(io_err),
        LinuxI2CError::Errno(code) => handle_errno(code),
    }
}

fn handle_io_error(io_err: io::Error) -> I2cProbeStatus {
    match io_err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => I2cProbeStatus::Unavailable,
        _ => I2cProbeStatus::Error(io_err.to_string()),
    }
}

fn handle_errno(code: i32) -> I2cProbeStatus {
    let io_err = io::Error::from_raw_os_error(code);
    match io_err.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => I2cProbeStatus::Unavailable,
        _ => I2cProbeStatus::Error(io_err.to_string()),
    }
}

fn is_blank_eeprom(data: &[u8]) -> bool {
    data.iter().all(|&b| b == 0xFF || b == 0x00)
}
