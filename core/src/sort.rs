//! Library-style ordering: authors by surname, titles without their article.

use crate::model::{BookInfo, Shelf, ShelfOrder};
use std::cmp::Ordering;

/// ASCII-folded, lower-cased key so that "Éluard" files under E.
pub(crate) fn fold(s: &str) -> String {
    deunicode::deunicode(s)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn articles(language: Option<&str>) -> &'static [&'static str] {
    let lang = language.unwrap_or("en").to_ascii_lowercase();
    match lang.get(..2).unwrap_or("en") {
        "it" => &["il ", "lo ", "la ", "i ", "gli ", "le ", "un ", "uno ", "una ", "l'", "l\u{2019}", "un'", "un\u{2019}"],
        "fr" => &["le ", "la ", "les ", "un ", "une ", "des ", "l'", "l\u{2019}"],
        "de" => &["der ", "die ", "das ", "ein ", "eine "],
        "es" => &["el ", "la ", "los ", "las ", "un ", "una "],
        "pt" => &["o ", "a ", "os ", "as ", "um ", "uma "],
        "nl" => &["de ", "het ", "een "],
        _ => &["the ", "a ", "an "],
    }
}

pub(crate) fn title_sort_key(title: &str, language: Option<&str>) -> String {
    let lower = title.trim().to_lowercase();
    for article in articles(language) {
        if let Some(rest) = lower.strip_prefix(article) {
            if !rest.trim().is_empty() {
                return fold(rest);
            }
        }
    }
    fold(&lower)
}

/// "Italo Calvino" -> "Calvino, Italo". Names that already contain a comma,
/// or consist of a single word, are kept.
pub(crate) fn author_sort_name(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name.contains(',') {
        return name.to_string();
    }
    let words: Vec<&str> = name.split_whitespace().collect();
    if words.len() < 2 {
        return name.to_string();
    }
    // Keep nobiliary particles with the surname: "Ursula K. Le Guin".
    const PARTICLES: &[&str] = &["de", "del", "della", "di", "da", "van", "von", "der", "den", "le", "la", "du", "d'", "mc", "mac", "st.", "ten", "ter"];
    let mut split = words.len() - 1;
    while split > 1 && PARTICLES.contains(&words[split - 1].to_lowercase().as_str()) {
        split -= 1;
    }
    format!("{}, {}", words[split..].join(" "), words[..split].join(" "))
}

fn initial(key: &str) -> String {
    match fold(key).chars().next() {
        Some(c) if c.is_ascii_alphabetic() => c.to_ascii_uppercase().to_string(),
        Some(_) => "#".to_string(),
        None => UNKNOWN.to_string(),
    }
}

const UNKNOWN: &str = "Unknown";

fn by_author(a: &BookInfo, b: &BookInfo) -> Ordering {
    let ka = fold(&a.author_sort);
    let kb = fold(&b.author_sort);
    // Books without an author go last.
    ka.is_empty()
        .cmp(&kb.is_empty())
        .then_with(|| ka.cmp(&kb))
        .then_with(|| a.title_sort.cmp(&b.title_sort))
        .then_with(|| a.year.unwrap_or(i32::MAX).cmp(&b.year.unwrap_or(i32::MAX)))
}

fn by_title(a: &BookInfo, b: &BookInfo) -> Ordering {
    a.title_sort.cmp(&b.title_sort).then_with(|| by_author(a, b))
}

/// Sort `books` and split them into labelled shelves.
#[uniffi::export]
pub fn arrange(books: Vec<BookInfo>, order: ShelfOrder) -> Vec<Shelf> {
    let mut books = books;
    let label_of: Box<dyn Fn(&BookInfo) -> String> = match order {
        ShelfOrder::Author => {
            books.sort_by(by_author);
            Box::new(|b| initial(&b.author_sort))
        }
        ShelfOrder::Title => {
            books.sort_by(by_title);
            Box::new(|b| initial(&b.title_sort))
        }
        ShelfOrder::Publisher => {
            books.sort_by(|a, b| {
                let ka = a.publisher.as_deref().map(fold).unwrap_or_default();
                let kb = b.publisher.as_deref().map(fold).unwrap_or_default();
                ka.is_empty().cmp(&kb.is_empty()).then_with(|| ka.cmp(&kb)).then_with(|| by_author(a, b))
            });
            Box::new(|b| match b.publisher.as_deref().map(str::trim) {
                Some(p) if !p.is_empty() => p.to_string(),
                _ => UNKNOWN.to_string(),
            })
        }
        ShelfOrder::Year => {
            books.sort_by(|a, b| {
                a.year.unwrap_or(i32::MAX).cmp(&b.year.unwrap_or(i32::MAX)).then_with(|| by_author(a, b))
            });
            Box::new(|b| b.year.map(|y| y.to_string()).unwrap_or_else(|| UNKNOWN.to_string()))
        }
    };

    let mut shelves: Vec<Shelf> = Vec::new();
    for book in books {
        let label = label_of(&book);
        // Publishers differing only in case or accents share a shelf.
        match shelves.last_mut() {
            Some(shelf) if fold(&shelf.label) == fold(&label) => shelf.books.push(book),
            _ => shelves.push(Shelf { label, books: vec![book] }),
        }
    }
    shelves
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::BookFormat;

    fn book(title: &str, author: &str, publisher: Option<&str>, year: Option<i32>, lang: &str) -> BookInfo {
        BookInfo {
            id: title.into(),
            path: title.into(),
            format: BookFormat::Epub,
            title: title.into(),
            authors: vec![author.into()],
            author_sort: author_sort_name(author),
            title_sort: title_sort_key(title, Some(lang)),
            publisher: publisher.map(Into::into),
            year,
            language: Some(lang.into()),
            has_cover: false,
        }
    }

    #[test]
    fn names_and_titles() {
        assert_eq!(author_sort_name("Italo Calvino"), "Calvino, Italo");
        assert_eq!(author_sort_name("Ursula K. Le Guin"), "Le Guin, Ursula K.");
        assert_eq!(author_sort_name("Omero"), "Omero");
        assert_eq!(author_sort_name("Eco, Umberto"), "Eco, Umberto");
        assert_eq!(title_sort_key("The Left Hand of Darkness", Some("en")), "left hand of darkness");
        assert_eq!(title_sort_key("Il barone rampante", Some("it")), "barone rampante");
        assert_eq!(title_sort_key("L\u{2019}isola del giorno prima", Some("it-IT")), "isola del giorno prima");
        assert_eq!(title_sort_key("A", Some("en")), "a");
    }

    #[test]
    fn shelves() {
        let books = vec![
            book("Il nome della rosa", "Umberto Eco", Some("Bompiani"), Some(1980), "it"),
            book("Il barone rampante", "Italo Calvino", Some("Einaudi"), Some(1957), "it"),
            book("Le città invisibili", "Italo Calvino", Some("einaudi"), Some(1972), "it"),
            book("Anonymous notes", "", None, None, "en"),
        ];
        let s = arrange(books.clone(), ShelfOrder::Author);
        assert_eq!(s.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["C", "E", "Unknown"]);
        assert_eq!(s[0].books[0].title, "Il barone rampante");
        let s = arrange(books.clone(), ShelfOrder::Title);
        assert_eq!(s.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["A", "B", "C", "N"]);
        let s = arrange(books.clone(), ShelfOrder::Publisher);
        assert_eq!(s.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["Bompiani", "Einaudi", "Unknown"]);
        assert_eq!(s[1].books.len(), 2);
        let s = arrange(books, ShelfOrder::Year);
        assert_eq!(s.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(), ["1957", "1972", "1980", "Unknown"]);
    }
}
