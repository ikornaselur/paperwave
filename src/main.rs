#[cfg(target_os = "linux")]
fn main() {
    if let Err(err) = run_demo() {
        eprintln!("Error: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("UC8159 demo can only run on Linux targets.");
}

#[cfg(target_os = "linux")]
fn run_demo() -> inkwell::uc8159::Result<()> {
    use inkwell::{InkyUc8159, InkyUc8159Config};

    let mut display = InkyUc8159::new(InkyUc8159Config::default())?;

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
