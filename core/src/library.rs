//! The bookshelf: scanning a folder and remembering reading positions.

use crate::error::CoreError;
use crate::model::{BookFormat, BookInfo, ReadingPosition};
use crate::{epub, pdf, sort};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::hash::{BuildHasher, Hasher};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const MAX_DEPTH: usize = 8;
const MAX_BOOKS: usize = 50_000;
const CACHE_VERSION: u32 = 1;

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    modified: u64,
    size: u64,
    info: BookInfo,
}

#[derive(Serialize, Deserialize, Default)]
struct Cache {
    version: u32,
    entries: HashMap<String, CacheEntry>,
}

/// What we remember about one book. `updated` (milliseconds since 1970) makes
/// merging trivial: for each book the most recent entry wins.
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Debug)]
struct Entry {
    index: u32,
    fraction: f64,
    progress: f64,
    #[serde(default)]
    finished: bool,
    /// Removed from "continue reading" by hand; cleared by reading on.
    #[serde(default)]
    dismissed: bool,
    #[serde(default)]
    updated: u64,
    /// Time spent with the book open and pages turning, in seconds. Absent
    /// in entries written before this was tracked; those count as read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seconds_read: Option<u32>,
}

impl Entry {
    fn position(&self) -> ReadingPosition {
        ReadingPosition { index: self.index, fraction: self.fraction, progress: self.progress, finished: self.finished }
    }
    /// Whether this entry should replace `other` for the same book. Every
    /// device applies the same rule, so they all settle on the same entry:
    /// the most recent, and between equally old ones (entries from before
    /// times were recorded) the one further into the book.
    fn supersedes(&self, other: &Entry) -> bool {
        (self.updated, self.progress) > (other.updated, other.progress)
    }
    fn started(&self) -> bool {
        self.index > 0 || self.fraction > 0.0
    }
    /// Open long enough to have read a few pages, not just leafed through.
    fn really_read(&self) -> bool {
        self.seconds_read.map_or(true, |s| s >= MIN_READING_SECONDS)
    }
    fn add_time(&mut self, seconds: u32) {
        self.seconds_read = self.seconds_read.map(|s| s.saturating_add(seconds));
    }
}

#[derive(Serialize, Deserialize, Default)]
struct SharedFile {
    version: u32,
    device: String,
    entries: HashMap<String, Entry>,
}

#[derive(Default)]
struct Progress {
    /// What this device recorded.
    own: HashMap<String, Entry>,
    /// The newest entry per book from every other device.
    others: HashMap<String, Entry>,
    /// `own` has changes not yet written to the shared folder.
    unshared: bool,
    shared_ok: bool,
    last_shared_write: Option<Instant>,
    /// The book being read right now and when its time was last counted.
    session: Option<(String, Instant)>,
}

impl Progress {
    /// Seconds since the clock was last read for `book_id`, capped; zero if
    /// another book, or none, is being read.
    fn take_session_time(&self, book_id: &str) -> u32 {
        match &self.session {
            Some((id, since)) if id == book_id => since.elapsed().as_secs().min(MAX_PAGE_SECONDS) as u32,
            _ => 0,
        }
    }

    fn latest(&self, book_id: &str) -> Option<Entry> {
        match (self.own.get(book_id), self.others.get(book_id)) {
            (Some(a), Some(b)) => Some(if b.supersedes(a) { *b } else { *a }),
            (a, b) => a.or(b).copied(),
        }
    }
}

/// A folder of books plus a private state directory (metadata cache and a
/// local copy of reading progress).
///
/// Reading progress is shared between devices through the folder itself, in
/// the hidden directory `.rattonero`. Each device writes only its own file,
/// `reading-<device>.json`, and reads everyone else's; per book, the most
/// recent entry wins. Because no two devices ever write the same file, file
/// synchronisers (iCloud Drive, Dropbox, Syncthing, a network share) never
/// see a conflict. Nothing else in the folder is ever written.
#[derive(uniffi::Object)]
pub struct Library {
    root: PathBuf,
    state_dir: PathBuf,
    device: String,
    progress: Mutex<Progress>,
}

const SHARED_DIR: &str = ".rattonero";
const SHARED_VERSION: u32 = 1;
/// Files written by other devices are untrusted input too.
const MAX_SHARED_FILE: u64 = 8 << 20;
const MAX_SHARED_FILES: usize = 64;
/// Leafing quickly through pages is shared as one write, not ten; whatever
/// is held back goes out at the next `reload_progress` or `flush`.
const SHARED_WRITE_INTERVAL: Duration = Duration::from_secs(1);
/// A book joins "continue reading" once it has been read for this long:
/// about the time a few pages take.
const MIN_READING_SECONDS: u32 = 120;
/// A longer pause between page turns is a break, not reading; only this
/// much of it counts.
const MAX_PAGE_SECONDS: u64 = 300;

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// A random, stable name for this installation.
fn device_id(state_dir: &Path) -> String {
    let path = state_dir.join("device-id");
    if let Ok(id) = fs::read_to_string(&path) {
        let id = id.trim();
        if (8..=32).contains(&id.len()) && id.chars().all(|c| c.is_ascii_alphanumeric()) {
            return id.to_string();
        }
    }
    // `RandomState` is seeded from the operating system's random source.
    let random = std::collections::hash_map::RandomState::new().build_hasher().finish();
    let id = format!("{random:016x}");
    let _ = fs::write(&path, &id);
    id
}

fn load_json<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> T {
    fs::read(path).ok().and_then(|data| serde_json::from_slice(&data).ok()).unwrap_or_default()
}

fn save_json<T: Serialize>(path: &Path, value: &T) -> Result<(), CoreError> {
    let data = serde_json::to_vec(value).map_err(|e| CoreError::Io { message: e.to_string() })?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn format_of(path: &Path) -> Option<BookFormat> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "epub" => Some(BookFormat::Epub),
        "pdf" => Some(BookFormat::Pdf),
        _ => None,
    }
}

fn collect(dir: &Path, depth: usize, out: &mut Vec<(PathBuf, BookFormat)>) {
    if depth > MAX_DEPTH || out.len() >= MAX_BOOKS {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        // `file_type` does not follow symlinks, so links cannot lead the scan
        // out of the folder or into a cycle.
        let Ok(kind) = entry.file_type() else { continue };
        let path = entry.path();
        if kind.is_dir() {
            collect(&path, depth + 1, out);
        } else if kind.is_file() {
            if let Some(format) = format_of(&path) {
                out.push((path, format));
            }
        }
    }
}

fn describe(path: &Path, id: &str, format: BookFormat) -> BookInfo {
    let path_str = path.to_string_lossy().into_owned();
    let (meta, has_cover) = match format {
        BookFormat::Epub => epub::read_meta(&path_str).unwrap_or_default(),
        // PDF covers are rendered by the front end from the first page.
        BookFormat::Pdf => (pdf::read_meta(&path_str).unwrap_or_default(), false),
    };
    // A book we cannot parse still gets a place on the shelf, by file name.
    let title = meta
        .title
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| path.file_stem().map(|s| s.to_string_lossy().replace('_', " ")).unwrap_or_default());
    let author_sort = meta
        .author_sort
        .or_else(|| meta.authors.first().map(|a| sort::author_sort_name(a)))
        .unwrap_or_default();
    BookInfo {
        id: id.to_string(),
        path: path_str,
        format,
        title_sort: sort::title_sort_key(&title, meta.language.as_deref()),
        title,
        authors: meta.authors,
        author_sort,
        publisher: meta.publisher,
        year: meta.year,
        language: meta.language,
        has_cover,
    }
}

#[uniffi::export]
impl Library {
    /// `folder` is the shared folder of books; `state_dir` a writable,
    /// app-private directory. Reading progress is shared through `folder`
    /// when it is writable and kept private otherwise.
    #[uniffi::constructor]
    pub fn new(folder: String, state_dir: String) -> Result<Arc<Self>, CoreError> {
        let root = PathBuf::from(folder);
        if !root.is_dir() {
            return Err(CoreError::not_found(format!("folder {}", root.display())));
        }
        let state_dir = PathBuf::from(state_dir);
        fs::create_dir_all(&state_dir)?;
        let device = device_id(&state_dir);
        let progress = Progress {
            own: load_json(&state_dir.join("positions.json")),
            ..Progress::default()
        };
        let library = Library { root, state_dir, device, progress: Mutex::new(progress) };
        library.publish_if_needed();
        library.reload_progress();
        Ok(Arc::new(library))
    }

    /// Find every EPUB and PDF under the folder. Blocking: call it off the UI
    /// thread. Metadata of unchanged files comes from a cache.
    pub fn scan(&self) -> Vec<BookInfo> {
        let cache_path = self.state_dir.join("library-cache.json");
        let mut cache: Cache = load_json(&cache_path);
        if cache.version != CACHE_VERSION {
            cache = Cache::default();
        }
        let mut files = Vec::new();
        collect(&self.root, 0, &mut files);

        let mut fresh = Cache { version: CACHE_VERSION, entries: HashMap::new() };
        let mut books = Vec::with_capacity(files.len());
        let mut changed = files.len() != cache.entries.len();
        for (path, format) in files {
            let Ok(relative) = path.strip_prefix(&self.root) else { continue };
            let id = relative.to_string_lossy().replace('\\', "/");
            let Ok(meta) = fs::metadata(&path) else { continue };
            let modified = meta.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            let size = meta.len();
            let path_str = path.to_string_lossy();
            let info = match cache.entries.remove(&id) {
                Some(e) if e.modified == modified && e.size == size && e.info.path == path_str => e.info,
                _ => {
                    changed = true;
                    describe(&path, &id, format)
                }
            };
            books.push(info.clone());
            fresh.entries.insert(id, CacheEntry { modified, size, info });
        }
        if changed {
            // The cache is an optimisation; failing to write it is not an error.
            let _ = save_json(&cache_path, &fresh);
        }
        books
    }

    /// Cover image bytes of an EPUB, as stored in the book.
    pub fn cover(&self, book_id: String) -> Option<Vec<u8>> {
        let path = self.path_of(&book_id)?;
        epub::read_cover(&path.to_string_lossy()).ok()
    }

    /// Where the reader left this book, on whichever device read it last.
    pub fn position(&self, book_id: String) -> Option<ReadingPosition> {
        self.lock().latest(&book_id).map(|e| e.position())
    }

    /// Record where the reader is; call it when a book opens and on every
    /// page turn. The time between calls is counted as reading time. A
    /// change is written to the shared folder at once, unless one was
    /// written within the last second; then it follows at the next
    /// `reload_progress`. Other devices so follow page by page.
    pub fn set_position(&self, book_id: String, position: ReadingPosition) {
        let mut progress = self.lock();
        let known = progress.latest(&book_id);
        let reading = matches!(&progress.session, Some((id, _)) if *id == book_id);
        if known.is_some_and(|e| e.position() == position && !e.dismissed) {
            if !reading {
                // The book was just opened where it was left: start the clock.
                progress.session = Some((book_id, Instant::now()));
            }
            return;
        }
        let seconds = progress.take_session_time(&book_id);
        progress.session = Some((book_id.clone(), Instant::now()));
        let entry = Entry {
            index: position.index,
            fraction: position.fraction,
            progress: position.progress,
            finished: position.finished,
            dismissed: false,
            updated: now_ms(),
            // A book first seen here starts counting from zero; one with an
            // older, untimed entry stays "read".
            seconds_read: match known {
                Some(e) => e.seconds_read.map(|s| s.saturating_add(seconds)),
                None => Some(seconds),
            },
        };
        progress.own.insert(book_id, entry);
        self.save(&mut progress, false);
    }

    /// The books to offer under "continue reading": actually read for a
    /// couple of minutes (not merely opened or leafed through), not
    /// finished, not dismissed, most recently read first. `books` is the result of
    /// `scan`, so books no longer in the folder never show up.
    pub fn continue_reading(&self, books: Vec<BookInfo>, limit: u32) -> Vec<BookInfo> {
        let progress = self.lock();
        let mut reading: Vec<(u64, BookInfo)> = books
            .into_iter()
            .filter_map(|book| {
                let entry = progress.latest(&book.id)?;
                (entry.started() && entry.really_read() && !entry.finished && !entry.dismissed).then_some((entry.updated, book))
            })
            .collect();
        reading.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
        reading.into_iter().take(limit as usize).map(|(_, book)| book).collect()
    }

    /// Take a book off the "continue reading" list on every device, keeping
    /// its place. Reading on puts it back.
    pub fn dismiss_from_continue_reading(&self, book_id: String) {
        let mut progress = self.lock();
        let Some(mut entry) = progress.latest(&book_id) else { return };
        entry.dismissed = true;
        entry.updated = now_ms();
        progress.own.insert(book_id, entry);
        self.save(&mut progress, true);
    }

    /// Exchange progress with the other devices: write anything of ours
    /// still held back, read what they have written since the last call,
    /// and say whether that changed anything. Call it every few seconds
    /// while the app is in front; it is cheap.
    pub fn reload_progress(&self) -> bool {
        {
            let mut progress = self.lock();
            if progress.unshared {
                self.save(&mut progress, true);
            }
        }
        let mut others: HashMap<String, Entry> = HashMap::new();
        let own_name = self.shared_file_name();
        let mut files: Vec<PathBuf> = fs::read_dir(self.root.join(SHARED_DIR))
            .map(|dir| dir.flatten().map(|e| e.path()).collect())
            .unwrap_or_default();
        files.retain(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            name.starts_with("reading-") && name.ends_with(".json") && name != own_name
        });
        files.sort();
        for path in files.into_iter().take(MAX_SHARED_FILES) {
            if fs::metadata(&path).map(|m| m.len() > MAX_SHARED_FILE).unwrap_or(true) {
                continue;
            }
            let file: SharedFile = load_json(&path);
            for (book_id, entry) in file.entries {
                let newer = others.get(&book_id).map_or(true, |known| entry.supersedes(known));
                if newer && entry.fraction.is_finite() && entry.progress.is_finite() {
                    others.insert(book_id, entry);
                }
            }
        }
        let mut progress = self.lock();
        let changed = progress.others != others;
        progress.others = others;
        changed
    }

    /// Count the time spent on the page that is open and retry any write to
    /// the shared folder that failed. Call it when a book is closed and
    /// when the app goes to the background.
    pub fn flush(&self) {
        let mut progress = self.lock();
        // Count the time spent on the page that was open, and stop the clock.
        if let Some((book_id, _)) = progress.session.clone() {
            let seconds = progress.take_session_time(&book_id);
            progress.session = None;
            if let Some(mut entry) = progress.latest(&book_id).filter(|_| seconds > 0) {
                entry.add_time(seconds);
                progress.own.insert(book_id, entry);
                progress.unshared = true;
                let _ = save_json(&self.state_dir.join("positions.json"), &progress.own);
            }
        }
        if progress.unshared {
            self.save(&mut progress, true);
        }
    }

    /// Whether progress is reaching the shared folder. False when the folder
    /// is read-only; progress is then kept on this device alone.
    pub fn shares_progress(&self) -> bool {
        self.lock().shared_ok
    }
}

impl Library {
    fn lock(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn shared_file_name(&self) -> String {
        format!("reading-{}.json", self.device)
    }

    /// On opening a folder, make sure this device's file is there and up to
    /// date. This publishes progress made while the folder was read-only,
    /// and finds out straight away whether the folder can be written.
    fn publish_if_needed(&self) {
        let mut progress = self.lock();
        let path = self.root.join(SHARED_DIR).join(self.shared_file_name());
        let published: Option<SharedFile> = fs::read(&path).ok().and_then(|data| serde_json::from_slice(&data).ok());
        if published.is_some_and(|file| file.entries == progress.own) {
            progress.shared_ok = true;
        } else {
            self.save(&mut progress, true);
        }
    }

    /// Writes this device's progress to the private copy and, unless a
    /// write just happened and `now` is not set, to the shared folder.
    fn save(&self, progress: &mut Progress, now: bool) {
        let _ = save_json(&self.state_dir.join("positions.json"), &progress.own);
        progress.unshared = true;
        if !now && progress.last_shared_write.is_some_and(|at| at.elapsed() < SHARED_WRITE_INTERVAL) {
            return;
        }
        let dir = self.root.join(SHARED_DIR);
        let file = SharedFile { version: SHARED_VERSION, device: self.device.clone(), entries: progress.own.clone() };
        let written = fs::create_dir_all(&dir).is_ok() && save_json(&dir.join(self.shared_file_name()), &file).is_ok();
        progress.shared_ok = written;
        progress.unshared = !written;
        if !now {
            // Only page turns start the quiet second; an explicit exchange
            // must not delay the turn that follows it.
            progress.last_shared_write = Some(Instant::now());
        }
    }

    /// Pretend the current page has been open for `seconds`.
    #[cfg(test)]
    fn age_session(&self, seconds: u64) {
        let mut progress = self.lock();
        if let Some((_, since)) = &mut progress.session {
            *since -= Duration::from_secs(seconds);
        }
        if let Some(at) = &mut progress.last_shared_write {
            *at -= Duration::from_secs(seconds);
        }
    }

    /// Book ids are relative paths; refuse anything that could leave the folder.
    fn path_of(&self, book_id: &str) -> Option<PathBuf> {
        let relative = Path::new(book_id);
        relative.components().all(|c| matches!(c, Component::Normal(_))).then(|| self.root.join(relative))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::epub::tests::{sample_epub, temp_dir};

    #[test]
    fn scan_cover_and_positions() {
        let dir = temp_dir("library");
        let books_dir = dir.join("books");
        fs::create_dir_all(books_dir.join("sub")).unwrap();
        sample_epub(&books_dir.join("sub"), "calvino.epub");
        fs::write(books_dir.join("Broken_file.epub"), b"garbage").unwrap();
        fs::write(books_dir.join("notes.txt"), b"ignored").unwrap();
        fs::write(books_dir.join(".hidden.epub"), b"ignored").unwrap();
        let state = dir.join("state").to_string_lossy().into_owned();

        let library = Library::new(books_dir.to_string_lossy().into_owned(), state.clone()).unwrap();
        let mut books = library.scan();
        books.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(books.len(), 2);
        assert_eq!(books[0].title, "Broken file");
        let calvino = &books[1];
        assert_eq!(calvino.id, "sub/calvino.epub");
        assert_eq!(calvino.author_sort, "Calvino, Italo");
        assert_eq!(calvino.title_sort, "barone rampante");
        assert!(calvino.has_cover);
        assert_eq!(library.cover(calvino.id.clone()).unwrap(), b"not really a png");
        assert!(library.cover("../outside.epub".into()).is_none());
        // Second scan is served from the cache and agrees.
        let mut again = library.scan();
        again.sort_by(|a, b| a.id.cmp(&b.id));
        assert_eq!(again, books);

        let position = ReadingPosition { index: 3, fraction: 0.5, progress: 0.4, finished: false };
        library.set_position(calvino.id.clone(), position);
        let reopened = Library::new(books_dir.to_string_lossy().into_owned(), state).unwrap();
        assert_eq!(reopened.position(calvino.id.clone()), Some(position));
        // The hidden state directory is not mistaken for books.
        assert_eq!(reopened.scan().len(), 2);
        fs::remove_dir_all(dir).ok();
    }

    fn at(index: u32, fraction: f64) -> ReadingPosition {
        ReadingPosition { index, fraction, progress: 0.1 * index as f64, finished: false }
    }

    /// Open the book where it was left, read for three minutes, turn to `to`.
    fn read(library: &Library, book: &str, to: ReadingPosition) {
        let from = library.position(book.into()).unwrap_or(at(0, 0.0));
        library.set_position(book.into(), from);
        library.age_session(180);
        library.set_position(book.into(), to);
    }

    #[test]
    fn progress_is_shared_between_devices() {
        let dir = temp_dir("shared");
        let folder = dir.join("books");
        fs::create_dir_all(&folder).unwrap();
        sample_epub(&folder, "a.epub");
        sample_epub(&folder, "b.epub");
        sample_epub(&folder, "c.epub");
        let open = |name: &str| Library::new(folder.to_string_lossy().into_owned(), dir.join(name).to_string_lossy().into_owned()).unwrap();
        let mac = open("mac");
        let phone = open("phone");
        assert!(mac.shares_progress() && phone.shares_progress());
        let books = mac.scan();

        read(&mac, "a.epub", at(1, 0.5));
        // No flush needed: a page turn is shared at once...
        assert!(phone.reload_progress());
        assert!(!phone.reload_progress(), "nothing new the second time");
        assert_eq!(phone.position("a.epub".into()), Some(at(1, 0.5)));
        // ...and a burst of quick turns is completed by the next exchange.
        mac.set_position("a.epub".into(), at(1, 0.6));
        mac.set_position("a.epub".into(), at(1, 0.7));
        mac.set_position("a.epub".into(), at(1, 0.5));
        mac.reload_progress();
        phone.reload_progress();
        assert_eq!(phone.position("a.epub".into()), Some(at(1, 0.5)));

        // Reading on, on the phone, wins on the Mac: last reader wins.
        std::thread::sleep(Duration::from_millis(5));
        read(&phone, "a.epub", at(2, 0.25));
        std::thread::sleep(Duration::from_millis(5));
        read(&phone, "b.epub", at(1, 0.0));
        phone.flush();
        mac.reload_progress();
        assert_eq!(mac.position("a.epub".into()), Some(at(2, 0.25)));

        // Each device wrote only its own file.
        let mut files: Vec<String> = fs::read_dir(folder.join(SHARED_DIR)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        files.sort();
        assert_eq!(files.len(), 2);
        assert!(files.iter().all(|f| f.starts_with("reading-") && f.ends_with(".json")));

        // Most recent first. A book that was only opened, or leafed through
        // in seconds, is left out; so is a finished one.
        std::thread::sleep(Duration::from_millis(5));
        mac.set_position("c.epub".into(), at(0, 0.0));
        mac.set_position("c.epub".into(), at(1, 0.2));
        mac.set_position("c.epub".into(), at(3, 0.8));
        mac.flush();
        let ids = |l: &Library| l.continue_reading(books.clone(), 10).into_iter().map(|b| b.id).collect::<Vec<_>>();
        assert_eq!(ids(&mac), ["b.epub", "a.epub"]);
        assert_eq!(mac.continue_reading(books.clone(), 1).len(), 1);
        mac.set_position("b.epub".into(), ReadingPosition { finished: true, ..at(1, 0.9) });
        assert_eq!(ids(&mac), ["a.epub"]);

        // Dismissing travels to the other device; reading on undoes it.
        std::thread::sleep(Duration::from_millis(5));
        mac.dismiss_from_continue_reading("a.epub".into());
        phone.reload_progress();
        assert!(ids(&phone).is_empty());
        assert_eq!(phone.position("a.epub".into()), Some(at(2, 0.25)));
        std::thread::sleep(Duration::from_millis(5));
        phone.set_position("a.epub".into(), at(2, 0.5));
        assert_eq!(ids(&phone), ["a.epub"]);

        // Leafing does not count, but coming back and reading does; the time
        // on the last page is counted when the book is closed.
        mac.set_position("c.epub".into(), at(3, 0.8));
        mac.age_session(150);
        mac.flush();
        assert_eq!(ids(&mac), ["c.epub"]);

        // Entries from before times were recorded tie; both devices then
        // agree on the one further into the book.
        let legacy = |progress: f64| Entry { index: 4, fraction: 0.0, progress, finished: false, dismissed: false, updated: 0, seconds_read: None };
        mac.lock().own.insert("old.epub".into(), legacy(0.3));
        phone.lock().own.insert("old.epub".into(), legacy(0.4));
        mac.lock().unshared = true;
        phone.lock().unshared = true;
        mac.flush();
        phone.flush();
        mac.reload_progress();
        phone.reload_progress();
        assert_eq!(mac.position("old.epub".into()).unwrap().progress, 0.4);
        assert_eq!(phone.position("old.epub".into()).unwrap().progress, 0.4);

        // Garbage left in the shared directory is ignored, and the reading
        // time counted on the Mac reaches the phone.
        fs::write(folder.join(SHARED_DIR).join("reading-evil.json"), b"{not json").unwrap();
        phone.reload_progress();
        assert_eq!(ids(&phone), ["a.epub", "c.epub"]);
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn read_only_folder_keeps_progress_private() {
        let dir = temp_dir("readonly");
        let folder = dir.join("books");
        fs::create_dir_all(&folder).unwrap();
        // A file where the shared directory should be makes it unwritable.
        fs::write(folder.join(SHARED_DIR), b"").unwrap();
        let state = dir.join("state").to_string_lossy().into_owned();
        let library = Library::new(folder.to_string_lossy().into_owned(), state.clone()).unwrap();
        assert!(!library.shares_progress());
        library.set_position("a.epub".into(), at(1, 0.5));
        library.flush();
        let again = Library::new(folder.to_string_lossy().into_owned(), state.clone()).unwrap();
        assert_eq!(again.position("a.epub".into()), Some(at(1, 0.5)));
        assert!(!again.shares_progress());

        // Once the folder becomes writable, what was read meanwhile is
        // published as soon as the folder is opened.
        fs::remove_file(folder.join(SHARED_DIR)).unwrap();
        let writable = Library::new(folder.to_string_lossy().into_owned(), state).unwrap();
        assert!(writable.shares_progress());
        let other = Library::new(folder.to_string_lossy().into_owned(), dir.join("other").to_string_lossy().into_owned()).unwrap();
        assert_eq!(other.position("a.epub".into()), Some(at(1, 0.5)));
        fs::remove_dir_all(dir).ok();
    }
}
