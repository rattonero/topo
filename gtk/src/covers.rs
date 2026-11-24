//! Cover thumbnails, loaded off the main thread and cached. EPUB covers come
//! from the core; PDF covers are the first page, drawn by Poppler.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk::gdk_pixbuf::{InterpType, PixbufLoader};
use gtk::prelude::*;
use gtk::{cairo, gdk, gio, glib};
use rattonero_core::{BookFormat, BookInfo, Library};

const MAX_PIXELS: f64 = 520.0;
const MAX_CACHED: usize = 600;

type Waiter = Box<dyn FnOnce(&gdk::Texture)>;

#[derive(Default)]
pub struct Covers {
    cache: RefCell<HashMap<String, Option<gdk::Texture>>>,
    pending: RefCell<HashMap<String, Vec<Waiter>>>,
}

/// Decoded pixels; only plain data crosses back from the worker thread.
struct Pixels {
    width: i32,
    height: i32,
    stride: usize,
    format: gdk::MemoryFormat,
    bytes: glib::Bytes,
}

impl Covers {
    pub fn new() -> Rc<Covers> {
        Rc::new(Covers::default())
    }

    /// Call `done` with the cover of `book` once it is at hand; never for a
    /// book that has none.
    pub fn get(
        self: &Rc<Self>,
        book: &BookInfo,
        library: &Arc<Library>,
        done: impl FnOnce(&gdk::Texture) + 'static,
    ) {
        let key = book.path.clone();
        if let Some(entry) = self.cache.borrow().get(&key) {
            if let Some(texture) = entry {
                done(texture);
            }
            return;
        }
        if book.format == BookFormat::Epub && !book.has_cover {
            return;
        }
        {
            let mut pending = self.pending.borrow_mut();
            if let Some(waiters) = pending.get_mut(&key) {
                waiters.push(Box::new(done));
                return;
            }
            pending.insert(key.clone(), vec![Box::new(done)]);
        }
        let this = self.clone();
        let book = book.clone();
        let library = library.clone();
        glib::spawn_future_local(async move {
            let pixels = gio::spawn_blocking(move || load(&book, &library))
                .await
                .ok()
                .flatten();
            let texture = pixels.map(|p| {
                gdk::MemoryTexture::new(p.width, p.height, p.format, &p.bytes, p.stride)
                    .upcast::<gdk::Texture>()
            });
            {
                let mut cache = this.cache.borrow_mut();
                if cache.len() > MAX_CACHED {
                    cache.clear();
                }
                cache.insert(key.clone(), texture.clone());
            }
            let waiters = this.pending.borrow_mut().remove(&key).unwrap_or_default();
            if let Some(texture) = texture {
                for waiter in waiters {
                    waiter(&texture);
                }
            }
        });
    }
}

fn load(book: &BookInfo, library: &Library) -> Option<Pixels> {
    match book.format {
        BookFormat::Epub => thumbnail(&library.cover(book.id.clone())?),
        BookFormat::Pdf => first_page(&book.path),
    }
}

fn fit(width: f64, height: f64) -> (i32, i32) {
    let scale = (MAX_PIXELS / width.max(height)).min(1.0);
    (
        ((width * scale).round() as i32).max(1),
        ((height * scale).round() as i32).max(1),
    )
}

fn thumbnail(data: &[u8]) -> Option<Pixels> {
    let loader = PixbufLoader::new();
    // Decode straight to thumbnail size; a huge cover never lands in memory
    // at full resolution.
    loader.connect_size_prepared(|loader, width, height| {
        if width > 0 && height > 0 {
            let (w, h) = fit(width as f64, height as f64);
            loader.set_size(w, h);
        }
    });
    loader.write(data).ok()?;
    loader.close().ok()?;
    let mut pixbuf = loader.pixbuf()?;
    let (w, h) = fit(pixbuf.width() as f64, pixbuf.height() as f64);
    if (w, h) != (pixbuf.width(), pixbuf.height()) {
        pixbuf = pixbuf.scale_simple(w, h, InterpType::Bilinear)?;
    }
    let pixbuf = pixbuf.apply_embedded_orientation().unwrap_or(pixbuf);
    let format = if pixbuf.has_alpha() {
        gdk::MemoryFormat::R8g8b8a8
    } else {
        gdk::MemoryFormat::R8g8b8
    };
    Some(Pixels {
        width: pixbuf.width(),
        height: pixbuf.height(),
        stride: pixbuf.rowstride() as usize,
        format,
        bytes: pixbuf.read_pixel_bytes(),
    })
}

fn first_page(path: &str) -> Option<Pixels> {
    let uri = glib::filename_to_uri(path, None).ok()?;
    let document = poppler::Document::from_file(&uri, None).ok()?;
    let page = document.page(0)?;
    let (width, height) = page.size();
    if width < 1.0 || height < 1.0 {
        return None;
    }
    let (w, h) = fit(width, height);
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, w, h).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        cr.set_source_rgb(1.0, 1.0, 1.0);
        cr.paint().ok()?;
        cr.scale(w as f64 / width, h as f64 / height);
        page.render(&cr);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let data = surface.data().ok()?.to_vec();
    Some(Pixels {
        width: w,
        height: h,
        stride,
        // Cairo's ARGB32 is native-endian, premultiplied.
        format: if cfg!(target_endian = "little") {
            gdk::MemoryFormat::B8g8r8a8Premultiplied
        } else {
            gdk::MemoryFormat::A8r8g8b8Premultiplied
        },
        bytes: glib::Bytes::from_owned(data),
    })
}
