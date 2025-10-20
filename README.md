# Paperwave

Paperwave is a Rust CLI for working with Inky e-paper displays. It can probe the
attached hardware, render demo patterns, and show images with palette-aware
resizing, rotation, and dithering.

## Features

- Detects connected displays and reports EEPROM metadata for quick diagnostics.
- Displays PNG images, resizing to the panel while preserving aspect ratio.
- Applies palette-aware Floyd–Steinberg dithering with adjustable saturation.
- Provides a colour stripe demo to validate panel output without an image.
- Supports four rotation angles to match display orientation at runtime.

## Supported Displays

- Pimoroni Inky Impression 5.7" (UC8159 controller).
- Pimoroni Inky Impression 13.3" (Spectra 6 / EL133UF1) — initial implementation.

## Usage

1. Build the project with `cargo build --release`.
2. Run the binary on a system with access to the required SPI, GPIO, and I2C
   interfaces.
3. Supply a PNG to render or use the built-in demo stripes.

Example commands:

```sh
# Probe the system without updating the display
paperwave --detect-only --debug

# Display an image with custom rotation and saturation
paperwave --rotate 90 --saturation 0.6 path/to/image.png
```

## Command-Line Reference

```
CLI tool to display images on Inky displays

Usage: paperwave [OPTIONS] [IMAGE]

Arguments:
  [IMAGE]  Optional PNG to display

Options:
  -s, --saturation <SAT>   Palette saturation from 0.0 (desaturated) to 1.0 (saturated) [default: 0.5]
  -r, --rotate <ROTATION>  Rotate image before display (degrees clockwise) [default: 0] [possible values: 0, 90, 180, 270]
      --detect-only        Probe hardware and report detection results without updating the panel
      --debug              Print probe/debug information before running
  -h, --help               Print help
```
