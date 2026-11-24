//! Theme, text size, margins and line spacing: the reader's few controls.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;

use crate::prefs::{Prefs, Theme, FONT_SIZES, LINE_HEIGHTS, MARGINS};

/// A popover editing `prefs`; `changed` runs after every change.
pub fn popover(prefs: &Rc<RefCell<Prefs>>, changed: Rc<dyn Fn()>) -> gtk::Popover {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(16)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .width_request(280)
        .build();

    let themes = adw::ToggleGroup::new();
    for theme in Theme::ALL {
        themes.add(
            adw::Toggle::builder()
                .name(theme.name())
                .label(theme.label())
                .build(),
        );
    }
    themes.set_active_name(Some(prefs.borrow().theme.name()));
    {
        let prefs = prefs.clone();
        let changed = changed.clone();
        themes.connect_active_name_notify(move |group| {
            let Some(theme) = group.active_name().and_then(|n| Theme::from_name(&n)) else {
                return;
            };
            prefs.borrow_mut().theme = theme;
            changed();
        });
    }
    content.append(&themes);

    type Field = fn(&mut Prefs) -> &mut f64;
    let rows: [(&str, (f64, f64), f64, Field, fn(f64) -> String); 3] = [
        (
            "Text size",
            FONT_SIZES,
            1.0,
            |p| &mut p.font_size,
            |v| format!("{v:.0} px"),
        ),
        (
            "Margins",
            MARGINS,
            4.0,
            |p| &mut p.margin,
            |v| format!("{v:.0}"),
        ),
        (
            "Line spacing",
            LINE_HEIGHTS,
            0.1,
            |p| &mut p.line_height,
            |v| format!("{v:.1}"),
        ),
    ];
    for (title, (low, high), step, field, format) in rows {
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .build();
        let heading = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        heading.append(
            &gtk::Label::builder()
                .label(title)
                .hexpand(true)
                .xalign(0.0)
                .build(),
        );
        let value = gtk::Label::builder()
            .css_classes(["dim-label", "numeric"])
            .build();
        heading.append(&value);
        row.append(&heading);

        let current = *field(&mut prefs.borrow_mut());
        value.set_label(&format(current));
        let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, low, high, step);
        scale.set_value(current);
        scale.set_draw_value(false);
        scale.update_property(&[gtk::accessible::Property::Label(title)]);
        let prefs = prefs.clone();
        let changed = changed.clone();
        scale.connect_value_changed(move |scale| {
            // Snap to the step, so the page only reflows for real changes.
            let snapped = ((scale.value() / step).round() * step).clamp(low, high);
            let mut p = prefs.borrow_mut();
            let slot = field(&mut p);
            if (*slot - snapped).abs() < step / 2.0 {
                return;
            }
            *slot = snapped;
            drop(p);
            value.set_label(&format(snapped));
            changed();
        });
        row.append(&scale);
        content.append(&row);
    }

    gtk::Popover::builder().child(&content).build()
}
