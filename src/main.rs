use clap::{Parser, Subcommand, ValueEnum};
#[cfg(target_os = "linux")]
use image::{DynamicImage, Rgb, RgbImage};
use std::path::PathBuf;

#[cfg(target_os = "linux")]
mod web;

#[derive(Parser, Debug)]
#[command(
    name = "paperwave",
    about = "CLI tool to display images on Inky displays"
)]
struct Args {
    /// Subcommands (e.g., `web`)
    #[command(subcommand)]
    cmd: Option<Command>,

    /// Optional PNG to display
    #[arg(value_name = "IMAGE")]
    image: Option<PathBuf>,

    /// Palette saturation from 0.0 (desaturated) to 1.0 (saturated)
    #[arg(short, long, value_name = "SAT", default_value_t = 0.5)]
    saturation: f32,

    /// Lighten image before quantization (0.0 = none, 1.0 = strongest)
    #[arg(short = 'l', long, value_name = "LIGHTEN", default_value_t = 0.0)]
    lighten: f32,

    /// Rotate image before display (degrees clockwise)
    #[arg(short, long = "rotate", value_enum, default_value_t = RotationArg::Deg0)]
    rotation: RotationArg,

    /// Probe hardware and report detection results without updating the panel
    #[arg(long)]
    detect_only: bool,

    /// Print probe/debug information before running
    #[arg(long)]
    debug: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the web server for uploading images
    #[cfg(target_os = "linux")]
    Web {
        /// Bind host (default: 0.0.0.0)
        #[arg(long, default_value = "0.0.0.0")]
        host: String,
        /// Bind port (default: 8080)
        #[arg(long, default_value_t = 8080)]
        port: u16,
    },
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
impl From<RotationArg> for paperwave::Rotation {
    fn from(value: RotationArg) -> Self {
        match value {
            RotationArg::Deg0 => paperwave::Rotation::Deg0,
            RotationArg::Deg90 => paperwave::Rotation::Deg90,
            RotationArg::Deg180 => paperwave::Rotation::Deg180,
            RotationArg::Deg270 => paperwave::Rotation::Deg270,
        }
    }
}

#[cfg(target_os = "linux")]
fn main() {
    let args = Args::parse();
    #[cfg(target_os = "linux")]
    if let Some(Command::Web { host, port }) = &args.cmd {
        if let Err(err) = run_web(host, *port) {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
        return;
    }

    let rotation = args.rotation.into();
    let probe = paperwave::probe_system();

    if args.debug || args.detect_only {
        print_probe(&probe);
    }

    if args.detect_only {
        return;
    }

    if let Some(path) = args.image {
        if let Err(err) = run_image(&path, rotation, args.saturation, args.lighten, &probe) {
            eprintln!("Error: {err}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(err) = run_demo(rotation, args.saturation, args.lighten, &probe) {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("Inky display CLI can only run on Linux targets.");
}

#[cfg(target_os = "linux")]
fn run_web(host: &str, port: u16) -> paperwave::Result<()> {
    // Perform hardware probe once and launch async server with that context
    let probe = paperwave::probe_system();
    web::run_server(host.to_string(), port, probe)
}

#[cfg(target_os = "linux")]
fn run_demo(
    rotation: paperwave::Rotation,
    saturation: f32,
    lighten: f32,
    probe: &paperwave::ProbeInfo,
) -> paperwave::Result<()> {
    let mut display = create_display(rotation, probe)?;

    let (input_w, input_h) = display.input_dimensions();
    let mut image = RgbImage::new(input_w as u32, input_h as u32);

    let palette: Vec<Rgb<u8>> = match probe.display {
        Some(paperwave::DisplaySpec::El133Uf1 { .. }) => vec![
            Rgb([0, 0, 0]),
            Rgb([255, 255, 255]),
            Rgb([255, 255, 0]),
            Rgb([255, 0, 0]),
            Rgb([0, 0, 255]),
            Rgb([0, 255, 0]),
        ],
        _ => vec![
            Rgb([57, 48, 57]),
            Rgb([255, 255, 255]),
            Rgb([58, 91, 70]),
            Rgb([61, 59, 94]),
            Rgb([156, 72, 75]),
            Rgb([208, 190, 71]),
            Rgb([177, 106, 73]),
        ],
    };

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
    display.set_image(&dynamic, saturation, lighten)?;
    display.show()
}

#[cfg(target_os = "linux")]
fn create_display(
    rotation: paperwave::Rotation,
    probe: &paperwave::ProbeInfo,
) -> paperwave::Result<Box<dyn paperwave::InkyDisplay>> {
    use paperwave::InkyDisplay;

    match probe.display {
        Some(paperwave::DisplaySpec::El133Uf1 { width, height }) => {
            let mut config = paperwave::InkyEl133Uf1Config::default();
            config.width = width;
            config.height = height;
            config.rotation = rotation;
            let mut display = paperwave::InkyEl133Uf1::new(config)?;
            display.set_rotation(rotation);
            Ok(Box::new(display))
        }
        Some(paperwave::DisplaySpec::Uc8159 { width, height, .. }) => {
            let mut config = paperwave::InkyUc8159Config::default();
            config.width = width;
            config.height = height;
            config.rotation = rotation;
            let mut display = paperwave::InkyUc8159::new(config)?;
            display.set_rotation(rotation);
            Ok(Box::new(display))
        }
        None => {
            let mut config = paperwave::InkyUc8159Config::default();
            config.rotation = rotation;
            let mut display = paperwave::InkyUc8159::new(config)?;
            display.set_rotation(rotation);
            Ok(Box::new(display))
        }
    }
}

#[cfg(target_os = "linux")]
fn run_image(
    path: &PathBuf,
    rotation: paperwave::Rotation,
    saturation: f32,
    lighten: f32,
    probe: &paperwave::ProbeInfo,
) -> paperwave::Result<()> {
    let mut display = create_display(rotation, probe)?;
    display.set_image_from_path(path.as_path(), saturation, lighten)?;
    display.show()
}

#[cfg(target_os = "linux")]
fn print_probe(probe: &paperwave::ProbeInfo) {
    use paperwave::I2cProbeStatus;
    use std::fmt::Write as _;

    println!("== Probe Report ==");
    match (&probe.eeprom, &probe.eeprom_error) {
        (Some(info), _) => {
            if let Some(bus) = &probe.eeprom_bus {
                println!("EEPROM: {info} (via {})", bus.display());
            } else {
                println!("EEPROM: {info}");
            }
        }
        (None, Some(err)) => println!("EEPROM: error - {err}"),
        (None, None) => println!("EEPROM: not found"),
    }

    if let Some(spec) = &probe.display {
        println!("Display: {spec}");
    } else {
        println!("Display: not detected (fallback to 600x448)");
    }

    if probe.i2c_buses.is_empty() {
        println!("I2C buses: none detected");
    } else {
        let mut line = String::from("I2C buses:");
        for path in &probe.i2c_buses {
            let _ = write!(&mut line, " {}", path.display());
        }
        println!("{line}");
    }

    if !probe.i2c_bus_results.is_empty() {
        println!("I2C probe results:");
        for report in &probe.i2c_bus_results {
            match &report.status {
                I2cProbeStatus::Found(info) => {
                    println!("  {}: found {}", report.path.display(), info);
                }
                I2cProbeStatus::Blank => {
                    println!("  {}: blank/cleared (all 0x00/0xFF)", report.path.display());
                }
                I2cProbeStatus::Invalid(reason) => {
                    println!("  {}: invalid data ({})", report.path.display(), reason);
                }
                I2cProbeStatus::Unavailable => {
                    println!("  {}: no response / not available", report.path.display());
                }
                I2cProbeStatus::Error(err) => {
                    println!("  {}: error {}", report.path.display(), err);
                }
            }
        }
    }

    if probe.spi_devices.is_empty() {
        println!("SPI devices: none detected");
    } else {
        let mut line = String::from("SPI devices:");
        for path in &probe.spi_devices {
            let _ = write!(&mut line, " {}", path.display());
        }
        println!("{line}");
    }

    if probe.gpio_chips.is_empty() {
        println!("GPIO chips: none detected");
    } else {
        let mut line = String::from("GPIO chips:");
        for path in &probe.gpio_chips {
            let _ = write!(&mut line, " {}", path.display());
        }
        println!("{line}");

        if !probe.gpio_chip_labels.is_empty() {
            println!("GPIO labels:");
            for label in &probe.gpio_chip_labels {
                println!("  {label}");
            }
        }
    }
    println!();
}
