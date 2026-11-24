//! Rattonero Books for GNOME and other GTK desktops.
//!
//! Everything about books comes from `rattonero-core`; this crate is only
//! the user interface: libadwaita for the window, WebKitGTK for EPUB pages
//! (paginated by the core's script) and Poppler for PDFs.

mod appearance;
mod covers;
mod epub;
mod pdf;
mod prefs;
mod shelf;
mod store;
mod window;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

const APP_ID: &str = "com.rattonero.ebookreader";

/// The app's own icons, so the header looks the same under any icon theme.
/// The Flatpak installs them into /app/share/icons; a build run from the
/// source tree finds them here.
fn add_icons() {
    let icons = concat!(env!("CARGO_MANIFEST_DIR"), "/data/icons");
    if let Some(display) = gdk::Display::default() {
        if std::path::Path::new(icons).is_dir() {
            gtk::IconTheme::for_display(&display).add_search_path(icons);
        }
    }
}

fn main() -> glib::ExitCode {
    glib::set_application_name("Rattonero Books");
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::empty())
        .build();
    app.connect_startup(|app| {
        gtk::Window::set_default_icon_name(APP_ID);
        add_icons();
        load_css();
        install_app_actions(app);
    });
    app.connect_activate(|app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }
        let window = window::Window::new(app);
        let gtk_window = window.window.clone();
        // Signal handlers only hold weak references; this one keeps the
        // Rust side of the window alive until GTK destroys it.
        let keep = std::cell::RefCell::new(Some(window));
        gtk_window.connect_destroy(move |_| {
            keep.borrow_mut().take();
        });
        gtk_window.present();
    });
    app.run()
}

fn install_app_actions(app: &adw::Application) {
    let quit = gio::SimpleAction::new("quit", None);
    let weak = app.downgrade();
    quit.connect_activate(move |_, _| {
        if let Some(app) = weak.upgrade() {
            // Close windows properly so progress is shared before leaving.
            for window in app.windows() {
                window.close();
            }
        }
    });
    app.add_action(&quit);

    let about = gio::SimpleAction::new("about", None);
    let weak = app.downgrade();
    about.connect_activate(move |_, _| {
        let Some(app) = weak.upgrade() else { return };
        let dialog = adw::AboutDialog::builder()
            .application_name("Rattonero Books")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .comments("A small reader for EPUB and PDF books in a folder of your own")
            .license_type(gtk::License::Custom)
            .license("Rattonero Books is available under the MIT or the Apache 2.0 licence.")
            .build();
        dialog.present(app.active_window().as_ref());
    });
    app.add_action(&about);

    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
    app.set_accels_for_action("win.choose-folder", &["<Control>o"]);
    app.set_accels_for_action("win.refresh", &["<Control>r", "F5"]);
    app.set_accels_for_action("win.show-help-overlay", &["<Control>question"]);
    app.set_accels_for_action(
        "reader.bigger",
        &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
    );
    app.set_accels_for_action(
        "reader.smaller",
        &["<Control>minus", "<Control>KP_Subtract"],
    );
}

fn load_css() {
    let mut css = include_str!("style.css").to_string();
    // Cloth bindings for books without a cover, one colour per title.
    for i in 0..shelf::CLOTH_COLORS {
        let (r, g, b) = hsv_to_rgb(i as f64 / shelf::CLOTH_COLORS as f64, 0.42, 0.46);
        css.push_str(&format!(
            ".cloth-{i} {{ background-color: rgb({r}, {g}, {b}); }}\n"
        ));
    }
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let i = (h * 6.0).floor();
    let f = h * 6.0 - i;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i as i32 % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    (
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}
