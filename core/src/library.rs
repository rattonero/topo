//! The bookshelf: scanning a folder and remembering reading positions.

use crate::error::CoreError;
use crate::model::{BookFormat, BookInfo, ReadingPosition};
use crate::{epub, pdf, sort};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;

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

/// A folder of books plus a private state directory (metadata cache and
/// reading positions). The library folder itself is only ever read.
#[derive(uniffi::Object)]
pub struct Library {
    root: PathBuf,
    state_dir: PathBuf,
    positions: Mutex<HashMap<String, ReadingPosition>>,
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
    /// app-private directory.
    #[uniffi::constructor]
    pub fn new(folder: String, state_dir: String) -> Result<Arc<Self>, CoreError> {
        let root = PathBuf::from(folder);
        if !root.is_dir() {
            return Err(CoreError::not_found(format!("folder {}", root.display())));
        }
        let state_dir = PathBuf::from(state_dir);
        fs::create_dir_all(&state_dir)?;
        let positions = load_json(&state_dir.join("positions.json"));
        Ok(Arc::new(Library { root, state_dir, positions: Mutex::new(positions) }))
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

    pub fn position(&self, book_id: String) -> Option<ReadingPosition> {
        self.positions.lock().unwrap_or_else(|e| e.into_inner()).get(&book_id).copied()
    }

    pub fn set_position(&self, book_id: String, position: ReadingPosition) {
        let mut positions = self.positions.lock().unwrap_or_else(|e| e.into_inner());
        if positions.get(&book_id) == Some(&position) {
            return;
        }
        positions.insert(book_id, position);
        let _ = save_json(&self.state_dir.join("positions.json"), &*positions);
    }
}

impl Library {
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

        let position = ReadingPosition { index: 3, fraction: 0.5, progress: 0.4 };
        library.set_position(calvino.id.clone(), position);
        let reopened = Library::new(books_dir.to_string_lossy().into_owned(), state).unwrap();
        assert_eq!(reopened.position(calvino.id.clone()), Some(position));
        fs::remove_dir_all(dir).ok();
    }
}
