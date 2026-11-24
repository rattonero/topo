//! The shared folder of books and what is on its shelves.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use gtk::{gio, glib};
use rattonero_core::{arrange, BookInfo, CoreError, Library, Shelf, ShelfOrder};

/// How many books the "Continue reading" shelf offers at most.
const CONTINUE_LIMIT: u32 = 12;

#[derive(Default)]
pub struct Store {
    library: RefCell<Option<Arc<Library>>>,
    folder: RefCell<Option<PathBuf>>,
    books: RefCell<Vec<BookInfo>>,
    shelves: RefCell<Vec<Shelf>>,
    continue_reading: RefCell<Vec<BookInfo>>,
    shares_progress: Cell<bool>,
    scanning: Cell<bool>,
    reloading: Cell<bool>,
    order: Cell<Option<ShelfOrder>>,
    listeners: RefCell<Vec<Rc<dyn Fn()>>>,
}

/// A message fit for showing to the reader.
pub fn describe(error: &CoreError) -> String {
    match error {
        CoreError::Io { message } => message.clone(),
        CoreError::Invalid { message } => format!("This file is not a readable book ({message})."),
        CoreError::NotFound { message } => format!("Not found: {message}"),
        CoreError::Protected => "This book is protected by DRM and cannot be opened.".into(),
    }
}

impl Store {
    pub fn new(order: ShelfOrder) -> Rc<Store> {
        let store = Store {
            shares_progress: Cell::new(true),
            ..Store::default()
        };
        store.order.set(Some(order));
        Rc::new(store)
    }

    /// Called after anything visible changed.
    pub fn connect_changed(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Rc::new(f));
    }

    fn changed(&self) {
        let listeners: Vec<_> = self.listeners.borrow().clone();
        for listener in listeners {
            listener();
        }
    }

    pub fn library(&self) -> Option<Arc<Library>> {
        self.library.borrow().clone()
    }
    pub fn folder(&self) -> Option<PathBuf> {
        self.folder.borrow().clone()
    }
    pub fn shelves(&self) -> Vec<Shelf> {
        self.shelves.borrow().clone()
    }
    pub fn continue_reading(&self) -> Vec<BookInfo> {
        self.continue_reading.borrow().clone()
    }
    pub fn book_count(&self) -> usize {
        self.books.borrow().len()
    }
    pub fn is_scanning(&self) -> bool {
        self.scanning.get()
    }
    /// False when the folder is read-only and progress stays on this device.
    pub fn shares_progress(&self) -> bool {
        self.shares_progress.get()
    }
    pub fn order(&self) -> ShelfOrder {
        self.order.get().unwrap_or(ShelfOrder::Author)
    }

    pub fn set_order(&self, order: ShelfOrder) {
        if self.order.get() == Some(order) {
            return;
        }
        self.order.set(Some(order));
        *self.shelves.borrow_mut() = arrange(self.books.borrow().clone(), order);
        self.changed();
    }

    pub fn book(&self, id: &str) -> Option<BookInfo> {
        self.books.borrow().iter().find(|b| b.id == id).cloned()
    }

    pub fn progress(&self, book_id: &str) -> Option<f64> {
        self.library
            .borrow()
            .as_ref()?
            .position(book_id.to_string())
            .map(|p| p.progress)
    }

    /// Open a folder of books. The state directory (metadata cache, this
    /// device's id and a private copy of the progress) lives in the user's
    /// data directory.
    pub fn open(self: &Rc<Self>, folder: &Path) -> Result<(), String> {
        let state = glib::user_data_dir().join("rattonero");
        let library = Library::new(
            folder.to_string_lossy().into_owned(),
            state.to_string_lossy().into_owned(),
        )
        .map_err(|e| describe(&e))?;
        // Whatever was read in the old folder must reach the other devices.
        if let Some(old) = self.library.replace(Some(library)) {
            old.flush();
        }
        *self.folder.borrow_mut() = Some(folder.to_path_buf());
        self.books.borrow_mut().clear();
        self.shelves.borrow_mut().clear();
        self.continue_reading.borrow_mut().clear();
        self.shares_progress.set(true);
        self.changed();
        self.refresh();
        Ok(())
    }

    /// Look for new books. The scan runs on a worker thread.
    pub fn refresh(self: &Rc<Self>) {
        let Some(library) = self.library() else {
            return;
        };
        if self.scanning.replace(true) {
            return;
        }
        self.changed();
        let this = self.clone();
        glib::spawn_future_local(async move {
            let worker = library.clone();
            let found = gio::spawn_blocking(move || {
                worker.reload_progress();
                worker.scan()
            })
            .await
            .unwrap_or_default();
            this.scanning.set(false);
            // The folder may have been switched while the scan ran: then
            // these books are stale, and the new folder still needs a look.
            if !this
                .library()
                .is_some_and(|current| Arc::ptr_eq(&current, &library))
            {
                this.refresh();
                return;
            }
            *this.shelves.borrow_mut() = arrange(found.clone(), this.order());
            *this.books.borrow_mut() = found;
            this.progress_changed();
        });
    }

    /// Reading progress changed: redraw marks and the top shelf.
    pub fn progress_changed(&self) {
        if let Some(library) = self.library() {
            let books = self.books.borrow().clone();
            *self.continue_reading.borrow_mut() = library.continue_reading(books, CONTINUE_LIMIT);
            self.shares_progress.set(library.shares_progress());
        }
        self.changed();
    }

    /// A book was closed, or the window lost focus: make sure the other
    /// devices can see where we are.
    pub fn share_progress(&self) {
        if let Some(library) = self.library() {
            library.flush();
        }
        self.progress_changed();
    }

    /// Pick up what other devices have read since we last looked. Called
    /// every few seconds while the window is in front.
    pub fn reload_progress(self: &Rc<Self>) {
        let Some(library) = self.library() else {
            return;
        };
        if self.reloading.replace(true) {
            return;
        }
        let this = self.clone();
        glib::spawn_future_local(async move {
            let worker = library.clone();
            let changed = gio::spawn_blocking(move || worker.reload_progress())
                .await
                .unwrap_or(false);
            this.reloading.set(false);
            if changed
                && this
                    .library()
                    .is_some_and(|current| Arc::ptr_eq(&current, &library))
            {
                this.progress_changed();
            }
        });
    }

    pub fn dismiss_from_continue_reading(&self, book_id: &str) {
        if let Some(library) = self.library() {
            library.dismiss_from_continue_reading(book_id.to_string());
        }
        self.progress_changed();
    }
}
