use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use gpio_cdev::{Chip, LineHandle, LineRequestFlags};
use image::{DynamicImage, GenericImageView, RgbImage};
use spidev::{SpiModeFlags, Spidev, SpidevOptions};

use super::common::{
    InkyDisplay, Rotation, clamp_aspect_resize, distribute_error, lighten_image_in_place,
    nearest_colour, pack_buffer_nibbles, load_image_respecting_exif,
};
use super::error::{InkyError, Result};

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

const DESATURATED_PALETTE: [[u8; 3]; 7] = [
    [0, 0, 0],
    [255, 255, 255],
    [0, 255, 0],
    [0, 0, 255],
    [255, 0, 0],
    [255, 255, 0],
    [255, 140, 0],
];

const SATURATED_PALETTE: [[u8; 3]; 7] = [
    [57, 48, 57],
    [255, 255, 255],
    [58, 91, 70],
    [61, 59, 94],
    [156, 72, 75],
    [208, 190, 71],
    [177, 106, 73],
];

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
    pub rotation: Rotation,
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
            rotation: Rotation::Deg0,
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
    rotation: Rotation,
}

impl InkyUc8159 {
    pub fn new(config: InkyUc8159Config) -> Result<Self> {
        let mut chip = Chip::new(&config.gpio_chip)?;

        let cs = chip
            .get_line(config.pins.cs)?
            .request(LineRequestFlags::OUTPUT, 1, "paperwave-cs")?;
        let dc = chip
            .get_line(config.pins.dc)?
            .request(LineRequestFlags::OUTPUT, 0, "paperwave-dc")?;
        let reset = chip.get_line(config.pins.reset)?.request(
            LineRequestFlags::OUTPUT,
            1,
            "paperwave-reset",
        )?;
        let busy =
            chip.get_line(config.pins.busy)?
                .request(LineRequestFlags::INPUT, 0, "paperwave-busy")?;

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
            _ => {
                return Err(InkyError::UnsupportedResolution(
                    config.width,
                    config.height,
                ));
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
            rotation: config.rotation,
        })
    }

    pub fn width(&self) -> u16 {
        self.width
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    pub fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    pub fn input_dimensions(&self) -> (u16, u16) {
        self.rotation.target_dimensions(self.width, self.height)
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
        let (logical_w, logical_h) = self.logical_dimensions_usize();
        if x >= logical_w || y >= logical_h {
            return;
        }

        let index = self.logical_to_physical_index(x, y);
        self.buffer[index] = colour & 0x07;
    }

    pub fn set_image_from_path(&mut self, path: &Path, saturation: f32, lighten: f32) -> Result<()> {
        let image = load_image_respecting_exif(path)?;
        self.set_image(&image, saturation, lighten)
    }

    pub fn set_image(&mut self, image: &DynamicImage, saturation: f32, lighten: f32) -> Result<()> {
        let mut rgb = self.prepare_image(image);
        lighten_image_in_place(&mut rgb, lighten);
        let palette = build_palette(saturation);
        self.quantize_into_buffer(&rgb, &palette);

        Ok(())
    }

    pub fn set_border(&mut self, colour: u8) {
        let value = colour & 0x07;
        if self.border_colour != value {
            self.border_colour = value;
            self.initialised = false;
        }
    }

    pub fn set_buffer(&mut self, data: &[u8]) -> Result<()> {
        let (logical_w, logical_h) = self.logical_dimensions_usize();
        let expected = logical_w * logical_h;
        if data.len() != expected {
            return Err(InkyError::InvalidBufferSize {
                expected,
                received: data.len(),
            });
        }

        for (idx, &value) in data.iter().enumerate() {
            let x = idx % logical_w;
            let y = idx / logical_w;
            let physical_index = self.logical_to_physical_index(x, y);
            self.buffer[physical_index] = value & 0x07;
        }
        Ok(())
    }

    pub fn show(&mut self) -> Result<()> {
        if !self.initialised {
            self.initialise()?;
            self.initialised = true;
        }

        let packed = pack_buffer_nibbles(&self.buffer);
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

    fn prepare_image(&self, image: &DynamicImage) -> RgbImage {
        let (target_w, target_h) = self.input_dimensions();
        let target_w = target_w as u32;
        let target_h = target_h as u32;

        let prepared = if image.dimensions() == (target_w, target_h) {
            image.to_rgb8()
        } else {
            clamp_aspect_resize(image, target_w, target_h)
        };

        self.rotation.apply(prepared)
    }

    fn quantize_into_buffer(&mut self, rgb: &RgbImage, palette: &[[f32; 3]; 7]) {
        let width = self.width as usize;
        let height = self.height as usize;
        let mut working: Vec<[f32; 3]> = rgb
            .pixels()
            .map(|p| [p[0] as f32, p[1] as f32, p[2] as f32])
            .collect();

        for y in 0..height {
            for x in 0..width {
                let idx = y * width + x;
                let old_pixel = working[idx];
                let (closest_index, closest_colour) = nearest_colour(palette, old_pixel);
                self.buffer[idx] = closest_index as u8;

                let error = [
                    old_pixel[0] - closest_colour[0],
                    old_pixel[1] - closest_colour[1],
                    old_pixel[2] - closest_colour[2],
                ];

                distribute_error(&mut working, width, height, x, y, error);
            }
        }
    }

    fn logical_dimensions_usize(&self) -> (usize, usize) {
        let (w, h) = self.input_dimensions();
        (w as usize, h as usize)
    }

    fn logical_to_physical_index(&self, x: usize, y: usize) -> usize {
        let (px, py) = match self.rotation {
            Rotation::Deg0 => (x, y),
            Rotation::Deg90 => ((self.width as usize - 1) - y, x),
            Rotation::Deg180 => (
                (self.width as usize - 1) - x,
                (self.height as usize - 1) - y,
            ),
            Rotation::Deg270 => (y, (self.height as usize - 1) - x),
        };

        py * self.width as usize + px
    }
}

fn build_palette(saturation: f32) -> [[f32; 3]; 7] {
    let sat = saturation.clamp(0.0, 1.0);
    let mut palette = [[0.0f32; 3]; 7];
    for i in 0..7 {
        for channel in 0..3 {
            let saturated = SATURATED_PALETTE[i][channel] as f32;
            let desaturated = DESATURATED_PALETTE[i][channel] as f32;
            palette[i][channel] = saturated * sat + desaturated * (1.0 - sat);
        }
    }
    palette
}

impl InkyDisplay for InkyUc8159 {
    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn set_rotation(&mut self, rotation: Rotation) {
        InkyUc8159::set_rotation(self, rotation);
    }

    fn input_dimensions(&self) -> (u16, u16) {
        InkyUc8159::input_dimensions(self)
    }

    fn clear(&mut self, colour: u8) {
        InkyUc8159::clear(self, colour)
    }

    fn set_pixel(&mut self, x: usize, y: usize, colour: u8) {
        InkyUc8159::set_pixel(self, x, y, colour)
    }

    fn set_image_from_path(&mut self, path: &Path, saturation: f32, lighten: f32) -> Result<()> {
        InkyUc8159::set_image_from_path(self, path, saturation, lighten)
    }

    fn set_image(&mut self, image: &DynamicImage, saturation: f32, lighten: f32) -> Result<()> {
        InkyUc8159::set_image(self, image, saturation, lighten)
    }

    fn show(&mut self) -> Result<()> {
        InkyUc8159::show(self)
    }
}
