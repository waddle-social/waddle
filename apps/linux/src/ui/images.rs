//! Shared images: decoded off the main thread within a pixel budget,
//! scaled to the size they are shown at, and kept in a bounded cache.
//!
//! A sender controls an image's bytes, so its declared size means nothing:
//! a 1.5 MB PNG can claim 20000×20000 pixels and 1.6 GB once decoded. The
//! header is read first and anything over [`MAX_PIXELS`] is refused before
//! its pixels are decoded.

use std::cell::Cell;
use std::collections::{HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::gdk_pixbuf;
use gtk::gdk_pixbuf::prelude::*;
use gtk::glib;
use thiserror::Error;

/// Largest image decoded, in pixels (25 megapixels).
pub const MAX_PIXELS: u64 = 25_000_000;
/// Decoded at most this size: twice the 400×300 display box, for HiDPI.
pub const DECODE_BOX: (i32, i32) = (800, 600);
/// Header bytes fed before the size is known. Small, so a hostile file
/// gets no further than its header before it is refused.
const HEADER_CHUNK: usize = 512;
const BODY_CHUNK: usize = 64 * 1024;
const CAPACITY: usize = 64;
/// The formats decoded. Others (SVG, TIFF, ICO…) never run on a sender's
/// bytes; such files are shown as files.
const FORMATS: [&str; 4] = ["png", "jpeg", "gif", "webp"];
/// Images shown inline per message; the rest are shown as files.
pub const MAX_INLINE_PER_MESSAGE: usize = 8;
/// Decodes at once: each may briefly hold a full-size image.
static DECODES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(2);
const RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("the image is too large to show")]
    TooLarge,
    #[error("the image could not be read")]
    Invalid,
    #[error("the image format is not shown inline")]
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    TooLarge,
    Unsupported,
}

/// Decoded pixels, ready to become a texture on the main thread.
pub struct Decoded {
    width: i32,
    height: i32,
    stride: usize,
    has_alpha: bool,
    pixels: glib::Bytes,
}

impl Decoded {
    pub fn texture(&self) -> gdk::Texture {
        let format = if self.has_alpha {
            gdk::MemoryFormat::R8g8b8a8
        } else {
            gdk::MemoryFormat::R8g8b8
        };
        gdk::MemoryTexture::new(self.width, self.height, format, &self.pixels, self.stride).upcast()
    }
}

/// Decode `bytes`, refusing images over [`MAX_PIXELS`] and scaling the
/// rest to fit [`DECODE_BOX`]. Blocking: run it off the main thread.
pub fn decode(bytes: &[u8]) -> Result<Decoded, DecodeError> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let refused = std::rc::Rc::new(Cell::new(None::<Refusal>));
    let known = std::rc::Rc::new(Cell::new(false));
    let (flag, seen) = (refused.clone(), known.clone());
    loader.connect_size_prepared(move |loader, width, height| {
        seen.set(true);
        let format = loader
            .format()
            .and_then(|format| format.name())
            .map(|name| name.to_string());
        let pixels = u64::try_from(width)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(height).unwrap_or(u64::MAX));
        let refusal = if !format.is_some_and(|name| FORMATS.contains(&name.as_str())) {
            Some(Refusal::Unsupported)
        } else if pixels > MAX_PIXELS || width <= 0 || height <= 0 {
            Some(Refusal::TooLarge)
        } else {
            None
        };
        if let Some(refusal) = refusal {
            flag.set(Some(refusal));
            // The loader allocates the full image as soon as this returns;
            // a zero size is the abort gdk_pixbuf_get_file_info uses, and
            // stops the module before it allocates anything.
            loader.set_size(0, 0);
            return;
        }
        let (fit_width, fit_height) = fit(width, height, DECODE_BOX);
        if (fit_width, fit_height) != (width, height) {
            loader.set_size(fit_width, fit_height);
        }
    });

    let refusal = |refused: &Cell<Option<Refusal>>| {
        refused.get().map(|refusal| match refusal {
            Refusal::TooLarge => DecodeError::TooLarge,
            Refusal::Unsupported => DecodeError::Unsupported,
        })
    };
    let mut offset = 0;
    while offset < bytes.len() {
        let chunk = if known.get() {
            BODY_CHUNK
        } else {
            HEADER_CHUNK
        };
        let end = usize::min(offset + chunk, bytes.len());
        let written = loader.write(&bytes[offset..end]);
        if let Some(error) = refusal(&refused) {
            let _ = loader.close();
            return Err(error);
        }
        written.map_err(|_| DecodeError::Invalid)?;
        offset = end;
    }
    let closed = loader.close();
    if let Some(error) = refusal(&refused) {
        return Err(error);
    }
    closed.map_err(|_| DecodeError::Invalid)?;
    let pixbuf = loader.pixbuf().ok_or(DecodeError::Invalid)?;
    if pixbuf.bits_per_sample() != 8 {
        return Err(DecodeError::Invalid);
    }
    Ok(Decoded {
        width: pixbuf.width(),
        height: pixbuf.height(),
        stride: usize::try_from(pixbuf.rowstride()).map_err(|_| DecodeError::Invalid)?,
        has_alpha: pixbuf.has_alpha(),
        pixels: pixbuf.read_pixel_bytes(),
    })
}

/// [`decode`] on a worker thread, at most two at a time.
pub async fn decode_off_main(bytes: Vec<u8>) -> Result<Decoded, DecodeError> {
    let _permit = DECODES.acquire().await.map_err(|_| DecodeError::Invalid)?;
    gtk::gio::spawn_blocking(move || decode(&bytes))
        .await
        .unwrap_or(Err(DecodeError::Invalid))
}

/// Scale `width`×`height` down to fit `max`, keeping the aspect ratio.
pub fn fit(width: i32, height: i32, max: (i32, i32)) -> (i32, i32) {
    if width <= 0 || height <= 0 {
        return max;
    }
    let scale = f64::min(
        1.0,
        f64::min(
            f64::from(max.0) / f64::from(width),
            f64::from(max.1) / f64::from(height),
        ),
    );
    (
        ((f64::from(width) * scale).round() as i32).max(1),
        ((f64::from(height) * scale).round() as i32).max(1),
    )
}

/// Where one image stands.
#[derive(Clone)]
pub enum ImageState {
    Loading,
    Ready(gdk::Texture),
    /// It failed; `retry_at` is when a transient failure may be tried
    /// again, `None` when the file itself is bad.
    Failed {
        retry_at: Option<Instant>,
    },
}

/// A least-recently-used map with a fixed capacity.
pub struct Lru<V> {
    entries: HashMap<String, V>,
    order: VecDeque<String>,
    capacity: usize,
}

impl<V> Lru<V> {
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    pub fn get(&mut self, key: &str) -> Option<&V> {
        if self.entries.contains_key(key) {
            self.touch(key);
        }
        self.entries.get(key)
    }

    pub fn insert(&mut self, key: String, value: V) {
        if self.entries.insert(key.clone(), value).is_some() {
            self.touch(&key);
            return;
        }
        self.order.push_back(key);
        while self.order.len() > self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn touch(&mut self, key: &str) {
        if let Some(at) = self.order.iter().position(|held| held == key) {
            if let Some(key) = self.order.remove(at) {
                self.order.push_back(key);
            }
        }
    }
}

/// The window's shared images by content key.
///
/// Loads in flight are never evicted, and an image still on screen is
/// found through its widget's reference even after the LRU drops it, so
/// binding rows can never refetch what is loading or shown. Without that,
/// more images on screen than the LRU holds would fetch in an endless
/// cycle.
pub struct ImageCache {
    settled: Lru<ImageState>,
    in_flight: HashSet<String>,
    shown: HashMap<String, glib::WeakRef<gdk::Texture>>,
}

impl Default for ImageCache {
    fn default() -> Self {
        Self {
            settled: Lru::new(CAPACITY),
            in_flight: HashSet::new(),
            shown: HashMap::new(),
        }
    }
}

impl ImageCache {
    /// The image's state, or `None` when it should be (re)loaded now.
    pub fn lookup(&mut self, key: &str, now: Instant) -> Option<ImageState> {
        if self.in_flight.contains(key) {
            return Some(ImageState::Loading);
        }
        match self.settled.get(key) {
            Some(ImageState::Failed {
                retry_at: Some(retry_at),
            }) if now >= *retry_at => return None,
            Some(state) => return Some(state.clone()),
            None => {}
        }
        let texture = self.shown.get(key)?.upgrade()?;
        self.settled
            .insert(key.to_string(), ImageState::Ready(texture.clone()));
        Some(ImageState::Ready(texture))
    }

    pub fn loading(&mut self, key: String) {
        self.in_flight.insert(key);
    }

    pub fn ready(&mut self, key: String, texture: gdk::Texture) {
        self.in_flight.remove(&key);
        if self.shown.len() > CAPACITY * 4 {
            self.shown.retain(|_, texture| texture.upgrade().is_some());
        }
        self.shown.insert(key.clone(), texture.downgrade());
        self.settled.insert(key, ImageState::Ready(texture));
    }

    pub fn failed(&mut self, key: String, transient: bool, now: Instant) {
        self.in_flight.remove(&key);
        let retry_at = transient.then(|| now + RETRY_AFTER);
        self.settled.insert(key, ImageState::Failed { retry_at });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_forgets_the_least_recently_used() {
        let mut lru = Lru::new(2);
        lru.insert("a".into(), 1);
        lru.insert("b".into(), 2);
        assert_eq!(lru.get("a"), Some(&1));
        lru.insert("c".into(), 3);
        assert_eq!(lru.get("b"), None, "b was the least recently used");
        assert_eq!(lru.get("a"), Some(&1));
        assert_eq!(lru.get("c"), Some(&3));
        assert_eq!(lru.entries.len(), 2);
    }

    #[test]
    fn transient_failures_are_retried_and_bad_files_are_not() {
        let mut cache = ImageCache::default();
        let now = Instant::now();
        cache.failed("flaky".into(), true, now);
        cache.failed("bad".into(), false, now);
        assert!(cache.lookup("flaky", now).is_some());
        assert!(cache.lookup("flaky", now + RETRY_AFTER).is_none());
        assert!(cache.lookup("bad", now + RETRY_AFTER * 10).is_some());
        assert!(cache.lookup("unknown", now).is_none());
    }

    #[test]
    fn images_fit_their_box() {
        assert_eq!(fit(1600, 1200, (400, 300)), (400, 300));
        assert_eq!(fit(100, 50, (400, 300)), (100, 50));
        assert_eq!(fit(10000, 10, (400, 300)), (400, 1));
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let pixbuf = gdk_pixbuf::Pixbuf::new(
            gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            i32::try_from(width).expect("fits"),
            i32::try_from(height).expect("fits"),
        )
        .expect("pixbuf");
        pixbuf.fill(0x3366_99ff);
        pixbuf.save_to_bufferv("png", &[]).expect("png")
    }

    #[test]
    fn images_decode_scaled_to_the_decode_box() {
        let decoded = decode(&png(1600, 1200)).expect("decoded");
        assert_eq!((decoded.width, decoded.height), DECODE_BOX);
        let small = decode(&png(40, 20)).expect("decoded");
        assert_eq!((small.width, small.height), (40, 20));
        assert!(small.has_alpha);
    }

    /// A tiny file claiming a huge canvas is refused from its header,
    /// before its pixels are decoded.
    #[test]
    fn decompression_bombs_are_refused_from_the_header() {
        let mut bomb = png(1, 1);
        // Rewrite IHDR's width and height (bytes 16..24) to 20000×20000.
        bomb[16..20].copy_from_slice(&20_000u32.to_be_bytes());
        bomb[20..24].copy_from_slice(&20_000u32.to_be_bytes());
        // Keep the chunk valid: its CRC covers the type and data (12..29).
        let crc = crc32(&bomb[12..29]);
        bomb[29..33].copy_from_slice(&crc.to_be_bytes());
        assert_eq!(decode(&bomb).err(), Some(DecodeError::TooLarge));
    }

    /// PNG's CRC-32 (ISO-HDLC).
    fn crc32(bytes: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for byte in bytes {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                crc = if crc & 1 == 1 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    fn texture() -> gdk::Texture {
        gdk::MemoryTexture::new(
            1,
            1,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_static(&[0, 0, 0, 0]),
            4,
        )
        .upcast()
    }

    /// More images than the LRU holds, all loading or on screen: none is
    /// ever asked for again, so rows cannot refetch in a cycle.
    #[test]
    fn loading_and_shown_images_are_never_refetched() {
        let mut cache = ImageCache::default();
        let now = Instant::now();
        let keys: Vec<String> = (0..CAPACITY * 2).map(|at| format!("k{at}")).collect();
        for key in &keys {
            cache.loading(key.clone());
        }
        assert!(keys
            .iter()
            .all(|key| matches!(cache.lookup(key, now), Some(ImageState::Loading))));

        // Widgets on screen hold the textures.
        let on_screen: Vec<gdk::Texture> = keys.iter().map(|_| texture()).collect();
        for (key, texture) in keys.iter().zip(&on_screen) {
            cache.ready(key.clone(), texture.clone());
        }
        assert!(keys
            .iter()
            .all(|key| matches!(cache.lookup(key, now), Some(ImageState::Ready(_)))));

        // Off screen and out of the LRU: loaded again when next shown.
        drop(on_screen);
        let missing = keys
            .iter()
            .filter(|key| cache.lookup(key, now).is_none())
            .count();
        assert!(missing >= CAPACITY, "{missing}");
    }

    fn peak_memory_kib() -> u64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| {
                status
                    .lines()
                    .find_map(|line| line.strip_prefix("VmHWM:"))
                    .and_then(|value| value.trim().trim_end_matches("kB").trim().parse().ok())
            })
            .unwrap_or(0)
    }

    /// A 43-byte GIF declaring a 12000×12000 canvas: refused without
    /// allocating it (it used to cost 554 MB).
    #[test]
    fn a_tiny_bomb_is_refused_without_allocating_its_canvas() {
        let mut gif: Vec<u8> = b"GIF89a".to_vec();
        gif.extend_from_slice(&12_000u16.to_le_bytes());
        gif.extend_from_slice(&12_000u16.to_le_bytes());
        gif.extend_from_slice(&[0x80, 0, 0, 0xff, 0xff, 0xff, 0, 0, 0]);
        gif.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0]);
        gif.extend_from_slice(&[0x02, 0x02, 0x44, 0x01, 0x00, 0x3b]);
        let before = peak_memory_kib();
        assert_eq!(decode(&gif).err(), Some(DecodeError::TooLarge));
        let grown = peak_memory_kib().saturating_sub(before);
        assert!(grown < 128 * 1024, "peak memory grew by {grown} KiB");
    }

    #[test]
    fn garbage_is_not_an_image() {
        assert_eq!(
            decode(b"not an image at all").err(),
            Some(DecodeError::Invalid)
        );
    }
}
