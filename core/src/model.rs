use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, uniffi::Enum)]
pub enum BookFormat {
    Epub,
    Pdf,
}

/// How the bookshelf is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, uniffi::Enum)]
pub enum ShelfOrder {
    Author,
    Title,
    Publisher,
    Year,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, uniffi::Record)]
pub struct BookInfo {
    /// Stable identifier: the path relative to the library folder.
    pub id: String,
    /// Absolute path of the file.
    pub path: String,
    pub format: BookFormat,
    pub title: String,
    pub authors: Vec<String>,
    /// Catalogue form of the main author, e.g. "Calvino, Italo".
    pub author_sort: String,
    /// Title without leading article, folded for sorting.
    pub title_sort: String,
    pub publisher: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
    pub has_cover: bool,
}

/// One labelled run of books, e.g. all authors under "C".
#[derive(Debug, Clone, PartialEq, Eq, Hash, uniffi::Record)]
pub struct Shelf {
    pub label: String,
    pub books: Vec<BookInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, uniffi::Record)]
pub struct TocEntry {
    pub title: String,
    /// Path of the document inside the EPUB.
    pub path: String,
    pub fragment: Option<String>,
    pub depth: u32,
    pub spine_index: Option<u32>,
}

#[derive(Debug, Clone, uniffi::Record)]
pub struct Resource {
    pub data: Vec<u8>,
    pub mime: String,
}

/// Where the reader was: a spine item (EPUB) or page (PDF) and how far into it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, uniffi::Record)]
pub struct ReadingPosition {
    pub index: u32,
    /// 0.0 ..= 1.0 within the spine item; unused for PDF.
    pub fraction: f64,
    /// 0.0 ..= 1.0 through the whole book, for display on the shelf.
    pub progress: f64,
    /// The reader is on the last page.
    pub finished: bool,
}
