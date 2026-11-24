//! The main window: every book in the library folder, on shelves, and the
//! reader pushed on top when a book is opened.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use rattonero_core::{BookFormat, BookInfo};

use crate::covers::Covers;
use crate::epub::EpubReader;
use crate::pdf::PdfReader;
use crate::prefs::{self, Prefs};
use crate::shelf::ShelfView;
use crate::store::Store;

/// The reader on screen, kept alive while its page is shown.
enum Reader {
    Epub(Rc<EpubReader>),
    Pdf(#[allow(dead_code)] Rc<PdfReader>),
}

pub struct Window {
    pub window: adw::ApplicationWindow,
    nav: adw::NavigationView,
    store: Rc<Store>,
    prefs: Rc<RefCell<Prefs>>,
    shelf: Rc<ShelfView>,
    title: adw::WindowTitle,
    stack: gtk::Stack,
    banner: adw::Banner,
    empty: adw::StatusPage,
    order: adw::ToggleGroup,
    reader: RefCell<Option<Reader>>,
}

const DESCRIPTION: &str = "Rattonero Books shows every EPUB and PDF in a folder you pick, such as one \
synchronised between your computers or a network disk. Your books are never changed; reading progress \
is kept in a small hidden folder beside them so your other devices can pick up where you left off.";

impl Window {
    pub fn new(app: &adw::Application) -> Rc<Window> {
        let prefs = Rc::new(RefCell::new(Prefs::load()));
        let store = Store::new(prefs.borrow().order());
        let shelf = ShelfView::new(store.clone(), Covers::new());

        let (width, height, maximized) = {
            let p = prefs.borrow();
            (p.window_width, p.window_height, p.maximized)
        };
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Rattonero Books")
            .default_width(width)
            .default_height(height)
            .maximized(maximized)
            .width_request(360)
            .height_request(360)
            .build();

        let this = Rc::new(Window {
            window,
            nav: adw::NavigationView::new(),
            store,
            prefs,
            shelf,
            title: adw::WindowTitle::new("Rattonero Books", ""),
            stack: gtk::Stack::builder()
                .transition_type(gtk::StackTransitionType::Crossfade)
                .build(),
            banner: adw::Banner::builder()
                .title("Reading progress on this device is not reaching your other devices.")
                .button_label("Allow…")
                .build(),
            empty: adw::StatusPage::builder()
                .icon_name("rattonero-folder-symbolic")
                .title("No books here yet")
                .build(),
            order: adw::ToggleGroup::new(),
            reader: RefCell::new(None),
        });
        this.build_library_page();
        this.install_actions();
        this.connect_signals();
        this.window.set_content(Some(&this.nav));
        this.restore_folder();
        this.update();
        this
    }

    fn build_library_page(self: &Rc<Self>) {
        let header = adw::HeaderBar::builder().title_widget(&self.title).build();

        let menu = gio::Menu::new();
        let library = gio::Menu::new();
        library.append(Some("Choose Library Folder…"), Some("win.choose-folder"));
        library.append(Some("Look for New Books"), Some("win.refresh"));
        menu.append_section(None, &library);
        let order = gio::Menu::new();
        for o in prefs::ORDERS {
            let item = gio::MenuItem::new(Some(prefs::order_label(o)), None);
            item.set_action_and_target_value(
                Some("win.order"),
                Some(&prefs::order_name(o).to_variant()),
            );
            order.append_item(&item);
        }
        menu.append_submenu(Some("Arrange By"), &order);
        let app = gio::Menu::new();
        app.append(Some("Keyboard Shortcuts"), Some("win.show-help-overlay"));
        app.append(Some("About Rattonero Books"), Some("app.about"));
        menu.append_section(None, &app);
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("rattonero-menu-symbolic")
                .menu_model(&menu)
                .tooltip_text("Main Menu")
                .primary(true)
                .build(),
        );

        // Arrange the shelves by author, title, publisher or year.
        for o in prefs::ORDERS {
            self.order.add(
                adw::Toggle::builder()
                    .name(prefs::order_name(o))
                    .label(prefs::order_label(o))
                    .build(),
            );
        }
        self.order
            .set_active_name(Some(prefs::order_name(self.store.order())));
        self.order.set_tooltip_text(Some(
            "Arrange the shelves by author, title, publisher or year",
        ));
        header.pack_start(&self.order);

        // Nothing chosen yet.
        let choose = adw::StatusPage::builder()
            .icon_name("com.rattonero.ebookreader")
            .title("Choose your library folder")
            .description(DESCRIPTION)
            .child(&pill("Choose Folder…", "win.choose-folder", true))
            .build();
        // Looking.
        let scanning = adw::StatusPage::builder()
            .paintable(&adw::SpinnerPaintable::new(None::<&gtk::Widget>))
            .title("Looking for books…")
            .build();
        // Nothing found.
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        buttons.append(&pill("Look Again", "win.refresh", true));
        buttons.append(&pill("Choose Another Folder…", "win.choose-folder", false));
        self.empty.set_child(Some(&buttons));

        self.stack.add_named(&choose, Some("choose"));
        self.stack.add_named(&scanning, Some("scanning"));
        self.stack.add_named(&self.empty, Some("empty"));
        self.stack.add_named(&self.shelf.widget, Some("shelf"));
        if let Some(spinner) = scanning.paintable().and_downcast::<adw::SpinnerPaintable>() {
            spinner.set_widget(Some(&scanning));
        }

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.add_top_bar(&self.banner);
        view.set_content(Some(&self.stack));

        // A narrow window keeps the order in the main menu.
        let breakpoint = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            620.0,
            adw::LengthUnit::Sp,
        ));
        breakpoint.add_setter(&self.order, "visible", Some(&false.to_value()));
        self.window.add_breakpoint(breakpoint);

        let page = adw::NavigationPage::builder()
            .title("Library")
            .tag("library")
            .child(&view)
            .build();
        self.nav.add(&page);
    }

    fn install_actions(self: &Rc<Self>) {
        let window = &self.window;
        let add = |name: &str, f: fn(&Rc<Window>)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(this) = weak.upgrade() {
                    f(&this);
                }
            });
            window.add_action(&action);
        };
        add("choose-folder", |w| w.choose_folder());
        add("refresh", |w| w.store.refresh());
        add("show-help-overlay", |w| w.show_shortcuts());

        let order = gio::SimpleAction::new_stateful(
            "order",
            Some(glib::VariantTy::STRING),
            &prefs::order_name(self.store.order()).to_variant(),
        );
        let weak = Rc::downgrade(self);
        order.connect_activate(move |_, parameter| {
            if let (Some(this), Some(name)) = (weak.upgrade(), parameter.and_then(|p| p.str())) {
                this.set_order(name);
            }
        });
        window.add_action(&order);

        let weak = Rc::downgrade(self);
        self.order.connect_active_name_notify(move |group| {
            if let (Some(this), Some(name)) = (weak.upgrade(), group.active_name()) {
                this.set_order(&name);
            }
        });
        let weak = Rc::downgrade(self);
        self.banner.connect_button_clicked(move |_| {
            if let Some(this) = weak.upgrade() {
                this.choose_folder();
            }
        });
    }

    fn set_order(&self, name: &str) {
        let Some(order) = prefs::order_from_name(name) else {
            return;
        };
        if let Some(action) = self.window.lookup_action("order") {
            action.change_state(&name.to_variant());
        }
        if self.order.active_name().as_deref() != Some(name) {
            self.order.set_active_name(Some(name));
        }
        if self.store.order() != order {
            self.prefs.borrow_mut().order = name.to_string();
            self.prefs.borrow().save();
            self.store.set_order(order);
        }
    }

    fn connect_signals(self: &Rc<Self>) {
        // The style sheet's dark shelf colours hang off this class.
        let style = adw::StyleManager::default();
        let follow = |window: &adw::ApplicationWindow, dark: bool| {
            if dark {
                window.add_css_class("dark");
            } else {
                window.remove_css_class("dark");
            }
        };
        follow(&self.window, style.is_dark());
        let window = self.window.downgrade();
        style.connect_dark_notify(move |style| {
            if let Some(window) = window.upgrade() {
                follow(&window, style.is_dark());
            }
        });

        let weak = Rc::downgrade(self);
        self.store.connect_changed(move || {
            if let Some(this) = weak.upgrade() {
                this.update();
            }
        });
        let weak = Rc::downgrade(self);
        self.shelf.connect_open(move |book| {
            if let Some(this) = weak.upgrade() {
                this.open_book(book);
            }
        });

        // The book was closed: share where we are, and give the chrome back
        // its usual colours.
        let weak = Rc::downgrade(self);
        self.nav.connect_popped(move |_, _| {
            let Some(this) = weak.upgrade() else { return };
            if let Some(Reader::Epub(reader)) = this.reader.borrow_mut().take() {
                reader.close();
            }
            adw::StyleManager::default().set_color_scheme(adw::ColorScheme::Default);
            this.store.share_progress();
        });

        // Leaving the window counts as putting the book down.
        let weak = Rc::downgrade(self);
        self.window.connect_is_active_notify(move |window| {
            if let Some(this) = weak.upgrade() {
                if window.is_active() {
                    this.store.reload_progress();
                } else {
                    this.store.share_progress();
                }
            }
        });

        // While the window is in front, keep following the other devices.
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(Duration::from_secs(3), move || {
            let Some(this) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if this.window.is_active() {
                this.store.reload_progress();
            }
            glib::ControlFlow::Continue
        });

        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |window| {
            if let Some(this) = weak.upgrade() {
                this.store.share_progress();
                let mut prefs = this.prefs.borrow_mut();
                prefs.maximized = window.is_maximized();
                if !prefs.maximized {
                    prefs.window_width = window.default_width();
                    prefs.window_height = window.default_height();
                }
                prefs.save();
            }
            glib::Propagation::Proceed
        });
    }

    /// Show what the store holds now.
    fn update(&self) {
        let store = &self.store;
        let folder = store.folder();
        let count = store.book_count();
        self.title.set_subtitle(&match (&folder, count) {
            (None, _) => String::new(),
            (Some(_), 1) => "1 book".into(),
            (Some(_), n) => format!("{n} books"),
        });
        self.order.set_sensitive(count > 0);
        let page = match folder {
            None => "choose",
            Some(_) if count > 0 => "shelf",
            Some(_) if store.is_scanning() => "scanning",
            Some(folder) => {
                let name = folder
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                self.empty.set_description(Some(&format!(
                    "Put EPUB or PDF files in “{}” and they will appear on the shelf.",
                    glib::markup_escape_text(&name)
                )));
                "empty"
            }
        };
        self.stack.set_visible_child_name(page);
        self.banner
            .set_revealed(store.folder().is_some() && !store.shares_progress());
        if page == "shelf" {
            self.shelf.rebuild();
        }
    }

    /// Reopen the folder from last time. In debug builds `RATTONERO_LIBRARY`
    /// points the app at a folder without the picker, and `RATTONERO_OPEN`
    /// opens a book by library id.
    fn restore_folder(self: &Rc<Self>) {
        let development = cfg!(debug_assertions);
        let from_env = std::env::var_os("RATTONERO_LIBRARY")
            .filter(|_| development)
            .map(PathBuf::from);
        let remembered = self
            .prefs
            .borrow()
            .library_folder
            .clone()
            .map(PathBuf::from);
        if let Some(folder) = from_env.or(remembered) {
            if folder.is_dir() {
                if let Err(message) = self.store.open(&folder) {
                    self.show_error(&message);
                }
            }
        }
        if let Some(id) = std::env::var("RATTONERO_OPEN").ok().filter(|_| development) {
            let weak = Rc::downgrade(self);
            let mut tries = 0;
            glib::timeout_add_local(Duration::from_millis(100), move || {
                let Some(this) = weak.upgrade() else {
                    return glib::ControlFlow::Break;
                };
                tries += 1;
                if let Some(book) = this.store.book(&id) {
                    this.open_book(book);
                    return glib::ControlFlow::Break;
                }
                if tries > 50 {
                    glib::ControlFlow::Break
                } else {
                    glib::ControlFlow::Continue
                }
            });
        }
    }

    fn choose_folder(self: &Rc<Self>) {
        let dialog = gtk::FileDialog::builder()
            .title("Choose Library Folder")
            .accept_label("Choose")
            .modal(true)
            .build();
        if let Some(folder) = self.store.folder() {
            dialog.set_initial_folder(Some(&gio::File::for_path(folder)));
        }
        let weak = Rc::downgrade(self);
        dialog.select_folder(
            Some(&self.window),
            None::<&gio::Cancellable>,
            move |result| {
                let (Some(this), Ok(file)) = (weak.upgrade(), result) else {
                    return;
                };
                let Some(path) = file.path() else {
                    this.show_error("Only folders on this computer can be opened.");
                    return;
                };
                match this.store.open(&path) {
                    Ok(()) => {
                        this.prefs.borrow_mut().library_folder =
                            Some(path.to_string_lossy().into_owned());
                        this.prefs.borrow().save();
                    }
                    Err(message) => this.show_error(&message),
                }
            },
        );
    }

    fn show_error(&self, message: &str) {
        let alert = adw::AlertDialog::new(Some("Could not open the folder"), Some(message));
        alert.add_response("ok", "OK");
        alert.present(Some(&self.window));
    }

    fn open_book(self: &Rc<Self>, book: BookInfo) {
        if self.reader.borrow().is_some() {
            return;
        }
        let library = self.store.library();
        let opened = match book.format {
            BookFormat::Epub => {
                EpubReader::new(book.clone(), library, self.prefs.clone()).map(Reader::Epub)
            }
            BookFormat::Pdf => PdfReader::new(book.clone(), library).map(Reader::Pdf),
        };
        match opened {
            Ok(reader) => {
                let page = match &reader {
                    Reader::Epub(r) => r.page.clone(),
                    Reader::Pdf(r) => r.page.clone(),
                };
                *self.reader.borrow_mut() = Some(reader);
                self.nav.push(&page);
            }
            Err(message) => {
                let what = if book.format == BookFormat::Pdf {
                    "This PDF cannot be opened"
                } else {
                    "This book cannot be opened"
                };
                let status = adw::StatusPage::builder()
                    .icon_name("rattonero-warning-symbolic")
                    .title(what)
                    .description(glib::markup_escape_text(&message))
                    .build();
                let view = adw::ToolbarView::new();
                view.add_top_bar(&adw::HeaderBar::new());
                view.set_content(Some(&status));
                self.nav.push(
                    &adw::NavigationPage::builder()
                        .title(&book.title)
                        .child(&view)
                        .build(),
                );
            }
        }
    }

    fn show_shortcuts(&self) {
        let dialog = adw::ShortcutsDialog::new();
        let library = adw::ShortcutsSection::new(Some("Library"));
        for (title, accel) in [
            ("Choose library folder", "<Control>o"),
            ("Look for new books", "<Control>r"),
            ("Back to the library", "<Alt>Left"),
            ("Quit", "<Control>q"),
        ] {
            library.add(adw::ShortcutsItem::new(title, accel));
        }
        dialog.add(library);
        let reading = adw::ShortcutsSection::new(Some("Reading"));
        for (title, accel) in [
            ("Next page", "Right space Page_Down"),
            ("Previous page", "Left <Shift>space Page_Up"),
            ("Larger text", "<Control>plus"),
            ("Smaller text", "<Control>minus"),
        ] {
            reading.add(adw::ShortcutsItem::new(title, accel));
        }
        dialog.add(reading);
        dialog.present(Some(&self.window));
    }
}

fn pill(label: &str, action: &str, suggested: bool) -> gtk::Button {
    let button = gtk::Button::builder()
        .label(label)
        .action_name(action)
        .halign(gtk::Align::Center)
        .css_classes(["pill"])
        .build();
    if suggested {
        button.add_css_class("suggested-action");
    }
    button
}
