//! One open EPUB: a locked-down WebKitGTK view fed by the Rust core.
//!
//! The web view never touches the file system or the network. Every request
//! goes through `rattonero-book://`, answered from the core, which hands out
//! sanitised chapters. Script elements, event handlers and `javascript:`
//! URLs in book content are disabled; only the core's own pagination script
//! runs, injected here as a user script. A content filter blocks every
//! other URL, and the web view gets an ephemeral network session.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use percent_encoding::{
    percent_decode_str, utf8_percent_encode, AsciiSet, CONTROLS, NON_ALPHANUMERIC,
};
use rattonero_core::{
    content_security_policy, reader_script, BookInfo, EpubBook, Library, ReadingPosition, TocEntry,
    URL_SCHEME,
};
use webkit::prelude::*;
use webkit::{javascriptcore, soup};

use crate::appearance;
use crate::prefs::Prefs;
use crate::store::describe;

/// Characters to escape in a path inside a `rattonero-book://` URL.
const PATH: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

/// Where to land when a chapter opens.
enum Start {
    Fraction(f64),
    Anchor(String),
    End,
}

impl Start {
    fn json(&self) -> String {
        match self {
            Start::Fraction(f) => format!("{{\"fraction\":{f}}}"),
            Start::End => "{\"end\":true}".into(),
            Start::Anchor(id) => {
                format!("{{\"anchor\":{}}}", serde_json::Value::String(id.clone()))
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Loading {
    /// A chapter was asked for; the old one may still be talking.
    Requested,
    /// The new chapter replaced the old one but has not laid out yet.
    Committed,
    /// The chapter reported its page: everything it says counts.
    Shown,
}

pub struct EpubReader {
    pub page: adw::NavigationPage,
    info: BookInfo,
    book: Arc<EpubBook>,
    library: Option<Arc<Library>>,
    prefs: Rc<RefCell<Prefs>>,
    web: webkit::WebView,
    content: webkit::UserContentManager,
    toc: Vec<TocEntry>,
    spine_count: u32,
    spine_index: Cell<u32>,
    /// Where the chapter being opened has got to. Until it reports its
    /// first page, messages still queued from the chapter we left are
    /// stale: acting on their "turned past the chapter" would skip whole
    /// chapters, and their positions belong to the old chapter.
    loading: Cell<Loading>,
    page_index: Cell<u32>,
    pages: Cell<u32>,
    selection: RefCell<String>,
    chapter_label: gtk::Label,
    page_label: gtk::Label,
    progress_bar: gtk::ProgressBar,
    lookup: gtk::Revealer,
    lookup_label: gtk::Label,
    surface: gtk::Box,
    dark_handler: RefCell<Option<glib::SignalHandlerId>>,
}

impl EpubReader {
    pub fn new(
        info: BookInfo,
        library: Option<Arc<Library>>,
        prefs: Rc<RefCell<Prefs>>,
    ) -> Result<Rc<EpubReader>, String> {
        let book = EpubBook::open(info.path.clone()).map_err(|e| describe(&e))?;
        let toc = book.toc();
        let spine_count = book.spine_count();

        let context = webkit::WebContext::new();
        context.set_cache_model(webkit::CacheModel::DocumentViewer);
        let session = webkit::NetworkSession::new_ephemeral();
        let content = webkit::UserContentManager::new();
        let web = webkit::WebView::builder()
            .web_context(&context)
            .network_session(&session)
            .user_content_manager(&content)
            .hexpand(true)
            .vexpand(true)
            .build();
        if let Some(settings) = WebViewExt::settings(&web) {
            // The pagination script is a user script, which needs JavaScript;
            // scripts in the book's own markup stay off.
            settings.set_enable_javascript(true);
            settings.set_enable_javascript_markup(false);
            settings.set_javascript_can_open_windows_automatically(false);
            settings.set_javascript_can_access_clipboard(false);
            settings.set_allow_file_access_from_file_urls(false);
            settings.set_allow_universal_access_from_file_urls(false);
            settings.set_allow_top_navigation_to_data_urls(false);
            settings.set_allow_modal_dialogs(false);
            settings.set_enable_html5_local_storage(false);
            settings.set_enable_html5_database(false);
            settings.set_enable_page_cache(false);
            settings.set_enable_webgl(false);
            settings.set_enable_webaudio(false);
            settings.set_enable_webrtc(false);
            settings.set_enable_media_stream(false);
            settings.set_enable_encrypted_media(false);
            settings.set_enable_dns_prefetching(false);
            settings.set_enable_fullscreen(false);
            settings.set_enable_back_forward_navigation_gestures(false);
            settings.set_media_playback_requires_user_gesture(true);
            settings.set_enable_developer_extras(cfg!(debug_assertions));
        }

        let chapter_label = gtk::Label::builder()
            .hexpand(true)
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        let page_label = gtk::Label::builder().css_classes(["numeric"]).build();
        let progress_bar = gtk::ProgressBar::builder()
            .css_classes(["reader-progress"])
            .build();
        let lookup_label = gtk::Label::builder()
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .max_width_chars(24)
            .build();
        let lookup = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::SlideUp)
            .valign(gtk::Align::End)
            .halign(gtk::Align::Center)
            .margin_bottom(12)
            .build();
        let surface = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .css_classes(["reader-surface"])
            .build();

        let reader = Rc::new(EpubReader {
            page: adw::NavigationPage::builder()
                .title(&info.title)
                .tag("reader")
                .build(),
            info,
            book,
            library,
            prefs,
            web,
            content,
            toc,
            spine_count,
            spine_index: Cell::new(0),
            loading: Cell::new(Loading::Shown),
            page_index: Cell::new(0),
            pages: Cell::new(1),
            selection: RefCell::new(String::new()),
            chapter_label,
            page_label,
            progress_bar,
            lookup,
            lookup_label,
            surface,
            dark_handler: RefCell::new(None),
        });
        reader.build_ui();
        reader.connect_web(&context);
        reader.apply();
        reader.start();
        Ok(reader)
    }

    fn build_ui(self: &Rc<Self>) {
        let header = adw::HeaderBar::builder()
            .title_widget(&adw::WindowTitle::new(
                &self.info.title,
                &crate::shelf::author_line(&self.info),
            ))
            .build();

        let previous = gtk::Button::builder()
            .icon_name("rattonero-page-previous-symbolic")
            .tooltip_text("Previous Page")
            .action_name("reader.previous")
            .focus_on_click(false)
            .build();
        let next = gtk::Button::builder()
            .icon_name("rattonero-page-next-symbolic")
            .tooltip_text("Next Page")
            .action_name("reader.next")
            .focus_on_click(false)
            .build();
        let turns = gtk::Box::builder().css_classes(["linked"]).build();
        turns.append(&previous);
        turns.append(&next);
        header.pack_start(&turns);

        let changed: Rc<dyn Fn()> = {
            let weak = Rc::downgrade(self);
            Rc::new(move || {
                if let Some(reader) = weak.upgrade() {
                    reader.prefs.borrow().save();
                    reader.apply();
                }
            })
        };
        let appearance = gtk::MenuButton::builder()
            .icon_name("rattonero-text-size-symbolic")
            .tooltip_text("Appearance")
            .popover(&appearance::popover(&self.prefs, changed))
            .focus_on_click(false)
            .build();
        header.pack_end(&appearance);
        if !self.toc.is_empty() {
            header.pack_end(&self.contents_button());
        }

        // The lookup bar floats over the page so the text never reflows.
        let lookup_bar = gtk::Box::builder()
            .spacing(12)
            .css_classes(["lookup-bar", "toolbar", "osd"])
            .build();
        lookup_bar.append(&self.lookup_label);
        let online = gtk::Button::builder()
            .label("Look Up Online")
            .tooltip_text("Look the word up on Wiktionary in your browser")
            .focus_on_click(false)
            .build();
        let weak = Rc::downgrade(self);
        online.connect_clicked(move |button| {
            if let Some(url) = weak.upgrade().and_then(|r| r.lookup_url()) {
                launch(button, &url);
            }
        });
        lookup_bar.append(&online);
        self.lookup.set_child(Some(&lookup_bar));

        let overlay = gtk::Overlay::builder()
            .child(&self.web)
            .vexpand(true)
            .build();
        overlay.add_overlay(&self.lookup);

        // Where we are: chapter, page within it, and a hairline for the book.
        let footer = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(4)
            .margin_start(20)
            .margin_end(20)
            .margin_top(4)
            .margin_bottom(6)
            .css_classes(["reader-footer", "dim-label"])
            .build();
        let line = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        line.append(&self.chapter_label);
        line.append(&self.page_label);
        footer.append(&line);
        footer.append(&self.progress_bar);

        self.surface.append(&overlay);
        self.surface.append(&footer);

        let view = adw::ToolbarView::new();
        view.add_top_bar(&header);
        view.set_content(Some(&self.surface));
        self.page.set_child(Some(&view));

        let actions = gio::SimpleActionGroup::new();
        let add = |name: &str, f: fn(&EpubReader)| {
            let action = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            action.connect_activate(move |_, _| {
                if let Some(reader) = weak.upgrade() {
                    f(&reader);
                }
            });
            actions.add_action(&action);
        };
        add("previous", |r| r.run("Rattonero.prev()"));
        add("next", |r| r.run("Rattonero.next()"));
        add("bigger", |r| r.change_font_size(2.0));
        add("smaller", |r| r.change_font_size(-2.0));
        self.page.insert_action_group("reader", Some(&actions));

        // Follow the desktop's light and dark mode under "Auto".
        let weak = Rc::downgrade(self);
        let handler = adw::StyleManager::default().connect_dark_notify(move |_| {
            if let Some(reader) = weak.upgrade() {
                reader.apply();
            }
        });
        *self.dark_handler.borrow_mut() = Some(handler);
    }

    fn contents_button(self: &Rc<Self>) -> gtk::MenuButton {
        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .build();
        for entry in &self.toc {
            let label = gtk::Label::builder()
                .label(&entry.title)
                .xalign(0.0)
                // Long titles take a second line rather than losing
                // the words that tell chapters apart.
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .width_chars(24)
                .max_width_chars(40)
                .margin_start(6 + 16 * entry.depth.min(4) as i32)
                .build();
            let row = gtk::ListBoxRow::builder().child(&label).build();
            row.set_sensitive(entry.spine_index.is_some());
            list.append(&row);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_width(true)
            .propagate_natural_height(true)
            .max_content_height(520)
            .child(&list)
            .build();
        let popover = gtk::Popover::builder()
            .child(&scroller)
            .css_classes(["contents"])
            .build();
        // Open on the chapter being read.
        let weak = Rc::downgrade(self);
        let rows = list.downgrade();
        popover.connect_show(move |_| {
            let (Some(reader), Some(list)) = (weak.upgrade(), rows.upgrade()) else {
                return;
            };
            let row = reader
                .current_entry()
                .and_then(|i| list.row_at_index(i as i32));
            list.select_row(row.as_ref());
            if let Some(row) = row {
                row.grab_focus();
            }
        });
        let weak = Rc::downgrade(self);
        let closing = popover.downgrade();
        list.connect_row_activated(move |_, row| {
            let Some(reader) = weak.upgrade() else { return };
            if let Some(entry) = reader.toc.get(row.index() as usize).cloned() {
                reader.go_to(&entry);
            }
            if let Some(popover) = closing.upgrade() {
                popover.popdown();
            }
        });
        gtk::MenuButton::builder()
            .icon_name("rattonero-contents-symbolic")
            .tooltip_text("Contents")
            .popover(&popover)
            .focus_on_click(false)
            .build()
    }

    fn connect_web(self: &Rc<Self>, context: &webkit::WebContext) {
        // Serving the book.
        let book = self.book.clone();
        let policy = content_security_policy();
        context.register_uri_scheme(URL_SCHEME, move |request| serve(&book, &policy, request));

        // Messages from the pagination script.
        self.content
            .register_script_message_handler("rattonero", None);
        let weak = Rc::downgrade(self);
        self.content.connect_script_message_received(
            Some("rattonero"),
            move |_, value: &javascriptcore::Value| {
                if let (Some(reader), Some(json)) = (weak.upgrade(), value.to_json(0)) {
                    reader.receive(&json);
                }
            },
        );

        // Only chapters we load ourselves; never a navigation the content
        // asks for, and never a new window.
        self.web
            .connect_decide_policy(|_, decision, kind| match kind {
                webkit::PolicyDecisionType::NavigationAction => {
                    let ours = decision
                        .downcast_ref::<webkit::NavigationPolicyDecision>()
                        .and_then(|d| d.navigation_action())
                        .is_some_and(|action| {
                            let scheme_ok = action
                                .request()
                                .and_then(|r| r.uri())
                                .is_some_and(|uri| uri.starts_with(&format!("{URL_SCHEME}:")));
                            scheme_ok && action.navigation_type() == webkit::NavigationType::Other
                        });
                    if ours {
                        decision.use_();
                    } else {
                        decision.ignore();
                    }
                    true
                }
                webkit::PolicyDecisionType::NewWindowAction => {
                    decision.ignore();
                    true
                }
                _ => false,
            });

        // A page has no use for "Back", "Reload" or "Open Link": keep Copy.
        self.web.connect_context_menu(|_, menu, _| {
            for item in menu.items() {
                if item.stock_action() != webkit::ContextMenuAction::Copy {
                    menu.remove(&item);
                }
            }
            menu.items().is_empty()
        });

        self.web.connect_load_failed(|_, _, uri, error| {
            // A newer load replaced this one: nothing went wrong.
            if !error.matches(webkit::NetworkError::Cancelled)
                && !error.matches(webkit::PolicyError::FrameLoadInterruptedByPolicyChange)
            {
                glib::g_warning!("rattonero", "could not load {uri}: {error}");
            }
            false
        });
        let weak = Rc::downgrade(self);
        self.web.connect_load_changed(move |web, event| {
            glib::g_debug!("rattonero", "load {event:?}");
            if event == webkit::LoadEvent::Committed {
                // From here on, messages come from the new chapter.
                if let Some(reader) = weak.upgrade() {
                    if reader.loading.get() == Loading::Requested {
                        reader.loading.set(Loading::Committed);
                    }
                }
            }
            if event == webkit::LoadEvent::Finished {
                // Arrow keys and the space bar turn pages straight away.
                web.grab_focus();
            }
        });

        let weak = Rc::downgrade(self);
        self.web.connect_web_process_terminated(move |_, reason| {
            glib::g_warning!("rattonero", "web process ended: {reason:?}");
            if let Some(reader) = weak.upgrade() {
                let fraction = reader.page_index.get() as f64 / reader.pages.get().max(1) as f64;
                reader.load(reader.spine_index.get(), Start::Fraction(fraction));
            }
        });
    }

    /// Install the network block, then open the book where it was left.
    fn start(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        with_offline_filter(move |filter| {
            let Some(reader) = weak.upgrade() else { return };
            if let Some(filter) = filter {
                reader.content.add_filter(filter);
            }
            match reader
                .library
                .as_ref()
                .and_then(|l| l.position(reader.info.id.clone()))
            {
                Some(saved) if saved.index < reader.spine_count => {
                    reader.load(saved.index, Start::Fraction(saved.fraction))
                }
                _ => reader.load(0, Start::Fraction(0.0)),
            }
        });
    }

    /// The book was closed: stop following the desktop's theme.
    pub fn close(&self) {
        if let Some(handler) = self.dark_handler.borrow_mut().take() {
            adw::StyleManager::default().disconnect(handler);
        }
        self.web.terminate_web_process();
    }

    // ---- Navigation -------------------------------------------------------

    /// The contents entry for the chapter on screen.
    fn current_entry(&self) -> Option<usize> {
        let index = self.spine_index.get();
        self.toc
            .iter()
            .rposition(|e| e.spine_index.is_some_and(|i| i <= index))
    }

    fn chapter_title(&self) -> String {
        self.current_entry()
            .map(|i| self.toc[i].title.clone())
            .unwrap_or_else(|| self.info.title.clone())
    }

    fn progress(&self) -> f64 {
        if self.spine_count == 0 {
            return 0.0;
        }
        (self.spine_index.get() as f64
            + self.page_index.get() as f64 / self.pages.get().max(1) as f64)
            / self.spine_count as f64
    }

    fn go_to(&self, entry: &TocEntry) {
        let Some(index) = entry.spine_index else {
            return;
        };
        let start = entry
            .fragment
            .clone()
            .filter(|f| !f.is_empty())
            .map(Start::Anchor)
            .unwrap_or(Start::Fraction(0.0));
        self.load(index, start);
    }

    fn run(&self, script: &str) {
        self.web.evaluate_javascript(
            &format!("window.Rattonero && {script}; null"),
            None,
            None,
            None::<&gio::Cancellable>,
            |_| {},
        );
    }

    fn load(&self, index: u32, start: Start) {
        if index >= self.spine_count {
            return;
        }
        let Some(path) = self.book.spine_path(index) else {
            return;
        };
        self.spine_index.set(index);
        self.loading.set(Loading::Requested);
        self.set_selection(String::new());
        // The pagination script runs before any content so the page is laid
        // out, themed and positioned before it is first shown.
        let settings = self.settings_json();
        self.content.remove_all_scripts();
        let source = format!(
            "{}\nRattonero.configure({settings}, {});",
            reader_script(),
            start.json()
        );
        self.content.add_script(&webkit::UserScript::new(
            &source,
            webkit::UserContentInjectedFrames::TopFrame,
            webkit::UserScriptInjectionTime::Start,
            &[],
            &[],
        ));
        let url = format!(
            "{URL_SCHEME}://book/{}",
            utf8_percent_encode(path.trim_start_matches('/'), PATH)
        );
        glib::g_debug!("rattonero", "loading {url}");
        self.web.load_uri(&url);
        self.update_footer();
    }

    fn follow(&self, href: &str) {
        let prefix = format!("{URL_SCHEME}://book/");
        if let Some(rest) = href.strip_prefix(&prefix) {
            let (path, fragment) = match rest.split_once('#') {
                Some((path, fragment)) => (path, Some(fragment)),
                None => (rest, None),
            };
            let path = path.split('?').next().unwrap_or_default();
            let path = format!("/{}", percent_decode_str(path).decode_utf8_lossy());
            let Some(index) = self.book.spine_index_of(path) else {
                return;
            };
            let fragment = fragment
                .map(|f| percent_decode_str(f).decode_utf8_lossy().into_owned())
                .filter(|f| !f.is_empty());
            match fragment {
                Some(id) if index == self.spine_index.get() => {
                    self.run(&format!(
                        "Rattonero.goToAnchor({}.anchor)",
                        Start::Anchor(id).json()
                    ));
                }
                Some(id) => self.load(index, Start::Anchor(id)),
                None => self.load(index, Start::Fraction(0.0)),
            }
        } else {
            let scheme = href
                .split(':')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if ["http", "https", "mailto"].contains(&scheme.as_str()) {
                launch(&self.web, href);
            }
        }
    }

    fn save_position(&self) {
        let Some(library) = &self.library else { return };
        let (page, pages) = (self.page_index.get(), self.pages.get().max(1));
        let position = ReadingPosition {
            index: self.spine_index.get(),
            fraction: page as f64 / pages as f64,
            progress: self.progress(),
            finished: self.spine_index.get() + 1 == self.spine_count && page + 1 >= pages,
        };
        library.set_position(self.info.id.clone(), position);
    }

    fn update_footer(&self) {
        self.chapter_label.set_label(&self.chapter_title());
        self.page_label.set_label(&format!(
            "{} of {}",
            self.page_index.get() + 1,
            self.pages.get().max(1)
        ));
        self.progress_bar
            .set_fraction(self.progress().clamp(0.0, 1.0));
    }

    // ---- Messages from the pagination script --------------------------------

    fn receive(&self, json: &str) {
        glib::g_debug!("rattonero", "message {json}");
        let Ok(message) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let number = |key: &str| {
            message
                .get(key)
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0)
                .max(0.0) as u32
        };
        match message.get("type").and_then(|t| t.as_str()) {
            Some(_) if self.loading.get() == Loading::Requested => {}
            Some("position") => {
                self.loading.set(Loading::Shown);
                self.page_index.set(number("page"));
                self.pages.set(number("pages").max(1));
                self.update_footer();
                self.save_position();
            }
            Some("edge") if self.loading.get() != Loading::Shown => {}
            Some("edge") => {
                let index = self.spine_index.get();
                if message.get("direction").and_then(|d| d.as_str()) == Some("next") {
                    self.load(index + 1, Start::Fraction(0.0));
                } else if let Some(previous) = index.checked_sub(1) {
                    self.load(previous, Start::End);
                }
            }
            Some("link") => {
                if let Some(href) = message.get("href").and_then(|h| h.as_str()) {
                    self.follow(href);
                }
            }
            Some("image") => {
                if let Some(src) = message.get("src").and_then(|s| s.as_str()) {
                    self.zoom(src);
                }
            }
            Some("selection") => {
                let text = message
                    .get("text")
                    .and_then(|t| t.as_str())
                    .unwrap_or_default()
                    .to_string();
                self.set_selection(text);
            }
            _ => {}
        }
    }

    fn set_selection(&self, text: String) {
        self.lookup_label.set_label(&format!("“{text}”"));
        self.lookup.set_reveal_child(!text.is_empty());
        *self.selection.borrow_mut() = text;
    }

    /// A dictionary page for the selection, in the book's language.
    fn lookup_url(&self) -> Option<String> {
        let selection = self.selection.borrow();
        if selection.is_empty() {
            return None;
        }
        let mut language: String = self
            .info
            .language
            .as_deref()
            .unwrap_or("en")
            .chars()
            .take(2)
            .collect::<String>()
            .to_lowercase();
        if language.len() != 2 || !language.chars().all(|c| c.is_ascii_lowercase()) {
            language = "en".into();
        }
        let term = utf8_percent_encode(&selection.to_lowercase(), NON_ALPHANUMERIC).to_string();
        Some(format!(
            "https://{language}.wiktionary.org/w/index.php?search={term}"
        ))
    }

    /// Show a picture from the book on its own, as large as the window allows.
    fn zoom(&self, src: &str) {
        let prefix = format!("{URL_SCHEME}://book/");
        let Some(path) = src.strip_prefix(&prefix) else {
            return;
        };
        let path = percent_decode_str(path.split(['?', '#']).next().unwrap_or_default())
            .decode_utf8_lossy()
            .into_owned();
        let Ok(resource) = self.book.resource(path) else {
            return;
        };
        let Ok(texture) = gdk::Texture::from_bytes(&glib::Bytes::from_owned(resource.data)) else {
            return;
        };
        let picture = gtk::Picture::builder()
            .paintable(&texture)
            .content_fit(gtk::ContentFit::Contain)
            .hexpand(true)
            .vexpand(true)
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&picture));
        let (width, height) = (self.web.width().max(360), self.web.height().max(360));
        let dialog = adw::Dialog::builder()
            .child(&view)
            .content_width((texture.width() + 24).clamp(240, width))
            .content_height((texture.height() + 70).clamp(240, height))
            .title(&self.info.title)
            .build();
        let click = gtk::GestureClick::new();
        let closing = dialog.downgrade();
        click.connect_released(move |_, _, _, _| {
            if let Some(dialog) = closing.upgrade() {
                dialog.close();
            }
        });
        picture.add_controller(click);
        dialog.present(Some(&self.web));
    }

    // ---- Appearance ---------------------------------------------------------

    fn settings_json(&self) -> String {
        self.prefs
            .borrow()
            .reader_json(adw::StyleManager::default().is_dark())
    }

    fn change_font_size(&self, delta: f64) {
        {
            let mut prefs = self.prefs.borrow_mut();
            let (low, high) = crate::prefs::FONT_SIZES;
            prefs.font_size = (prefs.font_size + delta).clamp(low, high);
            prefs.save();
        }
        self.apply();
    }

    /// Theme the window and the page, and lay the text out again.
    pub fn apply(&self) {
        let theme = self.prefs.borrow().theme;
        let style = adw::StyleManager::default();
        style.set_color_scheme(theme.color_scheme());
        let dark = style.is_dark();
        self.web.set_background_color(&theme.background(dark));
        for name in ["light", "sepia", "dark"] {
            self.surface.remove_css_class(&format!("reader-{name}"));
        }
        self.surface
            .add_css_class(&format!("reader-{}", theme.css_name(dark)));
        self.run(&format!("Rattonero.apply({})", self.settings_json()));
    }
}

/// Answer a `rattonero-book://` request from the book.
fn serve(book: &EpubBook, policy: &str, request: &webkit::URISchemeRequest) {
    let path = request
        .path()
        .map(|p| percent_decode_str(&p).decode_utf8_lossy().into_owned())
        .unwrap_or_default();
    glib::g_debug!("rattonero", "serving {path}");
    match book.resource(path) {
        Ok(resource) => {
            let html = resource.mime == "text/html";
            let bytes = glib::Bytes::from_owned(resource.data);
            let length = bytes.len() as i64;
            let stream = gio::MemoryInputStream::from_bytes(&bytes);
            let response = webkit::URISchemeResponse::new(&stream, length);
            response.set_status(200, None);
            response.set_content_type(&resource.mime);
            let headers = soup::MessageHeaders::new(soup::MessageHeadersType::Response);
            headers.append("Cache-Control", "no-store");
            if html {
                headers.append("Content-Type", "text/html; charset=utf-8");
                headers.append("Content-Security-Policy", policy);
            } else {
                headers.append("Content-Type", &resource.mime);
            }
            response.set_http_headers(headers);
            request.finish_with_response(&response);
        }
        Err(error) => {
            let mut error = glib::Error::new(gio::IOErrorEnum::NotFound, &describe(&error));
            request.finish_error(&mut error);
        }
    }
}

thread_local! {
    static OFFLINE_FILTER: RefCell<Option<webkit::UserContentFilter>> = const { RefCell::new(None) };
}

/// A content filter that refuses every load except the book's own resources:
/// a second lock behind the Content-Security-Policy. Compiled once.
fn with_offline_filter(done: impl FnOnce(Option<&webkit::UserContentFilter>) + 'static) {
    if let Some(filter) = OFFLINE_FILTER.with_borrow(|f| f.clone()) {
        done(Some(&filter));
        return;
    }
    let rules = format!(
        r#"[{{"trigger":{{"url-filter":".*"}},"action":{{"type":"block"}}}},
            {{"trigger":{{"url-filter":"^{URL_SCHEME}:"}},"action":{{"type":"ignore-previous-rules"}}}},
            {{"trigger":{{"url-filter":"^data:"}},"action":{{"type":"ignore-previous-rules"}}}}]"#
    );
    let directory = glib::user_cache_dir()
        .join("rattonero")
        .join("content-filters");
    let store = webkit::UserContentFilterStore::new(&directory.to_string_lossy());
    store.save(
        "rattonero-offline",
        &glib::Bytes::from_owned(rules.into_bytes()),
        None::<&gio::Cancellable>,
        move |result| match result {
            Ok(filter) => {
                glib::g_debug!("rattonero", "content filter ready");
                OFFLINE_FILTER.with_borrow_mut(|f| *f = Some(filter.clone()));
                done(Some(&filter));
            }
            Err(error) => {
                glib::g_warning!("rattonero", "could not compile the content filter: {error}");
                done(None);
            }
        },
    );
}

/// Open a web or mail link in the user's browser or mail program.
pub fn launch(widget: &impl IsA<gtk::Widget>, uri: &str) {
    let window = widget.root().and_downcast::<gtk::Window>();
    gtk::UriLauncher::new(uri).launch(window.as_ref(), None::<&gio::Cancellable>, |_| {});
}
