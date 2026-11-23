//! EPUB container, package and navigation parsing.

use crate::error::CoreError;
use crate::model::{Resource, TocEntry};
use crate::{sanitize, READER_CSS_PATH};
use percent_encoding::percent_decode_str;
use roxmltree::{Document, Node, ParsingOptions};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek};
use std::sync::{Arc, Mutex};
use zip::ZipArchive;

// Book files are untrusted: every read out of the archive is capped so a
// crafted "zip bomb" cannot exhaust memory.
const MAX_XML: u64 = 8 << 20;
const MAX_CHAPTER: u64 = 16 << 20;
const MAX_RESOURCE: u64 = 64 << 20;
const MAX_TOC_ENTRIES: usize = 5000;
const MAX_TOC_DEPTH: u32 = 12;

#[derive(Debug, Default, Clone)]
pub(crate) struct Meta {
    pub title: Option<String>,
    pub authors: Vec<String>,
    pub author_sort: Option<String>,
    pub publisher: Option<String>,
    pub year: Option<i32>,
    pub language: Option<String>,
}

#[derive(Debug, Default)]
pub(crate) struct Package {
    pub meta: Meta,
    /// path inside the archive -> media type
    pub media_types: HashMap<String, String>,
    pub spine: Vec<String>,
    pub cover_path: Option<String>,
    pub nav_path: Option<String>,
    pub ncx_path: Option<String>,
}

pub(crate) fn read_entry<R: Read + Seek>(zip: &mut ZipArchive<R>, name: &str, limit: u64) -> Result<Vec<u8>, CoreError> {
    let file = zip.by_name(name).map_err(|e| match e {
        zip::result::ZipError::FileNotFound => CoreError::not_found(name),
        other => other.into(),
    })?;
    if file.size() > limit {
        return Err(CoreError::invalid(format!("{name} is too large")));
    }
    let mut data = Vec::with_capacity(file.size().min(1 << 20) as usize);
    // The declared size can lie; `take` enforces the cap on real output.
    file.take(limit + 1).read_to_end(&mut data)?;
    if data.len() as u64 > limit {
        return Err(CoreError::invalid(format!("{name} is too large")));
    }
    Ok(data)
}

pub(crate) fn decode_text(data: &[u8]) -> String {
    if data.starts_with(&[0xFF, 0xFE]) || data.starts_with(&[0xFE, 0xFF]) {
        let le = data[0] == 0xFF;
        let units: Vec<u16> = data[2..]
            .chunks_exact(2)
            .map(|c| if le { u16::from_le_bytes([c[0], c[1]]) } else { u16::from_be_bytes([c[0], c[1]]) })
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    String::from_utf8_lossy(data).into_owned()
}

fn parse_xml(text: &str) -> Result<Document<'_>, CoreError> {
    let options = ParsingOptions { allow_dtd: true, ..ParsingOptions::default() };
    Ok(Document::parse_with_options(text, options)?)
}

/// Resolve `href` against the directory of the referring file. Returns a
/// normalised archive path (never escaping the root) and the fragment.
pub(crate) fn resolve(base_dir: &str, href: &str) -> (String, Option<String>) {
    let (href, fragment) = match href.split_once('#') {
        Some((h, f)) => (h, Some(percent_decode_str(f).decode_utf8_lossy().into_owned())),
        None => (href, None),
    };
    let href = href.split('?').next().unwrap_or("");
    let decoded = percent_decode_str(href).decode_utf8_lossy();
    let joined = if decoded.starts_with('/') || base_dir.is_empty() {
        decoded.to_string()
    } else {
        format!("{base_dir}/{decoded}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    (parts.join("/"), fragment.filter(|f| !f.is_empty()))
}

fn dir_of(path: &str) -> &str {
    path.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn text_of(node: Node) -> String {
    let mut out = String::new();
    for n in node.descendants().filter(|n| n.is_text()) {
        out.push_str(n.text().unwrap_or(""));
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn attr<'a>(node: Node<'a, '_>, local: &str) -> Option<&'a str> {
    node.attributes().find(|a| a.name() == local).map(|a| a.value())
}

fn child<'a, 'i>(node: Node<'a, 'i>, name: &str) -> Option<Node<'a, 'i>> {
    node.children().find(|n| n.is_element() && n.tag_name().name() == name)
}

fn parse_year(date: &str) -> Option<i32> {
    let bytes = date.as_bytes();
    (0..bytes.len().saturating_sub(3)).find_map(|i| {
        let slice = &bytes[i..i + 4];
        if !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        let year: i32 = std::str::from_utf8(slice).ok()?.parse().ok()?;
        (1000..=2200).contains(&year).then_some(year)
    })
}

fn is_html(media_type: &str, path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    media_type == "application/xhtml+xml"
        || media_type == "text/html"
        || lower.ends_with(".xhtml")
        || lower.ends_with(".html")
        || lower.ends_with(".htm")
}

fn mime_for_extension(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "xhtml" | "html" | "htm" => "application/xhtml+xml",
        "css" => "text/css",
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "mp3" => "audio/mpeg",
        "m4a" | "mp4" => "video/mp4",
        _ => "application/octet-stream",
    }
}

pub(crate) fn read_package<R: Read + Seek>(zip: &mut ZipArchive<R>) -> Result<Package, CoreError> {
    let container = decode_text(&read_entry(zip, "META-INF/container.xml", MAX_XML)?);
    let opf_path = {
        let doc = parse_xml(&container)?;
        let rootfile = doc
            .descendants()
            .find(|n| n.is_element() && n.tag_name().name() == "rootfile")
            .and_then(|n| attr(n, "full-path"))
            .ok_or_else(|| CoreError::invalid("no package document"))?;
        resolve("", rootfile).0
    };
    let opf = decode_text(&read_entry(zip, &opf_path, MAX_XML)?);
    parse_package(&opf, dir_of(&opf_path))
}

fn parse_package(opf: &str, base: &str) -> Result<Package, CoreError> {
    let doc = parse_xml(opf)?;
    let root = doc.root_element();
    let mut package = Package::default();
    let mut cover_id: Option<String> = None;

    struct Creator {
        id: Option<String>,
        name: String,
        file_as: Option<String>,
        role: Option<String>,
    }
    let mut creators: Vec<Creator> = Vec::new();
    let mut dates: Vec<(bool, String)> = Vec::new();

    if let Some(metadata) = child(root, "metadata") {
        let nodes: Vec<Node> = metadata.descendants().filter(|n| n.is_element()).collect();
        for node in &nodes {
            let text = text_of(*node);
            match node.tag_name().name() {
                "title" if package.meta.title.is_none() && !text.is_empty() => package.meta.title = Some(text),
                "creator" if !text.is_empty() => creators.push(Creator {
                    id: attr(*node, "id").map(str::to_string),
                    name: text,
                    file_as: attr(*node, "file-as").map(str::to_string),
                    role: attr(*node, "role").map(str::to_string),
                }),
                "publisher" if package.meta.publisher.is_none() && !text.is_empty() => package.meta.publisher = Some(text),
                "language" if package.meta.language.is_none() && !text.is_empty() => package.meta.language = Some(text),
                "date" if !text.is_empty() => {
                    let event = attr(*node, "event").unwrap_or("");
                    // A file's modification date is not when the book was published.
                    if event != "modification" {
                        dates.push((event.contains("publication"), text));
                    }
                }
                "meta" => {
                    if attr(*node, "name") == Some("cover") {
                        cover_id = attr(*node, "content").map(str::to_string);
                    }
                }
                _ => {}
            }
        }
        // EPUB 3 refinements: <meta refines="#id" property="file-as|role">.
        for node in &nodes {
            if node.tag_name().name() != "meta" {
                continue;
            }
            let (Some(refines), Some(property)) = (attr(*node, "refines"), attr(*node, "property")) else { continue };
            let id = refines.trim_start_matches('#');
            if let Some(creator) = creators.iter_mut().find(|c| c.id.as_deref() == Some(id)) {
                match property {
                    "file-as" => creator.file_as = Some(text_of(*node)),
                    "role" => creator.role = Some(text_of(*node)),
                    _ => {}
                }
            }
        }
    }

    // Translators, editors and illustrators are creators too; keep authors.
    let has_authors = creators.iter().any(|c| matches!(c.role.as_deref(), None | Some("aut")));
    if has_authors {
        creators.retain(|c| matches!(c.role.as_deref(), None | Some("aut")));
    }
    package.meta.author_sort = creators.first().and_then(|c| c.file_as.clone()).filter(|s| !s.trim().is_empty());
    package.meta.authors = creators.into_iter().map(|c| c.name).collect();
    dates.sort_by_key(|(is_publication, _)| !*is_publication);
    package.meta.year = dates.iter().find_map(|(_, d)| parse_year(d));

    let mut ids: HashMap<String, String> = HashMap::new();
    let mut cover_by_property = None;
    let mut cover_by_name = None;
    if let Some(manifest) = child(root, "manifest") {
        for item in manifest.children().filter(|n| n.is_element() && n.tag_name().name() == "item") {
            let Some(href) = attr(item, "href") else { continue };
            let path = resolve(base, href).0;
            let media_type = attr(item, "media-type").unwrap_or("").trim().to_ascii_lowercase();
            let properties = attr(item, "properties").unwrap_or("");
            let id = attr(item, "id").unwrap_or("");
            if properties.split_whitespace().any(|p| p == "nav") {
                package.nav_path = Some(path.clone());
            }
            if media_type == "application/x-dtbncx+xml" && package.ncx_path.is_none() {
                package.ncx_path = Some(path.clone());
            }
            if media_type.starts_with("image/") {
                if properties.split_whitespace().any(|p| p == "cover-image") {
                    cover_by_property = Some(path.clone());
                } else if cover_by_name.is_none() && (id.to_ascii_lowercase().contains("cover") || path.to_ascii_lowercase().contains("cover")) {
                    cover_by_name = Some(path.clone());
                }
            }
            ids.insert(id.to_string(), path.clone());
            package.media_types.insert(path, media_type);
        }
    }
    let is_image = |p: &String| package.media_types.get(p).is_some_and(|m| m.starts_with("image/"));
    package.cover_path = cover_by_property
        .or_else(|| cover_id.and_then(|id| ids.get(&id).cloned()).filter(is_image))
        .or(cover_by_name);

    if let Some(spine) = child(root, "spine") {
        if let Some(path) = attr(spine, "toc").and_then(|id| ids.get(id)) {
            package.ncx_path = Some(path.clone());
        }
        for itemref in spine.children().filter(|n| n.is_element() && n.tag_name().name() == "itemref") {
            let Some(path) = attr(itemref, "idref").and_then(|id| ids.get(id)) else { continue };
            let media_type = package.media_types.get(path).map(String::as_str).unwrap_or("");
            if is_html(media_type, path) && !package.spine.contains(path) {
                package.spine.push(path.clone());
            }
        }
    }
    Ok(package)
}

/// HTML named entities are not defined in XML; map the ones navigation
/// documents actually use so a strict XML parser accepts them.
fn fix_entities(text: &str) -> String {
    const ENTITIES: &[(&str, &str)] = &[
        ("&nbsp;", "&#160;"), ("&mdash;", "&#8212;"), ("&ndash;", "&#8211;"), ("&hellip;", "&#8230;"),
        ("&rsquo;", "&#8217;"), ("&lsquo;", "&#8216;"), ("&rdquo;", "&#8221;"), ("&ldquo;", "&#8220;"),
        ("&laquo;", "&#171;"), ("&raquo;", "&#187;"), ("&copy;", "&#169;"), ("&shy;", "&#173;"),
    ];
    if !text.contains('&') {
        return text.to_string();
    }
    ENTITIES.iter().fold(text.to_string(), |acc, (name, value)| acc.replace(name, value))
}

fn push_entry(out: &mut Vec<TocEntry>, title: String, href: &str, base: &str, depth: u32, spine: &HashMap<String, u32>) {
    if title.is_empty() || out.len() >= MAX_TOC_ENTRIES {
        return;
    }
    let (path, fragment) = resolve(base, href);
    let spine_index = spine.get(&path).copied();
    out.push(TocEntry { title, path, fragment, depth, spine_index });
}

fn parse_nav(text: &str, base: &str, spine: &HashMap<String, u32>) -> Result<Vec<TocEntry>, CoreError> {
    let text = fix_entities(text);
    let doc = parse_xml(&text)?;
    let navs: Vec<Node> = doc.descendants().filter(|n| n.is_element() && n.tag_name().name() == "nav").collect();
    let nav = navs
        .iter()
        .find(|n| attr(**n, "type").is_some_and(|t| t.split_whitespace().any(|t| t == "toc")))
        .or(navs.first())
        .ok_or_else(|| CoreError::invalid("no nav element"))?;

    fn walk(list: Node, base: &str, depth: u32, spine: &HashMap<String, u32>, out: &mut Vec<TocEntry>) {
        if depth >= MAX_TOC_DEPTH {
            return;
        }
        for li in list.children().filter(|n| n.is_element() && n.tag_name().name() == "li") {
            if let Some(a) = child(li, "a") {
                push_entry(out, text_of(a), attr(a, "href").unwrap_or(""), base, depth, spine);
            }
            if let Some(sub) = child(li, "ol") {
                walk(sub, base, depth + 1, spine, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(list) = child(*nav, "ol") {
        walk(list, base, 0, spine, &mut out);
    }
    Ok(out)
}

fn parse_ncx(text: &str, base: &str, spine: &HashMap<String, u32>) -> Result<Vec<TocEntry>, CoreError> {
    let text = fix_entities(text);
    let doc = parse_xml(&text)?;
    fn walk(parent: Node, base: &str, depth: u32, spine: &HashMap<String, u32>, out: &mut Vec<TocEntry>) {
        if depth >= MAX_TOC_DEPTH {
            return;
        }
        for point in parent.children().filter(|n| n.is_element() && n.tag_name().name() == "navPoint") {
            let title = child(point, "navLabel").map(text_of).unwrap_or_default();
            let src = child(point, "content").and_then(|c| attr(c, "src")).unwrap_or("");
            push_entry(out, title, src, base, depth, spine);
            walk(point, base, depth + 1, spine, out);
        }
    }
    let mut out = Vec::new();
    if let Some(map) = doc.descendants().find(|n| n.is_element() && n.tag_name().name() == "navMap") {
        walk(map, base, 0, spine, &mut out);
    }
    Ok(out)
}

/// True when the archive is encrypted with anything other than the two
/// standard font-obfuscation schemes.
fn is_protected<R: Read + Seek>(zip: &mut ZipArchive<R>) -> bool {
    const FONT_OBFUSCATION: &[&str] = &["http://www.idpf.org/2008/embedding", "http://ns.adobe.com/pdf/enc#RC"];
    let Ok(data) = read_entry(zip, "META-INF/encryption.xml", MAX_XML) else { return false };
    let text = decode_text(&data);
    let Ok(doc) = parse_xml(&text) else { return true };
    let protected = doc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "EncryptionMethod")
        .any(|n| !attr(n, "Algorithm").is_some_and(|a| FONT_OBFUSCATION.contains(&a)));
    protected
}

type Archive = ZipArchive<BufReader<File>>;

fn open_archive(path: &str) -> Result<Archive, CoreError> {
    Ok(ZipArchive::new(BufReader::new(File::open(path)?))?)
}

/// Metadata and cover presence, for the library scan.
pub(crate) fn read_meta(path: &str) -> Result<(Meta, bool), CoreError> {
    let mut zip = open_archive(path)?;
    let package = read_package(&mut zip)?;
    let has_cover = package.cover_path.as_deref().is_some_and(|p| zip.index_for_name(p).is_some());
    Ok((package.meta, has_cover))
}

pub(crate) fn read_cover(path: &str) -> Result<Vec<u8>, CoreError> {
    let mut zip = open_archive(path)?;
    let package = read_package(&mut zip)?;
    let cover = package.cover_path.ok_or_else(|| CoreError::not_found("cover"))?;
    read_entry(&mut zip, &cover, MAX_RESOURCE)
}

/// An open EPUB. Cheap to keep around: content is read from the archive on
/// demand.
#[derive(uniffi::Object)]
pub struct EpubBook {
    zip: Mutex<Archive>,
    package: Package,
    toc: Vec<TocEntry>,
    spine_index: HashMap<String, u32>,
}

#[uniffi::export]
impl EpubBook {
    #[uniffi::constructor]
    pub fn open(path: String) -> Result<Arc<Self>, CoreError> {
        let mut zip = open_archive(&path)?;
        if is_protected(&mut zip) {
            return Err(CoreError::Protected);
        }
        let mut package = read_package(&mut zip)?;
        package.spine.retain(|p| zip.index_for_name(p).is_some());
        if package.spine.is_empty() {
            return Err(CoreError::invalid("the book has no readable chapters"));
        }
        let spine_index: HashMap<String, u32> =
            package.spine.iter().enumerate().map(|(i, p)| (p.clone(), i as u32)).collect();

        let mut toc = Vec::new();
        if let Some(nav) = &package.nav_path {
            if let Ok(data) = read_entry(&mut zip, nav, MAX_XML) {
                toc = parse_nav(&decode_text(&data), dir_of(nav), &spine_index).unwrap_or_default();
            }
        }
        if toc.is_empty() {
            if let Some(ncx) = &package.ncx_path {
                if let Ok(data) = read_entry(&mut zip, ncx, MAX_XML) {
                    toc = parse_ncx(&decode_text(&data), dir_of(ncx), &spine_index).unwrap_or_default();
                }
            }
        }
        Ok(Arc::new(EpubBook { zip: Mutex::new(zip), package, toc, spine_index }))
    }

    pub fn title(&self) -> Option<String> {
        self.package.meta.title.clone()
    }

    pub fn language(&self) -> Option<String> {
        self.package.meta.language.clone()
    }

    /// Number of documents in reading order.
    pub fn spine_count(&self) -> u32 {
        self.package.spine.len() as u32
    }

    pub fn spine_path(&self, index: u32) -> Option<String> {
        self.package.spine.get(index as usize).cloned()
    }

    /// Position in reading order of the document at `path`, if it is part of
    /// the spine. Used to follow internal links.
    pub fn spine_index_of(&self, path: String) -> Option<u32> {
        self.spine_index.get(resolve("", &path).0.as_str()).copied()
    }

    pub fn toc(&self) -> Vec<TocEntry> {
        self.toc.clone()
    }

    /// Fetch a resource by its (URL-decoded) path inside the book. XHTML
    /// documents are returned sanitised and wrapped for paginated display;
    /// everything else is returned as stored.
    pub fn resource(&self, path: String) -> Result<Resource, CoreError> {
        let trimmed = path.trim_start_matches('/');
        if trimmed == READER_CSS_PATH {
            return Ok(Resource { data: include_bytes!("reader.css").to_vec(), mime: "text/css".into() });
        }
        // `resolve` also normalises away any "..".
        let path = resolve("", &trimmed.replace('%', "%25")).0;
        let media_type = match self.package.media_types.get(&path) {
            Some(m) if !m.is_empty() => m.clone(),
            _ => mime_for_extension(&path).to_string(),
        };
        let mut zip = self.zip.lock().unwrap_or_else(|e| e.into_inner());
        if is_html(&media_type, &path) {
            let raw = decode_text(&read_entry(&mut zip, &path, MAX_CHAPTER)?);
            let html = sanitize::chapter_document(&raw, self.package.meta.language.as_deref());
            Ok(Resource { data: html.into_bytes(), mime: "text/html".into() })
        } else {
            Ok(Resource { data: read_entry(&mut zip, &path, MAX_RESOURCE)?, mime: media_type })
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    pub(crate) const OPF: &str = r##"<?xml version="1.0"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
 <metadata xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:opf="http://www.idpf.org/2007/opf">
  <dc:title>Il barone rampante</dc:title>
  <dc:creator id="c1">Italo Calvino</dc:creator>
  <meta refines="#c1" property="file-as">Calvino, Italo</meta>
  <dc:creator opf:role="trl">Some Translator</dc:creator>
  <dc:publisher>Einaudi</dc:publisher>
  <dc:language>it</dc:language>
  <dc:date>1957-06-01</dc:date>
  <meta name="cover" content="cover-img"/>
 </metadata>
 <manifest>
  <item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/>
  <item id="c1" href="text/ch%201.xhtml" media-type="application/xhtml+xml"/>
  <item id="c2" href="text/ch2.xhtml" media-type="application/xhtml+xml"/>
  <item id="css" href="style.css" media-type="text/css"/>
  <item id="cover-img" href="images/cover.png" media-type="image/png"/>
 </manifest>
 <spine><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"##;

    pub(crate) fn sample_epub(dir: &std::path::Path, name: &str) -> String {
        let path = dir.join(name);
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        let opts = SimpleFileOptions::default();
        let mut add = |name: &str, body: &str| {
            zip.start_file(name, opts).unwrap();
            zip.write_all(body.as_bytes()).unwrap();
        };
        add("mimetype", "application/epub+zip");
        add("META-INF/container.xml", r#"<?xml version="1.0"?><container xmlns="urn:oasis:names:tc:opendocument:xmlns:container" version="1.0"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#);
        add("OEBPS/content.opf", OPF);
        add("OEBPS/nav.xhtml", r#"<html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="text/ch%201.xhtml">Uno&nbsp;</a><ol><li><a href="text/ch2.xhtml#s2">Due</a></li></ol></li></ol></nav></body></html>"#);
        add("OEBPS/text/ch 1.xhtml", r#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xml:lang="it"><head><title>Uno</title><link rel="stylesheet" href="../style.css"/><script src="evil.js"/></head><body class="chapter"><h1 id="top">Uno</h1><p onclick="steal()">Ciao <a id="n1"/>mondo <img src="../images/cover.png" onerror="x()"/></p><script>alert(1)</script></body></html>"#);
        add("OEBPS/text/ch2.xhtml", "<html><body><p id='s2'>Due</p></body></html>");
        add("OEBPS/style.css", "p { color: red }");
        add("OEBPS/images/cover.png", "not really a png");
        zip.finish().unwrap();
        path.to_string_lossy().into_owned()
    }

    pub(crate) fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("rattonero-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn paths() {
        assert_eq!(resolve("OEBPS/text", "../images/a%20b.png#x").0, "OEBPS/images/a b.png");
        assert_eq!(resolve("OEBPS", "../../../etc/passwd").0, "etc/passwd");
        assert_eq!(resolve("", "a/./b//c.xhtml#frag"), ("a/b/c.xhtml".into(), Some("frag".into())));
    }

    #[test]
    fn metadata() {
        let p = parse_package(OPF, "OEBPS").unwrap();
        assert_eq!(p.meta.title.as_deref(), Some("Il barone rampante"));
        assert_eq!(p.meta.authors, ["Italo Calvino"]);
        assert_eq!(p.meta.author_sort.as_deref(), Some("Calvino, Italo"));
        assert_eq!(p.meta.publisher.as_deref(), Some("Einaudi"));
        assert_eq!(p.meta.year, Some(1957));
        assert_eq!(p.cover_path.as_deref(), Some("OEBPS/images/cover.png"));
        assert_eq!(p.spine, ["OEBPS/text/ch 1.xhtml", "OEBPS/text/ch2.xhtml"]);
    }

    #[test]
    fn open_and_read() {
        let dir = temp_dir("epub");
        let book = EpubBook::open(sample_epub(&dir, "a.epub")).unwrap();
        assert_eq!(book.spine_count(), 2);
        let toc = book.toc();
        assert_eq!(toc.len(), 2);
        assert_eq!((toc[0].title.as_str(), toc[0].spine_index, toc[0].depth), ("Uno", Some(0), 0));
        assert_eq!((toc[1].fragment.as_deref(), toc[1].spine_index, toc[1].depth), (Some("s2"), Some(1), 1));
        assert_eq!(book.spine_index_of("/OEBPS/text/ch2.xhtml".into()), Some(1));

        let chapter = book.resource("/OEBPS/text/ch 1.xhtml".into()).unwrap();
        assert_eq!(chapter.mime, "text/html");
        let html = String::from_utf8(chapter.data).unwrap();
        assert!(!html.contains("script") || !html.contains("alert"), "{html}");
        assert!(!html.contains("onclick") && !html.contains("onerror") && !html.contains("evil.js"), "{html}");
        assert!(html.contains(r#"href="../style.css""#), "{html}");
        assert!(html.contains(r#"<body class="chapter">"#), "{html}");
        // The self-closed anchor must not swallow the rest of the paragraph.
        assert!(html.contains(r#"<a id="n1"></a>mondo"#), "{html}");
        assert!(html.contains(r#"src="../images/cover.png""#), "{html}");

        assert_eq!(book.resource("OEBPS/style.css".into()).unwrap().mime, "text/css");
        assert_eq!(book.resource("/.rattonero/reader.css".into()).unwrap().mime, "text/css");
        assert!(book.resource("/OEBPS/../../etc/passwd".into()).is_err());
        std::fs::remove_dir_all(dir).ok();
    }
}
