//! PDF metadata. Rendering is left to the platform (PDFKit, Poppler, ...).

use crate::epub::Meta;
use crate::error::CoreError;

/// Do not slurp enormous scans into memory just to learn their title.
const MAX_PDF: u64 = 512 << 20;

pub(crate) fn read_meta(path: &str) -> Result<Meta, CoreError> {
    if std::fs::metadata(path)?.len() > MAX_PDF {
        return Err(CoreError::invalid("file too large to inspect"));
    }
    let info = lopdf::Document::load_metadata(path).map_err(|e| CoreError::invalid(e.to_string()))?;
    let clean = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let year = info.creation_date.as_deref().and_then(|d| {
        let digits: String = d.chars().skip_while(|c| !c.is_ascii_digit()).take(4).collect();
        digits.parse::<i32>().ok().filter(|y| (1000..=2200).contains(y))
    });
    // Several authors are conventionally separated by ';' or '&'.
    let authors = clean(info.author)
        .map(|a| a.split([';', '&']).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    Ok(Meta { title: clean(info.title), authors, author_sort: None, publisher: None, year, language: None })
}
