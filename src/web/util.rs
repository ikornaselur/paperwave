use image::DynamicImage;
use paperwave::InkyDisplay;
use std::path::PathBuf;

pub fn combine_rotation(calibrated: Option<paperwave::Rotation>, user_deg: u16) -> paperwave::Rotation {
    let base_deg = match calibrated.unwrap_or(paperwave::Rotation::Deg0) {
        paperwave::Rotation::Deg0 => 0u16,
        paperwave::Rotation::Deg90 => 90,
        paperwave::Rotation::Deg180 => 180,
        paperwave::Rotation::Deg270 => 270,
    };
    let total = (base_deg
        + match user_deg % 360 { 0=>0, 90=>90, 180=>180, 270=>270, _=>0 }) % 360;
    match total { 0=>paperwave::Rotation::Deg0, 90=>paperwave::Rotation::Deg90, 180=>paperwave::Rotation::Deg180, 270=>paperwave::Rotation::Deg270, _=>paperwave::Rotation::Deg0 }
}

pub fn create_display_from_probe(
    rotation: paperwave::Rotation,
    probe: &paperwave::ProbeInfo,
) -> paperwave::Result<Box<dyn paperwave::InkyDisplay>> {
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

pub fn arrow_image(dim: (u16, u16), dir: char) -> DynamicImage {
    use image::{imageops, Rgb, RgbImage};
    let (w, h) = (dim.0 as u32, dim.1 as u32);
    let mut img = RgbImage::from_pixel(w, h, Rgb([255, 255, 255]));
    let cx = (w / 2) as i32;
    let tip_y = (h as f32 * 0.12) as i32;
    let base_y = (h as f32 * 0.62) as i32;
    let max_half = ((w as f32) * 0.35) as i32;
    let red = Rgb([200, 20, 20]);

    for y in tip_y..=base_y {
        let t = (y - tip_y) as f32 / (base_y - tip_y).max(1) as f32;
        let half = (max_half as f32 * t) as i32;
        let x0 = (cx - half).max(0) as u32;
        let x1 = (cx + half).min(w as i32 - 1) as u32;
        for x in x0..=x1 { img.put_pixel(x, y as u32, red); }
    }
    let shaft_w = (w as f32 * 0.10).max(1.0) as i32;
    let shaft_y0 = base_y;
    let shaft_y1 = (h as f32 * 0.90) as i32;
    let x0 = (cx - shaft_w / 2).max(0) as u32;
    let x1 = (cx + shaft_w / 2).min(w as i32 - 1) as u32;
    for y in shaft_y0..=shaft_y1 { for x in x0..=x1 { img.put_pixel(x, y as u32, red); } }

    let r#dyn = DynamicImage::ImageRgb8(img);
    match dir {
        'U' => r#dyn,
        'R' => DynamicImage::ImageRgba8(imageops::rotate90(&r#dyn)),
        'D' => DynamicImage::ImageRgba8(imageops::rotate180(&r#dyn)),
        'L' => DynamicImage::ImageRgba8(imageops::rotate270(&r#dyn)),
        _ => r#dyn,
    }
}

fn config_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/paperwave");
        let _ = std::fs::create_dir_all(&p);
        p.push("state.json");
        return p;
    }
    PathBuf::from("paperwave_state.json")
}

fn portrait_path() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        let mut p = PathBuf::from(home);
        p.push(".config/paperwave");
        let _ = std::fs::create_dir_all(&p);
        p.push("portrait.txt");
        return p;
    }
    PathBuf::from("paperwave_portrait.txt")
}

pub fn load_saved_portrait() -> Option<bool> {
    let p = portrait_path();
    let s = std::fs::read_to_string(p).ok()?;
    Some(s.trim() == "true")
}

pub fn save_portrait(portrait: bool) -> std::io::Result<()> {
    std::fs::write(portrait_path(), if portrait { "true" } else { "false" })
}

pub fn load_saved_rotation() -> Option<paperwave::Rotation> {
    #[derive(serde::Deserialize)]
    struct State { rotation_deg: u16 }
    let path = config_path();
    let data = std::fs::read(path).ok()?;
    let st: State = serde_json::from_slice(&data).ok()?;
    match st.rotation_deg % 360 { 0=>Some(paperwave::Rotation::Deg0), 90=>Some(paperwave::Rotation::Deg90), 180=>Some(paperwave::Rotation::Deg180), 270=>Some(paperwave::Rotation::Deg270), _=>None }
}

pub fn save_rotation(rot: paperwave::Rotation) -> std::io::Result<()> {
    #[derive(serde::Serialize)]
    struct State { rotation_deg: u16 }
    let deg = match rot { paperwave::Rotation::Deg0=>0, paperwave::Rotation::Deg90=>90, paperwave::Rotation::Deg180=>180, paperwave::Rotation::Deg270=>270 };
    let path = config_path();
    let data = serde_json::to_vec_pretty(&State { rotation_deg: deg }).unwrap();
    std::fs::write(path, data)
}
