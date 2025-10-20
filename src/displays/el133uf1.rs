use std::io::Write;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use gpio_cdev::{Chip, LineHandle, LineRequestFlags};
use image::imageops;
use image::{DynamicImage, GenericImageView, ImageBuffer, RgbImage};
use spidev::{SpiModeFlags, Spidev, SpidevOptions};

use super::common::{
    InkyDisplay, Rotation, clamp_aspect_resize, distribute_error, nearest_colour, pack_luma_nibbles,
};
use super::error::{InkyError, Result};

const RESET_PIN_DEFAULT: u32 = 27;
const BUSY_PIN_DEFAULT: u32 = 17;
const DC_PIN_DEFAULT: u32 = 22;
const CS0_PIN_DEFAULT: u32 = 26;
const CS1_PIN_DEFAULT: u32 = 16;

const CS0_SEL: u8 = 0b01;
const CS1_SEL: u8 = 0b10;
const CS_BOTH_SEL: u8 = CS0_SEL | CS1_SEL;

const EL133UF1_PSR: u8 = 0x00;
const EL133UF1_PWR: u8 = 0x01;
const EL133UF1_POF: u8 = 0x02;
const EL133UF1_PON: u8 = 0x04;
const EL133UF1_BTST_N: u8 = 0x05;
const EL133UF1_BTST_P: u8 = 0x06;
const EL133UF1_DTM: u8 = 0x10;
const EL133UF1_DRF: u8 = 0x12;
const EL133UF1_PLL: u8 = 0x30;
const EL133UF1_CDI: u8 = 0x50;
const EL133UF1_TCON: u8 = 0x60;
const EL133UF1_TRES: u8 = 0x61;
const EL133UF1_AGID: u8 = 0x86;
const EL133UF1_PWS: u8 = 0xE3;
const EL133UF1_CCSET: u8 = 0xE0;
const EL133UF1_CMD66: u8 = 0xF0;
const EL133UF1_ANTM: u8 = 0x74;
const EL133UF1_EN_BUF: u8 = 0xB6;
const EL133UF1_BOOST_VDDP_EN: u8 = 0xB7;
const EL133UF1_BUCK_BOOST_VDDN: u8 = 0xB0;
const EL133UF1_BTST_P_PARAM: [u8; 2] = [0xD8, 0x18];
const EL133UF1_BTST_N_PARAM: [u8; 2] = [0xD8, 0x18];
const EL133UF1_TFT_VCOM_POWER: u8 = 0xB1;

const SPI_CHUNK_SIZE: usize = 4096;

const DESATURATED_PALETTE: [[u8; 3]; 6] = [
    [0, 0, 0],
    [255, 255, 255],
    [255, 255, 0],
    [255, 0, 0],
    [0, 0, 255],
    [0, 255, 0],
];

const SATURATED_PALETTE: [[u8; 3]; 6] = [
    [0, 0, 0],
    [161, 164, 165],
    [208, 190, 71],
    [156, 72, 75],
    [61, 59, 94],
    [58, 91, 70],
];

const REMAP: [u8; 6] = [0, 1, 2, 3, 5, 6];

pub struct SpectraPins {
    pub cs0: u32,
    pub cs1: u32,
    pub dc: u32,
    pub reset: u32,
    pub busy: u32,
}

impl Default for SpectraPins {
    fn default() -> Self {
        Self {
            cs0: CS0_PIN_DEFAULT,
            cs1: CS1_PIN_DEFAULT,
            dc: DC_PIN_DEFAULT,
            reset: RESET_PIN_DEFAULT,
            busy: BUSY_PIN_DEFAULT,
        }
    }
}

pub struct InkyEl133Uf1Config {
    pub width: u16,
    pub height: u16,
    pub spi_path: String,
    pub gpio_chip: String,
    pub pins: SpectraPins,
    pub rotation: Rotation,
}

impl Default for InkyEl133Uf1Config {
    fn default() -> Self {
        Self {
            width: 1600,
            height: 1200,
            spi_path: "/dev/spidev0.0".to_string(),
            gpio_chip: "/dev/gpiochip0".to_string(),
            pins: SpectraPins::default(),
            rotation: Rotation::Deg0,
        }
    }
}

pub struct InkyEl133Uf1 {
    spi: Spidev,
    cs0: LineHandle,
    cs1: LineHandle,
    dc: LineHandle,
    reset: LineHandle,
    busy: LineHandle,
    width: u16,
    height: u16,
    rotation: Rotation,
    buffer: Vec<u8>,
    initialised: bool,
}

impl InkyEl133Uf1 {
    pub fn new(config: InkyEl133Uf1Config) -> Result<Self> {
        let mut chip = Chip::new(&config.gpio_chip)?;

        let cs0 =
            chip.get_line(config.pins.cs0)?
                .request(LineRequestFlags::OUTPUT, 1, "inkwell-cs0")?;
        let cs1 =
            chip.get_line(config.pins.cs1)?
                .request(LineRequestFlags::OUTPUT, 1, "inkwell-cs1")?;
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
                .request(LineRequestFlags::INPUT, 1, "inkwell-busy")?;

        drop(chip);

        let mut spi = Spidev::open(&config.spi_path)?;
        let options = SpidevOptions::new()
            .bits_per_word(8)
            .max_speed_hz(10_000_000)
            .mode(SpiModeFlags::SPI_MODE_0)
            .build();
        spi.configure(&options)?;

        let buffer = vec![0; (config.width as usize) * (config.height as usize)];

        Ok(Self {
            spi,
            cs0,
            cs1,
            dc,
            reset,
            busy,
            width: config.width,
            height: config.height,
            rotation: config.rotation,
            buffer,
            initialised: false,
        })
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

    fn quantize_into_buffer(&mut self, rgb: &RgbImage, palette: &[[f32; 3]; 6]) {
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
                self.buffer[idx] = REMAP[closest_index as usize];

                let error = [
                    old_pixel[0] - closest_colour[0],
                    old_pixel[1] - closest_colour[1],
                    old_pixel[2] - closest_colour[2],
                ];

                distribute_error(&mut working, width, height, x, y, error);
            }
        }
    }

    fn initialise(&mut self) -> Result<()> {
        self.reset.set_value(0)?;
        thread::sleep(Duration::from_millis(30));
        self.reset.set_value(1)?;
        thread::sleep(Duration::from_millis(30));

        self.busy_wait(Duration::from_millis(300)).ok();

        self.send_command(
            EL133UF1_ANTM,
            CS0_SEL,
            &[0xC0, 0x1C, 0x1C, 0xCC, 0xCC, 0xCC, 0x15, 0x15, 0x55],
        )?;
        self.send_command(
            EL133UF1_CMD66,
            CS_BOTH_SEL,
            &[0x49, 0x55, 0x13, 0x5D, 0x05, 0x10],
        )?;
        self.send_command(EL133UF1_PSR, CS_BOTH_SEL, &[0xDF, 0x69])?;
        self.send_command(EL133UF1_PLL, CS_BOTH_SEL, &[0x08])?;
        self.send_command(EL133UF1_CDI, CS_BOTH_SEL, &[0xF7])?;
        self.send_command(EL133UF1_TCON, CS_BOTH_SEL, &[0x03, 0x03])?;
        self.send_command(EL133UF1_AGID, CS_BOTH_SEL, &[0x10])?;
        self.send_command(EL133UF1_PWS, CS_BOTH_SEL, &[0x22])?;
        self.send_command(EL133UF1_CCSET, CS_BOTH_SEL, &[0x01])?;
        self.send_command(EL133UF1_TRES, CS_BOTH_SEL, &[0x04, 0xB0, 0x03, 0x20])?;

        self.send_command(EL133UF1_PWR, CS0_SEL, &[0x0F, 0x00, 0x28, 0x2C, 0x28, 0x38])?;
        self.send_command(EL133UF1_EN_BUF, CS0_SEL, &[0x07])?;
        self.send_command(EL133UF1_BTST_P, CS0_SEL, &EL133UF1_BTST_P_PARAM)?;
        self.send_command(EL133UF1_BOOST_VDDP_EN, CS0_SEL, &[0x01])?;
        self.send_command(EL133UF1_BTST_N, CS0_SEL, &EL133UF1_BTST_N_PARAM)?;
        self.send_command(EL133UF1_BUCK_BOOST_VDDN, CS0_SEL, &[0x01])?;
        self.send_command(EL133UF1_TFT_VCOM_POWER, CS0_SEL, &[0x02])?;

        Ok(())
    }

    fn send_frame(&mut self, buf_a: &[u8], buf_b: &[u8]) -> Result<()> {
        self.send_command(EL133UF1_DTM, CS0_SEL, buf_a)?;
        self.send_command(EL133UF1_DTM, CS1_SEL, buf_b)?;

        self.send_command(EL133UF1_PON, CS_BOTH_SEL, &[])?;
        self.busy_wait(Duration::from_millis(200)).ok();

        self.send_command(EL133UF1_DRF, CS_BOTH_SEL, &[0x00])?;
        self.busy_wait(Duration::from_secs(32))?;

        self.send_command(EL133UF1_POF, CS_BOTH_SEL, &[0x00])?;
        self.busy_wait(Duration::from_millis(200)).ok();

        Ok(())
    }

    fn busy_wait(&mut self, timeout: Duration) -> Result<()> {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.busy.get_value()? == 0 {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(InkyError::Timeout("busy", timeout))
    }

    fn send_command(&mut self, command: u8, cs_sel: u8, data: &[u8]) -> Result<()> {
        if cs_sel & CS0_SEL != 0 {
            self.cs0.set_value(0)?;
        }
        if cs_sel & CS1_SEL != 0 {
            self.cs1.set_value(0)?;
        }

        self.dc.set_value(0)?;
        self.spi.write(&[command])?;

        if !data.is_empty() {
            self.dc.set_value(1)?;
            for chunk in data.chunks(SPI_CHUNK_SIZE) {
                self.spi.write(chunk)?;
            }
        }

        self.cs0.set_value(1)?;
        self.cs1.set_value(1)?;
        self.dc.set_value(0)?;
        Ok(())
    }

    fn logical_dimensions_usize(&self) -> (usize, usize) {
        let (w, h) = self.rotation.target_dimensions(self.width, self.height);
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

fn blend_palette(saturation: f32) -> [[f32; 3]; 6] {
    let sat = saturation.clamp(0.0, 1.0);
    let mut palette = [[0.0f32; 3]; 6];
    for i in 0..6 {
        for channel in 0..3 {
            let saturated = SATURATED_PALETTE[i][channel] as f32;
            let desaturated = DESATURATED_PALETTE[i][channel] as f32;
            palette[i][channel] = saturated * sat + desaturated * (1.0 - sat);
        }
    }
    palette
}

impl InkyDisplay for InkyEl133Uf1 {
    fn width(&self) -> u16 {
        self.width
    }

    fn height(&self) -> u16 {
        self.height
    }

    fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    fn input_dimensions(&self) -> (u16, u16) {
        self.rotation.target_dimensions(self.width, self.height)
    }

    fn clear(&mut self, colour: u8) {
        self.buffer.fill(colour & 0x07);
    }

    fn set_pixel(&mut self, x: usize, y: usize, colour: u8) {
        let (logical_w, logical_h) = self.logical_dimensions_usize();
        if x >= logical_w || y >= logical_h {
            return;
        }
        let idx = self.logical_to_physical_index(x, y);
        self.buffer[idx] = colour & 0x07;
    }

    fn set_image_from_path(&mut self, path: &Path, saturation: f32) -> Result<()> {
        let image = image::open(path)?;
        self.set_image(&image, saturation)
    }

    fn set_image(&mut self, image: &DynamicImage, saturation: f32) -> Result<()> {
        let rgb = self.prepare_image(image);
        let palette = blend_palette(saturation);
        self.quantize_into_buffer(&rgb, &palette);
        Ok(())
    }

    fn show(&mut self) -> Result<()> {
        if !self.initialised {
            self.initialise()?;
            self.initialised = true;
        }

        let image_buf = self.buffer.clone();
        let mut image = ImageBuffer::<image::Luma<u8>, _>::from_raw(
            self.width as u32,
            self.height as u32,
            image_buf,
        )
        .expect("valid buffer dimensions");

        image = imageops::rotate270(&image);
        let width = image.width() as usize;
        let split = width / 2;

        let buf_a = pack_luma_nibbles(&image, 0, split);
        let buf_b = pack_luma_nibbles(&image, split, width);

        self.send_frame(&buf_a, &buf_b)
    }
}
