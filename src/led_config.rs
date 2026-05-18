use serde::Deserialize;
use std::fs;

type Rgb = [u8; 3];

#[derive(Debug, Clone, Deserialize)]
pub enum LedMode {
    /// Static color(s); always exactly 4 entries, missing entries repeat the last value.
    Static { colors: Vec<Rgb> },
}

#[derive(Debug, Clone, Deserialize)]
pub struct LedConfig {
    /// `None` — no LED command will be sent, device keeps its current state
    pub mode: Option<LedMode>,
    /// LED brightness 0-100
    #[serde(default = "default_brightness")]
    pub brightness: u8,
}

fn default_brightness() -> u8 {
    100
}

/// Extends `colors` to `count` entries by repeating the last value.
/// Returns `None` if the input is empty.
fn fill_colors(colors: &mut Vec<Rgb>, count: usize) {
    if let Some(&last) = colors.last() {
        colors.resize(count, last);
    }
}

impl LedConfig {
    /// Normalizes the config after deserialization:
    /// fills missing LED colors and clears the mode if colors is empty.
    fn resolved_colors(mut self) -> Self {
        if let Some(LedMode::Static { ref mut colors }) = self.mode {
            if colors.is_empty() {
                self.mode = None;
            } else {
                fill_colors(colors, 4);
            }
        }
        self
    }
}

pub fn load() -> LedConfig {
    let no_change = LedConfig {
        mode: None,
        brightness: default_brightness(),
    };

    let Some(path) = dirs_config_path() else {
        return no_change;
    };

    let Ok(contents) = fs::read_to_string(&path) else {
        return no_change;
    };

    match toml::from_str::<LedConfig>(&contents) {
        Ok(cfg) => cfg.resolved_colors(),
        Err(e) => {
            log::warn!("Failed to parse LED config, making no LED changes: {e}");
            no_change
        }
    }
}

fn dirs_config_path() -> Option<std::path::PathBuf> {
    Some(dirs::config_dir()?.join("opendeck-stream-dock-xl").join("leds.toml"))
}