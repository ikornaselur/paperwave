use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "inkwell", about = "UC8159 demo utility")]
struct Args {
    /// Optional PNG to display (must match panel resolution)
    #[arg(value_name = "IMAGE")]
    image: Option<PathBuf>,

    /// Palette saturation from 0.0 (desaturated) to 1.0 (saturated)
    #[arg(short, long, value_name = "SAT", default_value_t = 0.5)]
    saturation: f32,
}

#[cfg(target_os = "linux")]
fn main() {
    let args = Args::parse();

    if let Some(path) = args.image {
        if let Err(err) = run_image(&path, args.saturation) {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(err) = run_demo(args.saturation) {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("UC8159 demo can only run on Linux targets.");
}

#[cfg(target_os = "linux")]
fn run_demo(saturation: f32) -> inkwell::uc8159::Result<()> {
    use inkwell::{InkyUc8159, InkyUc8159Config};

    let mut display = InkyUc8159::new(InkyUc8159Config::default())?;

    let _ = saturation;

    let colours = [0u8, 1, 2, 3, 4, 5, 6];
    let stripe_height = (display.height() as usize) / colours.len();
    let width = display.width() as usize;

    for (index, &colour) in colours.iter().enumerate() {
        let y_start = index * stripe_height;
        let y_end = if index == colours.len() - 1 {
            display.height() as usize
        } else {
            y_start + stripe_height
        };

        for y in y_start..y_end {
            for x in 0..width {
                display.set_pixel(x, y, colour);
            }
        }
    }

    display.show()
}

#[cfg(target_os = "linux")]
fn run_image(path: &PathBuf, saturation: f32) -> inkwell::uc8159::Result<()> {
    use inkwell::{InkyUc8159, InkyUc8159Config};

    let mut display = InkyUc8159::new(InkyUc8159Config::default())?;
    display.set_image_from_path(path, saturation)?;
    display.show()
}
