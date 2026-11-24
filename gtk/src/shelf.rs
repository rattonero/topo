//! The bookshelf: covers standing on wooden boards, captions beneath, one
//! list row per shelf so that only the shelves on screen are built.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::prelude::*;
use gtk::{gio, glib};
use rattonero_core::{BookFormat, BookInfo};

use crate::covers::Covers;
use crate::store::Store;

/// Covers are this wide when there is room, and shrink so that a shelf
/// always holds at least three books.
const FULL_COVER_WIDTH: f64 = 112.0;
const CELL_WIDTH: f64 = 132.0;
const MIN_COLUMNS: usize = 3;
const SIDE_PADDING: f64 = 24.0;

#[derive(Clone)]
struct Place {
    book: BookInfo,
    /// Set on the first book of a group: "C", "Einaudi", "1957".
    label: Option<String>,
}

#[derive(Clone)]
struct Row {
    places: Vec<Place>,
    /// The "Continue reading" shelf, whose books can be taken off it.
    continuing: bool,
}

pub struct ShelfView {
    pub widget: gtk::ScrolledWindow,
    model: gio::ListStore,
    columns: Cell<usize>,
    cover_width: Cell<f64>,
    rebuild_pending: Cell<bool>,
    store: Rc<Store>,
    covers: Rc<Covers>,
    open: RefCell<Option<Rc<dyn Fn(BookInfo)>>>,
}

pub fn author_line(book: &BookInfo) -> String {
    book.authors.join(", ")
}

impl ShelfView {
    pub fn new(store: Rc<Store>, covers: Rc<Covers>) -> Rc<ShelfView> {
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let factory = gtk::SignalListItemFactory::new();
        let list = gtk::ListView::builder()
            .model(&gtk::NoSelection::new(Some(model.clone())))
            .factory(&factory)
            .css_classes(["shelf-list"])
            .build();
        let widget = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&list)
            .css_classes(["shelf-wall"])
            .build();
        let view = Rc::new(ShelfView {
            widget,
            model,
            columns: Cell::new(0),
            cover_width: Cell::new(FULL_COVER_WIDTH),
            rebuild_pending: Cell::new(false),
            store,
            covers,
            open: RefCell::new(None),
        });

        let weak = Rc::downgrade(&view);
        factory.connect_setup(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            item.set_activatable(false);
            item.set_selectable(false);
            item.set_focusable(false);
        });
        factory.connect_bind(move |_, item| {
            let Some(view) = weak.upgrade() else { return };
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            let Some(object) = item.item().and_downcast::<glib::BoxedAnyObject>() else {
                return;
            };
            let row = object.borrow::<Row>();
            item.set_child(Some(&view.row_widget(&row)));
        });
        factory.connect_unbind(|_, item| {
            let item = item.downcast_ref::<gtk::ListItem>().expect("a list item");
            item.set_child(None::<&gtk::Widget>);
        });

        // The viewport's width decides how many books go on a shelf.
        let weak = Rc::downgrade(&view);
        view.widget
            .hadjustment()
            .connect_page_size_notify(move |adjustment| {
                if let Some(view) = weak.upgrade() {
                    view.set_width(adjustment.page_size());
                }
            });

        // "Remove from Continue Reading", from a book's context menu.
        let actions = gio::SimpleActionGroup::new();
        let dismiss = gio::SimpleAction::new("dismiss", Some(glib::VariantTy::STRING));
        let weak = Rc::downgrade(&view);
        dismiss.connect_activate(move |_, parameter| {
            if let (Some(view), Some(id)) = (weak.upgrade(), parameter.and_then(|p| p.str())) {
                view.store.dismiss_from_continue_reading(id);
            }
        });
        actions.add_action(&dismiss);
        list.insert_action_group("shelf", Some(&actions));
        view
    }

    pub fn connect_open(&self, f: impl Fn(BookInfo) + 'static) {
        *self.open.borrow_mut() = Some(Rc::new(f));
    }

    fn set_width(self: &Rc<Self>, width: f64) {
        // The viewport passes through tiny sizes while the window maps;
        // the window itself is never narrower than 360 pixels.
        if width < 300.0 {
            return;
        }
        let usable = width - SIDE_PADDING;
        let columns = MIN_COLUMNS.max((usable / CELL_WIDTH) as usize);
        let cover_width = FULL_COVER_WIDTH
            .min(usable / columns as f64 - 14.0)
            .floor()
            .max(40.0);
        if columns != self.columns.get() || cover_width != self.cover_width.get() {
            self.columns.set(columns);
            self.cover_width.set(cover_width);
            // The width arrives while GTK lays the window out. Rows added
            // now would be placed before they are measured, at no size at
            // all; build them once this layout pass is over.
            if !self.rebuild_pending.replace(true) {
                let weak = Rc::downgrade(self);
                glib::idle_add_local_once(move || {
                    if let Some(view) = weak.upgrade() {
                        view.rebuild_pending.set(false);
                        view.rebuild();
                    }
                });
            }
        }
    }

    /// Lay the books out again: after a scan, a change of order or width,
    /// or when reading progress moved.
    pub fn rebuild(&self) {
        let columns = self.columns.get();
        if columns == 0 {
            return; // not yet allocated; `set_width` will call back
        }
        let mut rows = Vec::new();
        let continuing = self.store.continue_reading();
        if !continuing.is_empty() {
            let places = continuing
                .into_iter()
                .take(columns)
                .enumerate()
                .map(|(i, book)| Place {
                    book,
                    label: (i == 0).then(|| "Continue reading".to_string()),
                })
                .collect();
            rows.push(Row {
                places,
                continuing: true,
            });
        }
        // Books run on from shelf to shelf, as in a library; the first book
        // of each group carries the group's label.
        let places: Vec<Place> = self
            .store
            .shelves()
            .into_iter()
            .flat_map(|shelf| {
                let label = shelf.label;
                shelf
                    .books
                    .into_iter()
                    .enumerate()
                    .map(move |(i, book)| Place {
                        book,
                        label: (i == 0).then(|| label.clone()),
                    })
            })
            .collect();
        for chunk in places.chunks(columns) {
            rows.push(Row {
                places: chunk.to_vec(),
                continuing: false,
            });
        }
        let objects: Vec<glib::BoxedAnyObject> =
            rows.into_iter().map(glib::BoxedAnyObject::new).collect();
        // Replacing the rows would scroll back to the top; stay where the
        // reader was.
        let adjustment = self.widget.vadjustment();
        let scrolled = adjustment.value();
        self.model.splice(0, self.model.n_items(), &objects);
        if scrolled > 0.0 {
            glib::idle_add_local_once(move || adjustment.set_value(scrolled));
        }
    }

    fn row_widget(&self, row: &Row) -> gtk::Widget {
        let columns = self.columns.get().max(row.places.len());
        let cover_width = self.cover_width.get();
        let shelf = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .margin_start(12)
            .margin_end(12)
            .margin_bottom(6)
            .build();

        let standing = gtk::Box::builder()
            .homogeneous(true)
            .valign(gtk::Align::End)
            .margin_top(16)
            .build();
        let captions = gtk::Box::builder().homogeneous(true).margin_top(8).build();
        for place in &row.places {
            standing.append(&self.standing_book(place, cover_width, row.continuing));
            captions.append(&self.caption(&place.book, cover_width));
        }
        // Blanks keep short shelves aligned with the full ones.
        for _ in row.places.len()..columns {
            standing.append(&gtk::Box::new(gtk::Orientation::Vertical, 0));
            captions.append(&gtk::Box::new(gtk::Orientation::Vertical, 0));
        }

        let board = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["board"])
            .build();
        board.append(&gtk::Box::builder().css_classes(["board-top"]).build());
        board.append(&gtk::Box::builder().css_classes(["board-front"]).build());

        shelf.append(&standing);
        shelf.append(&board);
        shelf.append(&captions);
        shelf.upcast()
    }

    fn standing_book(&self, place: &Place, cover_width: f64, continuing: bool) -> gtk::Widget {
        let book = &place.book;
        let cell = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::End)
            .halign(gtk::Align::Center)
            .build();
        // The little label a library puts at the start of a run of books.
        let label = gtk::Label::builder()
            .label(place.label.as_deref().unwrap_or(" "))
            .halign(gtk::Align::Center)
            .ellipsize(ellipsis(place.label.as_deref().unwrap_or("")))
            .max_width_chars(16)
            .margin_bottom(6)
            .css_classes(["shelf-label"])
            .build();
        if place.label.is_none() {
            label.set_opacity(0.0);
            label.set_accessible_role(gtk::AccessibleRole::Presentation);
        }
        cell.append(&label);

        let button = gtk::Button::builder()
            .child(&self.cover(book, cover_width))
            .tooltip_text(tooltip(book))
            .css_classes(["cover-button", "flat"])
            .halign(gtk::Align::Center)
            .build();
        button.update_property(&[gtk::accessible::Property::Label(&format!(
            "{}, {}",
            book.title,
            author_line(book)
        ))]);
        let open = self.open.borrow().clone();
        let chosen = book.clone();
        button.connect_clicked(move |_| {
            if let Some(open) = &open {
                open(chosen.clone());
            }
        });
        if continuing {
            let menu = gio::Menu::new();
            let item = gio::MenuItem::new(Some("Remove from Continue Reading"), None);
            item.set_action_and_target_value(Some("shelf.dismiss"), Some(&book.id.to_variant()));
            menu.append_item(&item);
            let popover = gtk::PopoverMenu::from_model(Some(&menu));
            popover.set_parent(&button);
            popover.set_has_arrow(false);
            let show = {
                let popover = popover.clone();
                move || popover.popup()
            };
            let click = gtk::GestureClick::builder()
                .button(gtk::gdk::BUTTON_SECONDARY)
                .build();
            let show_click = show.clone();
            click.connect_pressed(move |gesture, _, _, _| {
                gesture.set_state(gtk::EventSequenceState::Claimed);
                show_click();
            });
            button.add_controller(click);
            let press = gtk::GestureLongPress::builder().touch_only(true).build();
            press.connect_pressed(move |_, _, _| show());
            button.add_controller(press);
            // The popover is not a child the button knows about; unparent it
            // with the button.
            button.connect_destroy(move |_| popover.unparent());
        }
        cell.append(&button);
        cell.upcast()
    }

    /// A book standing on the shelf, cover out; a cloth binding with the
    /// title until (or unless) a cover image turns up.
    fn cover(&self, book: &BookInfo, width: f64) -> gtk::Widget {
        let height = (width * 164.0 / 112.0).round();
        let slot = gtk::Box::builder()
            .width_request(width as i32)
            .height_request(height as i32)
            .valign(gtk::Align::End)
            .build();
        let binding = gtk::Overlay::builder()
            .halign(gtk::Align::Center)
            .valign(gtk::Align::End)
            .hexpand(true)
            .overflow(gtk::Overflow::Hidden)
            .css_classes(["cover"])
            .build();
        binding.set_child(Some(&placeholder(book, width, height)));
        // The hint of a spine.
        binding.add_overlay(
            &gtk::Box::builder()
                .halign(gtk::Align::Start)
                .css_classes(["spine"])
                .can_target(false)
                .build(),
        );
        if book.format == BookFormat::Pdf {
            binding.add_overlay(
                &gtk::Label::builder()
                    .label("PDF")
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .margin_top(4)
                    .margin_end(4)
                    .css_classes(["badge"])
                    .can_target(false)
                    .build(),
            );
        }
        slot.append(&binding);

        if let Some(library) = self.store.library() {
            let weak = binding.downgrade();
            self.covers.get(book, &library, move |texture| {
                let Some(binding) = weak.upgrade() else {
                    return;
                };
                let (tw, th) = (texture.width() as f64, texture.height() as f64);
                // Fit inside the slot, standing on the board.
                let scale = (width / tw).min(height / th);
                let picture = gtk::Picture::builder()
                    .paintable(texture)
                    .content_fit(gtk::ContentFit::Fill)
                    .can_shrink(true)
                    .width_request((tw * scale).round() as i32)
                    .height_request((th * scale).round() as i32)
                    .build();
                binding.set_child(Some(&picture));
            });
        }
        slot.upcast()
    }

    fn caption(&self, book: &BookInfo, cover_width: f64) -> gtk::Widget {
        let caption = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(2)
            .margin_start(6)
            .margin_end(6)
            .build();
        caption.set_accessible_role(gtk::AccessibleRole::Presentation);
        if let Some(progress) = self.store.progress(&book.id).filter(|p| *p > 0.005) {
            let bar = gtk::ProgressBar::builder()
                .fraction(progress.min(1.0))
                .width_request((cover_width * 0.62) as i32)
                .halign(gtk::Align::Center)
                .css_classes(["shelf-progress"])
                .build();
            caption.append(&bar);
        }
        caption.append(
            &gtk::Label::builder()
                .label(&book.title)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .lines(2)
                .ellipsize(ellipsis(&book.title))
                .justify(gtk::Justification::Center)
                .max_width_chars(1)
                .css_classes(["book-title"])
                .build(),
        );
        caption.append(
            &gtk::Label::builder()
                .label(author_line(book))
                .ellipsize(ellipsis(&author_line(book)))
                .max_width_chars(1)
                .css_classes(["book-author", "dim-label"])
                .build(),
        );
        caption.upcast()
    }
}

/// Cut long text short with "…", but leave very short text alone: GTK sizes
/// an ellipsizing label at least as wide as "…", which is wider than a
/// title like "()" and makes GTK complain about the layout.
fn ellipsis(text: &str) -> gtk::pango::EllipsizeMode {
    if text.chars().count() > 3 {
        gtk::pango::EllipsizeMode::End
    } else {
        gtk::pango::EllipsizeMode::None
    }
}

fn tooltip(book: &BookInfo) -> String {
    let mut parts = vec![book.title.clone(), author_line(book)];
    if let Some(publisher) = &book.publisher {
        parts.push(publisher.clone());
    }
    if let Some(year) = book.year {
        parts.push(year.to_string());
    }
    parts.retain(|p| !p.is_empty());
    parts.join("\n")
}

/// Number of cloth colours defined in the style sheet (`cloth-0` ...).
pub const CLOTH_COLORS: u32 = 36;

/// A plain cloth binding with the title, for books without a cover.
fn placeholder(book: &BookInfo, width: f64, height: f64) -> gtk::Widget {
    let hue = book.title.chars().fold(7u32, |h, c| {
        (h.wrapping_mul(31).wrapping_add(c as u32)) % 360
    });
    let scale = width / FULL_COVER_WIDTH;
    let cloth = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .width_request(width as i32)
        .height_request(height as i32)
        .css_classes([
            format!("cloth-{}", hue * CLOTH_COLORS / 360).as_str(),
            "cloth",
        ])
        .build();
    let inner = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .margin_start((10.0 * scale) as i32)
        .margin_end((10.0 * scale) as i32)
        .build();
    let attributes = |size: f64, bold: bool| {
        let list = gtk::pango::AttrList::new();
        list.insert(gtk::pango::AttrSize::new_size_absolute(
            (size * scale * gtk::pango::SCALE as f64) as i32,
        ));
        if bold {
            list.insert(gtk::pango::AttrInt::new_weight(
                gtk::pango::Weight::Semibold,
            ));
        }
        list
    };
    inner.append(
        &gtk::Label::builder()
            .label(&book.title)
            .wrap(true)
            .wrap_mode(gtk::pango::WrapMode::WordChar)
            .lines(5)
            .ellipsize(ellipsis(&book.title))
            .justify(gtk::Justification::Center)
            .max_width_chars(1)
            .attributes(&attributes(13.0, true))
            .css_classes(["cloth-title"])
            .build(),
    );
    inner.append(
        &gtk::Box::builder()
            .width_request(24)
            .height_request(1)
            .halign(gtk::Align::Center)
            .css_classes(["cloth-rule"])
            .build(),
    );
    inner.append(
        &gtk::Label::builder()
            .label(author_line(book))
            .wrap(true)
            .lines(2)
            .ellipsize(ellipsis(&author_line(book)))
            .justify(gtk::Justification::Center)
            .max_width_chars(1)
            .attributes(&attributes(10.0, false))
            .css_classes(["cloth-author"])
            .build(),
    );
    cloth.append(&inner);
    cloth.upcast()
}
