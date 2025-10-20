use std::io::{self, Write};
use std::thread;
use std::time::{Duration, Instant};

use gpio_cdev::{Chip, LineHandle, LineRequestFlags};
use spidev::{SpiModeFlags, Spidev, SpidevOptions};

const UC8159_PSR: u8 = 0x00;
const UC8159_PWR: u8 = 0x01;
const UC8159_POF: u8 = 0x02;
const UC8159_PFS: u8 = 0x03;
const UC8159_PON: u8 = 0x04;
const UC8159_DTM1: u8 = 0x10;
const UC8159_DRF: u8 = 0x12;
const UC8159_PLL: u8 = 0x30;
const UC8159_TSE: u8 = 0x41;
const UC8159_CDI: u8 = 0x50;
const UC8159_TCON: u8 = 0x60;
const UC8159_TRES: u8 = 0x61;
const UC8159_DAM: u8 = 0x65;
const UC8159_PWS: u8 = 0xE3;

const SPI_CHUNK_SIZE: usize = 4096;

#[derive(Debug)]
pub enum InkyError {
    Io(io::Error),
    Gpio(gpio_cdev::errors::Error),
    Timeout(&'static str, Duration),
    InvalidBufferSize { expected: usize, received: usize },
    UnsupportedResolution(u16, u16),
}

impl std::fmt::Display for InkyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            InkyError::Io(err) => write!(f, "IO error: {err}"),
            InkyError::Gpio(err) => write!(f, "GPIO error: {err}"),
            InkyError::Timeout(context, duration) => {
                write!(f, "Timed out waiting for {context} after {:?}", duration)
            }
            InkyError::InvalidBufferSize { expected, received } => {
                write!(
                    f,
                    "Invalid buffer size: expected {expected}, got {received}"
                )
            }
            InkyError::UnsupportedResolution(w, h) => {
                write!(f, "Unsupported resolution {w}x{h}")
            }
        }
    }
}

impl std::error::Error for InkyError {}

impl From<io::Error> for InkyError {
    fn from(err: io::Error) -> Self {
        InkyError::Io(err)
    }
}

impl From<gpio_cdev::errors::Error> for InkyError {
    fn from(err: gpio_cdev::errors::Error) -> Self {
        InkyError::Gpio(err)
    }
}

pub type Result<T> = std::result::Result<T, InkyError>;

#[derive(Clone, Copy)]
pub struct Pins {
    pub cs: u32,
    pub dc: u32,
    pub reset: u32,
    pub busy: u32,
}

impl Default for Pins {
    fn default() -> Self {
        Self {
            cs: 8,
            dc: 22,
            reset: 27,
            busy: 17,
        }
    }
}

pub struct InkyUc8159Config {
    pub width: u16,
    pub height: u16,
    pub spi_path: String,
    pub gpio_chip: String,
    pub pins: Pins,
    pub border_colour: u8,
}

impl Default for InkyUc8159Config {
    fn default() -> Self {
        Self {
            width: 600,
            height: 448,
            spi_path: "/dev/spidev0.0".to_string(),
            gpio_chip: "/dev/gpiochip0".to_string(),
            pins: Pins::default(),
            border_colour: 1,
        }
    }
}

pub struct InkyUc8159 {
    spi: Spidev,
    cs: LineHandle,
    dc: LineHandle,
    reset: LineHandle,
    busy: LineHandle,
    width: u16,
    height: u16,
    resolution_setting: u8,
    buffer: Vec<u8>,
    border_colour: u8,
    initialised: bool,
}

impl InkyUc8159 {
    pub fn new(config: InkyUc8159Config) -> Result<Self> {
        let mut chip = Chip::new(&config.gpio_chip)?;

        let cs =
            chip.get_line(config.pins.cs)?
                .request(LineRequestFlags::OUTPUT, 1, "inkwell-cs")?;

        let dc =
            chip.get_line(config.pins.dc)?
                .request(LineRequestFlags::OUTPUT, 0, "inkwell-dc")?;

        let reset = chip.get_line(config.pins.reset)?.request(
            LineRequestFlags::OUTPUT,
            1,
            "inkwell-reset",
        )?;

        let busy =
            chip.get_line(config.pins.busy)?
                .request(LineRequestFlags::INPUT, 0, "inkwell-busy")?;

        drop(chip);

        let mut spi = Spidev::open(config.spi_path)?;
        let options = SpidevOptions::new()
            .bits_per_word(8)
            .max_speed_hz(3_000_000)
            .mode(SpiModeFlags::SPI_MODE_0 | SpiModeFlags::SPI_NO_CS)
            .build();
        spi.configure(&options)?;

        let resolution_setting = match (config.width, config.height) {
            (600, 448) => 0b11,
            (640, 400) => 0b10,
            other => {
                return Err(InkyError::UnsupportedResolution(other.0, other.1));
            }
        };

        let buffer = vec![0; (config.width as usize) * (config.height as usize)];

        Ok(Self {
            spi,
            cs,
            dc,
            reset,
            busy,
            width: config.width,
            height: config.height,
            resolution_setting,
            buffer,
            border_colour: config.border_colour & 0x07,
            initialised: false,
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn buffer(&self) -> &[u8] {
        &self.buffer
    }

    pub fn buffer_mut(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    pub fn clear(&mut self, colour: u8) {
        let value = colour & 0x07;
        self.buffer.fill(value);
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, colour: u8) {
        if x >= self.width as usize || y >= self.height as usize {
            return;
        }
        let index = y * self.width as usize + x;
        self.buffer[index] = colour & 0x07;
    }

    pub fn set_border(&mut self, colour: u8) {
        let value = colour & 0x07;
        if self.border_colour != value {
            self.border_colour = value;
            self.initialised = false;
        }
    }

    pub fn set_buffer(&mut self, data: &[u8]) -> Result<()> {
        let expected = self.buffer.len();
        if data.len() != expected {
            return Err(InkyError::InvalidBufferSize {
                expected,
                received: data.len(),
            });
        }
        for (dst, src) in self.buffer.iter_mut().zip(data.iter()) {
            *dst = src & 0x07;
        }
        Ok(())
    }

    pub fn show(&mut self) -> Result<()> {
        if !self.initialised {
            self.initialise()?;
            self.initialised = true;
        }

        let packed = self.pack_buffer();
        self.send_command_data(UC8159_DTM1, &packed)?;

        self.send_command(UC8159_PON)?;
        let _ = self.busy_wait(Duration::from_millis(200));

        self.send_command(UC8159_DRF)?;
        self.busy_wait(Duration::from_secs(32))?;

        self.send_command(UC8159_POF)?;
        let _ = self.busy_wait(Duration::from_millis(200));

        Ok(())
    }

    fn initialise(&mut self) -> Result<()> {
        self.hardware_reset()?;

        self.busy_wait(Duration::from_secs(1)).ok();

        let mut tres = [0u8; 4];
        tres[..2].copy_from_slice(&self.width.to_be_bytes());
        tres[2..].copy_from_slice(&self.height.to_be_bytes());
        self.send_command_data(UC8159_TRES, &tres)?;

        let psr = [(self.resolution_setting << 6) | 0b0010_1111, 0x08];
        self.send_command_data(UC8159_PSR, &psr)?;

        let pwr = [
            (0x06 << 3) | (0x01 << 2) | (0x01 << 1) | 0x01,
            0x00,
            0x23,
            0x23,
        ];
        self.send_command_data(UC8159_PWR, &pwr)?;

        self.send_command_data(UC8159_PLL, &[0x3C])?;
        self.send_command_data(UC8159_TSE, &[0x00])?;

        let cdi = (self.border_colour << 5) | 0x17;
        self.send_command_data(UC8159_CDI, &[cdi])?;

        self.send_command_data(UC8159_TCON, &[0x22])?;
        self.send_command_data(UC8159_DAM, &[0x00])?;
        self.send_command_data(UC8159_PWS, &[0xAA])?;
        self.send_command_data(UC8159_PFS, &[0x00])?;

        Ok(())
    }

    fn hardware_reset(&mut self) -> Result<()> {
        self.reset.set_value(0)?;
        thread::sleep(Duration::from_millis(100));
        self.reset.set_value(1)?;
        thread::sleep(Duration::from_millis(100));
        Ok(())
    }

    fn busy_wait(&mut self, timeout: Duration) -> Result<()> {
        let start = Instant::now();

        if self.busy.get_value()? != 0 {
            thread::sleep(timeout);
            return Ok(());
        }

        while start.elapsed() < timeout {
            if self.busy.get_value()? != 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }

        Err(InkyError::Timeout("busy", timeout))
    }

    fn send_command(&mut self, command: u8) -> Result<()> {
        self.write_spi(false, &[command])
    }

    fn send_command_data(&mut self, command: u8, data: &[u8]) -> Result<()> {
        self.write_spi(false, &[command])?;
        if !data.is_empty() {
            self.write_spi(true, data)?;
        }
        Ok(())
    }

    fn write_spi(&mut self, is_data: bool, payload: &[u8]) -> Result<()> {
        self.dc.set_value(if is_data { 1 } else { 0 })?;
        self.cs.set_value(0)?;

        if payload.len() <= SPI_CHUNK_SIZE {
            self.spi.write(payload)?;
        } else {
            for chunk in payload.chunks(SPI_CHUNK_SIZE) {
                self.spi.write(chunk)?;
            }
        }

        self.cs.set_value(1)?;
        Ok(())
    }

    fn pack_buffer(&self) -> Vec<u8> {
        let mut packed = Vec::with_capacity((self.buffer.len() + 1) / 2);
        let mut iter = self.buffer.iter();
        while let Some(&high) = iter.next() {
            let low = iter.next().copied().unwrap_or(0);
            let byte = ((high & 0x0F) << 4) | (low & 0x0F);
            packed.push(byte);
        }
        packed
    }
}
