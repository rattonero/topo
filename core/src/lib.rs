//! Rattonero core: everything about books that is not user interface.
//!
//! The crate is UI-toolkit agnostic. Front ends (SwiftUI today; GTK4, Qt and
//! WinUI later) talk to it through the UniFFI-exported API below, or, for Rust
//! front ends, by depending on the crate directly.
//!
//! Responsibilities:
//! * scanning a folder for EPUB and PDF files and reading their metadata,
//! * arranging books on shelves the way a library would,
//! * opening EPUBs and serving their content, sanitised, to a web view,
//! * remembering reading progress and sharing it between devices.
//!
//! Book files are untrusted input. All parsing happens here, in safe Rust,
//! with size limits; chapter markup is rebuilt from an allow-list before any
//! renderer sees it.

mod epub;
mod error;
mod library;
mod model;
mod pdf;
mod sanitize;
mod sort;

pub use epub::EpubBook;
pub use error::CoreError;
pub use library::Library;
pub use model::*;
pub use sort::arrange;

uniffi::setup_scaffolding!();

/// URL scheme front ends should register to serve book content from
/// [`EpubBook::resource`]. Chapter documents carry a Content-Security-Policy
/// that only allows loads from this scheme.
pub const URL_SCHEME: &str = "rattonero-book";

/// Reserved path under which the reader stylesheet is served.
pub(crate) const READER_CSS_PATH: &str = ".rattonero/reader.css";

#[uniffi::export]
pub fn url_scheme() -> String {
    URL_SCHEME.to_string()
}

/// The pagination script. Front ends inject it into the web view themselves
/// (as a privileged user script) so that scripting can stay disabled for book
/// content. See `reader.js` for the small message protocol.
#[uniffi::export]
pub fn reader_script() -> String {
    include_str!("reader.js").to_string()
}

/// Content-Security-Policy applied to every chapter; front ends may also send
/// it as a response header.
#[uniffi::export]
pub fn content_security_policy() -> String {
    sanitize::csp()
}
