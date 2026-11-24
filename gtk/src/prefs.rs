//! What the app remembers between launches: the library folder, the shelf
//! order, the reader's appearance and the window size. Kept as a small JSON
//! file in the user's configuration directory, so it works the same in the
//! Flatpak and in a plain `cargo run`.

use std::path::PathBuf;

use rattonero_core::ShelfOrder;
use serde::{Deserialize, Serialize};

pub const FONT_SIZES: (f64, f64) = (12.0, 34.0);
pub const LINE_HEIGHTS: (f64, f64) = (1.1, 2.2);
pub const MARGINS: (f64, f64) = (8.0, 96.0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Sepia,
    Dark,
}

impl Theme {
    pub const ALL: [Theme; 4] = [Theme::System, Theme::Light, Theme::Sepia, Theme::Dark];

    pub fn name(self) -> &'static str {
        match self {
            Theme::System => "system",
            Theme::Light => "light",
            Theme::Sepia => "sepia",
            Theme::Dark => "dark",
        }
    }

    pub fn from_name(name: &str) -> Option<Theme> {
        Theme::ALL.into_iter().find(|t| t.name() == name)
    }

    pub fn label(self) -> &'static str {
        match self {
            Theme::System => "Auto",
            Theme::Light => "Light",
            Theme::Sepia => "Sepia",
            Theme::Dark => "Dark",
        }
    }

    /// The scheme the window chrome takes while reading.
    pub fn color_scheme(self) -> adw::ColorScheme {
        match self {
            Theme::System => adw::ColorScheme::Default,
            Theme::Light | Theme::Sepia => adw::ColorScheme::ForceLight,
            Theme::Dark => adw::ColorScheme::ForceDark,
        }
    }

    /// Name understood by the core's reader stylesheet.
    pub fn css_name(self, system_dark: bool) -> &'static str {
        match self {
            Theme::System if system_dark => "dark",
            Theme::System => "light",
            other => other.name(),
        }
    }

    /// Page colour; keep in sync with `core/src/reader.css`.
    pub fn background(self, system_dark: bool) -> gtk::gdk::RGBA {
        let (r, g, b) = match self.css_name(system_dark) {
            "dark" => (0x1c, 0x1c, 0x1e),
            "sepia" => (0xf4, 0xec, 0xd8),
            _ => (0xff, 0xff, 0xff),
        };
        gtk::gdk::RGBA::new(r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0)
    }
}

pub fn order_name(order: ShelfOrder) -> &'static str {
    match order {
        ShelfOrder::Author => "author",
        ShelfOrder::Title => "title",
        ShelfOrder::Publisher => "publisher",
        ShelfOrder::Year => "year",
    }
}

pub fn order_label(order: ShelfOrder) -> &'static str {
    match order {
        ShelfOrder::Author => "Author",
        ShelfOrder::Title => "Title",
        ShelfOrder::Publisher => "Publisher",
        ShelfOrder::Year => "Year",
    }
}

pub const ORDERS: [ShelfOrder; 4] = [
    ShelfOrder::Author,
    ShelfOrder::Title,
    ShelfOrder::Publisher,
    ShelfOrder::Year,
];

pub fn order_from_name(name: &str) -> Option<ShelfOrder> {
    ORDERS.into_iter().find(|o| order_name(*o) == name)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    pub library_folder: Option<String>,
    pub order: String,
    pub theme: Theme,
    pub font_size: f64,
    pub line_height: f64,
    pub margin: f64,
    pub window_width: i32,
    pub window_height: i32,
    pub maximized: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs {
            library_folder: None,
            order: "author".into(),
            theme: Theme::System,
            font_size: 18.0,
            line_height: 1.5,
            margin: 28.0,
            window_width: 1040,
            window_height: 760,
            maximized: false,
        }
    }
}

fn clamp(value: f64, (low, high): (f64, f64), fallback: f64) -> f64 {
    if value.is_finite() && (low..=high).contains(&value) {
        value
    } else {
        fallback
    }
}

impl Prefs {
    fn path() -> PathBuf {
        gtk::glib::user_config_dir()
            .join("rattonero")
            .join("settings.json")
    }

    pub fn load() -> Prefs {
        let mut prefs: Prefs = std::fs::read(Self::path())
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
            .unwrap_or_default();
        let defaults = Prefs::default();
        prefs.font_size = clamp(prefs.font_size, FONT_SIZES, defaults.font_size);
        prefs.line_height = clamp(prefs.line_height, LINE_HEIGHTS, defaults.line_height);
        prefs.margin = clamp(prefs.margin, MARGINS, defaults.margin);
        if prefs.window_width < 360 || prefs.window_height < 360 {
            prefs.window_width = defaults.window_width;
            prefs.window_height = defaults.window_height;
        }
        prefs
    }

    pub fn save(&self) {
        let path = Self::path();
        let Some(dir) = path.parent() else { return };
        let Ok(data) = serde_json::to_vec_pretty(self) else {
            return;
        };
        let tmp = path.with_extension("tmp");
        // Losing a preference is not worth bothering the reader about.
        let _ = std::fs::create_dir_all(dir)
            .and_then(|_| std::fs::write(&tmp, data))
            .and_then(|_| std::fs::rename(&tmp, &path));
    }

    pub fn order(&self) -> ShelfOrder {
        order_from_name(&self.order).unwrap_or(ShelfOrder::Author)
    }

    /// Settings in the form the pagination script expects.
    pub fn reader_json(&self, system_dark: bool) -> String {
        format!(
            "{{\"theme\":\"{}\",\"fontSize\":{},\"lineHeight\":{:.2},\"margin\":{}}}",
            self.theme.css_name(system_dark),
            self.font_size.round() as i64,
            self.line_height,
            self.margin.round() as i64
        )
    }
}
