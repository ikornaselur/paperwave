use clap::{Parser, ValueEnum};
#[cfg(target_os = "linux")]
use image::{DynamicImage, Rgb, RgbImage};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "inkwell", about = "UC8159 demo utility")]
struct Args {
    /// Optional PNG to display
    #[arg(value_name = "IMAGE")]
    image: Option<PathBuf>,

    /// Palette saturation from 0.0 (desaturated) to 1.0 (saturated)
    #[arg(short, long, value_name = "SAT", default_value_t = 0.5)]
    saturation: f32,

    /// Rotate image before display (degrees clockwise)
    #[arg(short, long = "rotate", value_enum, default_value_t = RotationArg::Deg0)]
    rotation: RotationArg,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum RotationArg {
    #[value(name = "0")]
    Deg0,
    #[value(name = "90")]
    Deg90,
    #[value(name = "180")]
    Deg180,
    #[value(name = "270")]
    Deg270,
}

#[cfg(target_os = "linux")]
impl From<RotationArg> for inkwell::Rotation {
    fn from(value: RotationArg) -> Self {
        match value {
            RotationArg::Deg0 => inkwell::Rotation::Deg0,
            RotationArg::Deg90 => inkwell::Rotation::Deg90,
            RotationArg::Deg180 => inkwell::Rotation::Deg180,
            RotationArg::Deg270 => inkwell::Rotation::Deg270,
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let args = Args::parse();
    let rotation = args.rotation.into();

    if let Some(path) = args.image {
        if let Err(err) = run_image(&path, rotation, args.saturation) {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(err) = run_demo(rotation, args.saturation) {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("UC8159 demo can only run on Linux targets.");
}

#[cfg(target_os = "linux")]
fn run_demo(rotation: inkwell::Rotation, saturation: f32) -> inkwell::uc8159::Result<()> {
    use inkwell::{InkyUc8159, InkyUc8159Config};

    let mut config = InkyUc8159Config::default();
    config.rotation = rotation;
    let mut display = InkyUc8159::new(config)?;

    let (input_w, input_h) = display.input_dimensions();
    let mut image = RgbImage::new(input_w as u32, input_h as u32);

    let palette = [
        Rgb([57, 48, 57]),
        Rgb([255, 255, 255]),
        Rgb([58, 91, 70]),
        Rgb([61, 59, 94]),
        Rgb([156, 72, 75]),
        Rgb([208, 190, 71]),
        Rgb([177, 106, 73]),
    ];

    let stripes = palette.len();
    let stripe_height = ((input_h as usize) + stripes - 1) / stripes;

    for (index, colour) in palette.iter().enumerate() {
        let y_start = index * stripe_height;
        let y_end = if index == palette.len() - 1 {
            input_h as usize
        } else {
            (y_start + stripe_height).min(input_h as usize)
        };

        for y in y_start..y_end {
            for x in 0..input_w as usize {
                image.put_pixel(x as u32, y as u32, *colour);
            }
        }
    }

    let dynamic = DynamicImage::ImageRgb8(image);
    display.set_image(&dynamic, saturation)?;
    display.show()
}

#[cfg(target_os = "linux")]
fn run_image(
    path: &PathBuf,
    rotation: inkwell::Rotation,
    saturation: f32,
) -> inkwell::uc8159::Result<()> {
    use inkwell::{InkyUc8159, InkyUc8159Config};

    let mut config = InkyUc8159Config::default();
    config.rotation = rotation;
    let mut display = InkyUc8159::new(config)?;
    display.set_image_from_path(path, saturation)?;
    display.show()
}
