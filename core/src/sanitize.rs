//! Turns an untrusted XHTML chapter into a document that is safe to hand to
//! a web view.
//!
//! Defence in depth:
//! 1. the body is rebuilt from an allow-list of elements and attributes
//!    (no scripts, event handlers, forms, frames, objects);
//! 2. the head is rebuilt from scratch, keeping only stylesheets;
//! 3. a Content-Security-Policy forbids scripts and any load that is not a
//!    resource of the book itself, so nothing can phone home.
//!
//! Front ends should additionally disable content scripting in the web view.

use crate::{READER_CSS_PATH, URL_SCHEME};
use std::collections::HashSet;
use std::sync::OnceLock;

pub(crate) fn csp() -> String {
    format!(
        "default-src 'none'; img-src 'self' {s}: data:; media-src 'self' {s}:; \
         style-src 'self' {s}: 'unsafe-inline'; font-src 'self' {s}: data:; \
         base-uri 'none'; form-action 'none'",
        s = URL_SCHEME
    )
}

fn cleaner() -> &'static ammonia::Builder<'static> {
    static CLEANER: OnceLock<ammonia::Builder<'static>> = OnceLock::new();
    CLEANER.get_or_init(|| {
        let mut b = ammonia::Builder::default();
        b.add_tags([
            "article", "aside", "section", "nav", "main", "header", "footer", "hgroup", "figure", "figcaption",
            "details", "summary", "mark", "ruby", "rt", "rp", "bdi", "bdo", "wbr", "time", "center", "font",
            "big", "tt", "strike", "u", "s", "small", "sub", "sup", "dl", "dt", "dd", "caption", "colgroup",
            "col", "thead", "tbody", "tfoot", "svg", "image",
        ]);
        b.add_generic_attributes(["class", "id", "style", "dir", "lang", "title"]);
        b.add_tag_attributes("a", ["name"]);
        b.add_tag_attributes("ol", ["start", "type"]);
        b.add_tag_attributes("font", ["size", "face"]);
        b.add_tag_attributes("svg", ["viewBox", "width", "height", "preserveAspectRatio", "version"]);
        b.add_tag_attributes("image", ["href", "xlink:href", "x", "y", "width", "height", "preserveAspectRatio"]);
        // Relative URLs (resources of the book) pass through untouched.
        b.url_schemes(HashSet::from(["http", "https", "mailto", "data"]));
        b.link_rel(None);
        b
    })
}

fn escape_attr(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

/// Value of attribute `name` in the text of a start tag (case-insensitive,
/// also matching `xml:lang` for `lang`).
fn tag_attr(tag: &str, name: &str) -> Option<String> {
    let bytes = tag.as_bytes();
    let mut i = 0;
    // Skip the element name.
    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' && bytes[i] != b'>' {
            i += 1;
        }
        let key = &tag[start..i];
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = "";
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let quote = bytes[i];
                i += 1;
                let v = i;
                while i < bytes.len() && bytes[i] != quote {
                    i += 1;
                }
                value = &tag[v..i];
                i += 1;
            } else {
                let v = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>' {
                    i += 1;
                }
                value = &tag[v..i];
            }
        }
        if key.is_empty() {
            i += 1;
            continue;
        }
        if key.eq_ignore_ascii_case(name) {
            return Some(value.to_string());
        }
    }
    None
}

/// Index just past the `>` closing the tag that starts at `from`, honouring
/// quoted attribute values.
fn tag_end(s: &str, from: usize) -> Option<usize> {
    let mut quote = 0u8;
    for (i, &b) in s.as_bytes()[from..].iter().enumerate() {
        match (quote, b) {
            (0, b'"') | (0, b'\'') => quote = b,
            (0, b'>') => return Some(from + i + 1),
            (q, b) if q == b => quote = 0,
            _ => {}
        }
    }
    None
}

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source", "track", "wbr",
];

/// XHTML allows `<a id="x"/>` and `<div/>`; an HTML parser reads those as
/// unclosed start tags and swallows what follows. Expand them to
/// `<a id="x"></a>` before parsing as HTML.
fn expand_self_closing(xhtml: &str) -> String {
    let bytes = xhtml.as_bytes();
    let mut out = String::with_capacity(xhtml.len() + 64);
    let mut i = 0;
    while let Some(off) = xhtml[i..].find('<') {
        let lt = i + off;
        out.push_str(&xhtml[i..lt]);
        if xhtml[lt..].starts_with("<!--") {
            let end = xhtml[lt..].find("-->").map(|e| lt + e + 3).unwrap_or(xhtml.len());
            out.push_str(&xhtml[lt..end]);
            i = end;
            continue;
        }
        let is_start_tag = bytes.get(lt + 1).is_some_and(u8::is_ascii_alphabetic);
        let Some(end) = (if is_start_tag { tag_end(xhtml, lt) } else { None }) else {
            out.push('<');
            i = lt + 1;
            continue;
        };
        let tag = &xhtml[lt..end];
        let name_len = tag[1..].find(|c: char| c.is_ascii_whitespace() || c == '/' || c == '>').unwrap_or(0);
        let name = &tag[1..1 + name_len];
        if tag.ends_with("/>") && !VOID.contains(&name.to_ascii_lowercase().as_str()) {
            out.push_str(tag[..tag.len() - 2].trim_end());
            out.push_str("></");
            out.push_str(name);
            out.push('>');
        } else {
            out.push_str(tag);
        }
        i = end;
    }
    out.push_str(&xhtml[i..]);
    out
}

struct Parts<'a> {
    html_tag: &'a str,
    head: &'a str,
    body_tag: &'a str,
    body: &'a str,
}

fn split(doc: &str) -> Parts<'_> {
    // ASCII lower-casing keeps byte offsets identical.
    let lower = doc.to_ascii_lowercase();
    let find_tag = |name: &str, from: usize| -> Option<(usize, usize)> {
        let mut at = from;
        loop {
            let start = at + lower[at..].find(&format!("<{name}"))?;
            let after = lower.as_bytes().get(start + 1 + name.len()).copied();
            if matches!(after, Some(b'>') | Some(b'/')) || after.is_some_and(|b| b.is_ascii_whitespace()) {
                return Some((start, tag_end(doc, start)?));
            }
            at = start + 1;
        }
    };
    let html_tag = find_tag("html", 0).map(|(s, e)| &doc[s..e]).unwrap_or("");
    let head = find_tag("head", 0)
        .and_then(|(_, e)| lower[e..].find("</head").map(|close| &doc[e..e + close]))
        .unwrap_or("");
    match find_tag("body", 0) {
        Some((s, e)) => {
            let close = lower.rfind("</body").filter(|c| *c >= e).unwrap_or(doc.len());
            Parts { html_tag, head, body_tag: &doc[s..e], body: &doc[e..close] }
        }
        None => Parts { html_tag, head, body_tag: "", body: doc },
    }
}

/// Stylesheet links and inline styles of the original head, re-emitted.
fn head_styles(head: &str) -> String {
    let lower = head.to_ascii_lowercase();
    let mut out = String::new();
    let mut i = 0;
    while let Some(off) = lower[i..].find('<') {
        let at = i + off;
        if lower[at..].starts_with("<link") {
            let Some(end) = tag_end(head, at) else { break };
            let tag = &head[at..end];
            let rel = tag_attr(tag, "rel").unwrap_or_default().to_ascii_lowercase();
            let href = tag_attr(tag, "href").unwrap_or_default();
            // Only plain relative references: resources of the book.
            let relative = !href.is_empty() && !href.contains(':') && !href.starts_with("//");
            if rel.split_whitespace().any(|r| r == "stylesheet") && !rel.contains("alternate") && relative {
                out.push_str(&format!("<link rel=\"stylesheet\" href=\"{}\">\n", escape_attr(&href)));
            }
            i = end;
        } else if lower[at..].starts_with("<style") {
            let Some(open_end) = tag_end(head, at) else { break };
            let close = lower[open_end..].find("</style").map(|c| open_end + c).unwrap_or(head.len());
            let css = head[open_end..close].replace("<![CDATA[", "").replace("]]>", "").replace("</", "<\\/");
            out.push_str("<style>");
            out.push_str(&css);
            out.push_str("</style>\n");
            i = close;
        } else {
            i = at + 1;
        }
    }
    out
}

pub(crate) fn chapter_document(xhtml: &str, book_language: Option<&str>) -> String {
    let parts = split(xhtml);
    let body = cleaner().clean(&expand_self_closing(parts.body)).to_string();

    let lang = tag_attr(parts.html_tag, "lang")
        .or_else(|| tag_attr(parts.html_tag, "xml:lang"))
        .or_else(|| tag_attr(parts.body_tag, "lang"))
        .or_else(|| tag_attr(parts.body_tag, "xml:lang"))
        .or_else(|| book_language.map(str::to_string))
        .filter(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    let lang_attr = lang.map(|l| format!(" lang=\"{l}\"")).unwrap_or_default();
    let mut body_attrs = String::new();
    for name in ["class", "id", "dir"] {
        if let Some(value) = tag_attr(parts.body_tag, name).filter(|v| !v.is_empty()) {
            body_attrs.push_str(&format!(" {name}=\"{}\"", escape_attr(&value)));
        }
    }

    format!(
        "<!DOCTYPE html>\n<html{lang_attr}>\n<head>\n<meta charset=\"utf-8\">\n\
         <meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1, maximum-scale=1, user-scalable=no\">\n\
         {styles}<link rel=\"stylesheet\" href=\"/{reader_css}\">\n</head>\n<body{body_attrs}>{body}</body>\n</html>\n",
        csp = csp(),
        styles = head_styles(parts.head),
        reader_css = READER_CSS_PATH,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_closing() {
        assert_eq!(expand_self_closing(r#"<p>a<a id="x"/>b<br/><img src="a>b.png"/></p>"#), r#"<p>a<a id="x"></a>b<br/><img src="a>b.png"/></p>"#);
        assert_eq!(expand_self_closing("<div class='a' />x <!-- <b/> --> 1 < 2"), "<div class='a'></div>x <!-- <b/> --> 1 < 2");
    }

    #[test]
    fn strips_active_content() {
        let doc = r#"<html lang="en"><head><style>p{margin:0}</style><link rel="stylesheet" href="http://evil.example/x.css"/></head>
            <body><p style="text-align:center">Hi <a href="javascript:alert(1)">x</a> <a href="note.xhtml#n1">note</a></p>
            <iframe src="http://evil.example"></iframe><form action="/x"><input name="q"></form>
            <svg viewBox="0 0 10 10"><image xlink:href="cover.jpg" width="10" height="10"/><script>1</script></svg>
            <object data="x.swf"></object><img src="http://evil.example/track.gif"></body></html>"#;
        let out = chapter_document(doc, None);
        for bad in ["javascript:", "<iframe", "<form", "<input", "<script", "<object", "evil.example/x.css"] {
            assert!(!out.contains(bad), "{bad} survived in {out}");
        }
        assert!(out.contains("<html lang=\"en\">"));
        assert!(out.contains("<style>p{margin:0}</style>"));
        assert!(out.contains(r#"style="text-align:center""#));
        assert!(out.contains(r#"href="note.xhtml#n1""#));
        assert!(out.contains("cover.jpg"), "{out}");
        assert!(out.contains("Content-Security-Policy"));
    }

    #[test]
    fn bare_fragment() {
        let out = chapter_document("<p>Just text</p>", Some("it"));
        assert!(out.contains("<html lang=\"it\">") && out.contains("<body><p>Just text</p></body>"));
    }
}
