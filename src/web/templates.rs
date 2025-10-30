use askama::Template;

#[derive(Template)]
#[template(path = "index.html", escape = "none")]
pub struct IndexTemplate {
    pub width: u16,
    pub height: u16,
    pub aspect_str: String,
    pub disabled_attr: String,
    pub hint_text: String,
    pub portrait: bool,
    pub land_checked: String,
    pub port_checked: String,
    pub calib_deg: u16,
    pub is_spectra6: bool,
}
