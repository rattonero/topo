//! PDFs, a page at a time, drawn by Poppler; the core only keeps the place.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use rattonero_core::{BookInfo, Library, ReadingPosition};

/// Room around the page, so it reads as a sheet of paper.
const MARGIN: f64 = 16.0;

pub struct PdfReader {
    pub page: adw::NavigationPage,
    info: BookInfo,
    library: Option<Arc<Library>>,
    document: poppler::Document,
    count: i32,
    index: Cell<i32>,
    area: gtk::DrawingArea,
    page_label: gtk::Label,
    wheel_timer: RefCell<Option<glib::SourceId>>,
    wheel_sum: Cell<f64>,
    wheel_locked: Cell<bool>,
}

impl PdfReader {
    pub fn new(info: BookInfo, library: Option<Arc<Library>>) -> Result<Rc<PdfReader>, String> {
        let uri = glib::filename_to_uri(&info.path, None).map_err(|e| e.to_string())?;
        let document =
            poppler::Document::from_file(&uri, None).map_err(|e| e.message().to_string())?;
        let count = document.n_pages();
        if count < 1 {
            return Err("The document has no pages.".into());
        }
        let saved = library
            .as_ref()
            .and_then(|l| l.position(info.id.clone()))
            .map(|p| p.index as i32)
            .unwrap_or(0)
            .clamp(0, count - 1);

        let reader = Rc::new(PdfReader {
            page: adw::NavigationPage::builder()
                .title(&info.title)
                .tag("reader")
                .build(),
            info,
            library,
            document,
            count,
            index: Cell::new(saved),
            area: gtk::DrawingArea::builder()
                .hexpand(true)
                .vexpand(true)
                .focusable(true)
                .build(),
            page_label: gtk::Label::builder().css_classes(["numeric"]).build(),
            wheel_timer: RefCell::new(None),
            wheel_sum: Cell::new(0.0),
            wheel_locked: Cell::new(false),
        });
        reader.build_ui();
        reader.show(saved);
        Ok(reader)
    }

    fn build_ui(self: &Rc<Self>) {
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new(
                &self.info.title,
                &crate::shelf::author_line(&self.info),
            ))
            .build();
        let turns = gtk::Box::builder().css_classes(["linked"]).build();
        for (icon, tooltip, action) in [
            (
                "rattonero-page-previous-symbolic",
                "Previous Page",
                "reader.previous",
            ),
            ("rattonero-page-next-symbolic", "Next Page", "reader.next"),
        ] {
            turns.append(
                &gtk::Button::builder()
                    .icon_name(icon)
                    .tooltip_text(tooltip)
                    .action_name(action)
                    .focus_on_click(false)
                    .build(),
            );
        }
        header.pack_start(&turns);

        let weak = Rc::downgrade(self);
        self.area.set_draw_func(move |_, cr, width, height| {
            if let Some(reader) = weak.upgrade() {
                reader.draw(cr, width as f64, height as f64);
            }
        });

        // Clicks on the outer thirds turn the page, as in the EPUB reader.
        let click = gtk::GestureClick::new();
        let weak = Rc::downgrade(self);
        click.connect_released(move |gesture, _, x, _| {
            let Some(reader) = weak.upgrade() else { return };
            reader.area.grab_focus();
            let width = gesture.widget().map(|w| w.width()).unwrap_or(1).max(1) as f64;
            if x < width * 0.3 {
                reader.turn(-1);
            } else if x > width * 0.7 {
                reader.turn(1);
            }
        });
        self.area.add_controller(click);

        // Swipe left and right on touch screens.
        let swipe = gtk::GestureSwipe::builder().touch_only(true).build();
        let weak = Rc::downgrade(self);
        swipe.connect_swipe(move |_, vx, vy| {
            if let Some(reader) = weak.upgrade() {
                if vx.abs() > vy.abs() && vx.abs() > 300.0 {
                    reader.turn(if vx < 0.0 { 1 } else { -1 });
                }
            }
        });
        self.area.add_controller(swipe);

        // The mouse wheel and trackpad: one gesture, one page.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = Rc::downgrade(self);
        scroll.connect_scroll(move |controller, dx, dy| {
            let Some(reader) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            let delta = if dx.abs() > dy.abs() { dx } else { dy };
            let wheel = controller.unit() == gdk::ScrollUnit::Wheel;
            reader.wheel(if wheel { delta * 50.0 } else { delta });
            glib::Propagation::Stop
        });
        self.area.add_controller(scroll);

        let keys = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let Some(reader) = weak.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if modifiers.intersects(
                gdk::ModifierType::CONTROL_MASK
                    | gdk::ModifierType::ALT_MASK
                    | gdk::ModifierType::SUPER_MASK,
            ) {
                return glib::Propagation::Proceed;
            }
            let shift = modifiers.contains(gdk::ModifierType::SHIFT_MASK);
            use gdk::Key;
            match key {
                Key::Right | Key::Down | Key::Page_Down | Key::KP_Page_Down => reader.turn(1),
                Key::space if !shift => reader.turn(1),
                Key::Left | Key::Up | Key::Page_Up | Key::KP_Page_Up => reader.turn(-1),
                Key::space => reader.turn(-1),
                Key::Home => reader.show(0),
                Key::End => reader.show(reader.count - 1),
                _ => return glib::Propagation::Proceed,
            }
            glib::Propagation::Stop
        });
        self.area.add_controller(keys);

        let footer = gtk::Box::builder()
            .margin_start(20)
            .margin_end(20)
            .margin_top(4)
            .margin_bottom(6)
            .halign(gtk::Align::End)
            .css_classes(["reader-footer", "dim-label"])
            .build();
        footer.append(&self.page_label);

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["pdf-canvas"])
            .build();
        content.append(&self.area);
        content.append(&footer);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&content));
        self.page.set_child(Some(&view));

        let actions = gio::SimpleActionGroup::new();
        for (name, step) in [("previous", -1), ("next", 1)] {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(reader) = weak.upgrade() {
                    reader.turn(step);
                }
            });
            actions.add_action(&action);
        }
        self.page.insert_action_group("reader", Some(&actions));

        let area = self.area.downgrade();
        self.page.connect_shown(move |_| {
            if let Some(area) = area.upgrade() {
                area.grab_focus();
            }
        });
    }

    fn wheel(self: &Rc<Self>, delta: f64) {
        if !self.wheel_locked.get() {
            let sum = self.wheel_sum.get() + delta;
            if sum.abs() > 40.0 {
                self.wheel_locked.set(true);
                self.wheel_sum.set(0.0);
                self.turn(if sum > 0.0 { 1 } else { -1 });
            } else {
                self.wheel_sum.set(sum);
            }
        }
        // The gesture ends when the events stop for a moment.
        if let Some(timer) = self.wheel_timer.borrow_mut().take() {
            timer.remove();
        }
        let weak = Rc::downgrade(self);
        let timer = glib::timeout_add_local_once(Duration::from_millis(160), move || {
            if let Some(reader) = weak.upgrade() {
                reader.wheel_timer.borrow_mut().take();
                reader.wheel_sum.set(0.0);
                reader.wheel_locked.set(false);
            }
        });
        *self.wheel_timer.borrow_mut() = Some(timer);
    }

    fn turn(&self, step: i32) {
        let target = self.index.get() + step;
        if (0..self.count).contains(&target) {
            self.show(target);
        }
    }

    fn show(&self, index: i32) {
        let index = index.clamp(0, self.count - 1);
        self.index.set(index);
        self.page_label
            .set_label(&format!("{} of {}", index + 1, self.count));
        self.area.queue_draw();
        if let Some(library) = &self.library {
            let position = ReadingPosition {
                index: index as u32,
                fraction: 0.0,
                progress: (index + 1) as f64 / self.count as f64,
                finished: index >= self.count - 1,
            };
            library.set_position(self.info.id.clone(), position);
        }
    }

    fn draw(&self, cr: &gtk::cairo::Context, width: f64, height: f64) {
        let Some(page) = self.document.page(self.index.get()) else {
            return;
        };
        let (page_width, page_height) = page.size();
        if page_width <= 0.0 || page_height <= 0.0 {
            return;
        }
        let scale = ((width - 2.0 * MARGIN) / page_width)
            .min((height - 2.0 * MARGIN) / page_height)
            .max(0.01);
        let (w, h) = (page_width * scale, page_height * scale);
        let (x, y) = (((width - w) / 2.0).round(), ((height - h) / 2.0).round());
        // A soft shadow, then the sheet, then the page itself.
        cr.set_source_rgba(0.0, 0.0, 0.0, 0.18);
        cr.rectangle(x + 1.0, y + 2.0, w, h);
        let _ = cr.fill();
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.rectangle(x, y, w, h);
        let _ = cr.fill();
        cr.translate(x, y);
        cr.rectangle(0.0, 0.0, w, h);
        cr.clip();
        cr.scale(scale, scale);
        page.render(cr);
    }
}
