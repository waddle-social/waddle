//! Shared images: decoded off the main thread within a pixel budget,
//! scaled to the size they are shown at, and kept in a bounded cache.
//!
//! A sender controls an image's bytes, so its declared size means nothing:
//! a 1.5 MB PNG can claim 20000×20000 pixels and 1.6 GB once decoded. The
//! header is read first and anything over [`MAX_PIXELS`] is refused before
//! its pixels are decoded.

use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
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
const RETRY_AFTER: Duration = Duration::from_secs(60);

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("the image is too large to show")]
    TooLarge,
    #[error("the image could not be read")]
    Invalid,
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
    let refused = std::rc::Rc::new(Cell::new(false));
    let known = std::rc::Rc::new(Cell::new(false));
    let (flag, seen) = (refused.clone(), known.clone());
    loader.connect_size_prepared(move |loader, width, height| {
        seen.set(true);
        let pixels = u64::try_from(width)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(height).unwrap_or(u64::MAX));
        if pixels > MAX_PIXELS || width <= 0 || height <= 0 {
            flag.set(true);
            return;
        }
        let (fit_width, fit_height) = fit(width, height, DECODE_BOX);
        if (fit_width, fit_height) != (width, height) {
            loader.set_size(fit_width, fit_height);
        }
    });

    let mut offset = 0;
    while offset < bytes.len() {
        let chunk = if known.get() {
            BODY_CHUNK
        } else {
            HEADER_CHUNK
        };
        let end = usize::min(offset + chunk, bytes.len());
        let written = loader.write(&bytes[offset..end]);
        if refused.get() {
            let _ = loader.close();
            return Err(DecodeError::TooLarge);
        }
        written.map_err(|_| DecodeError::Invalid)?;
        offset = end;
    }
    loader.close().map_err(|_| DecodeError::Invalid)?;
    if refused.get() {
        return Err(DecodeError::TooLarge);
    }
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
pub struct ImageCache(Lru<ImageState>);

impl Default for ImageCache {
    fn default() -> Self {
        Self(Lru::new(CAPACITY))
    }
}

impl ImageCache {
    /// The image's state, or `None` when it should be (re)loaded now.
    pub fn lookup(&mut self, key: &str, now: Instant) -> Option<ImageState> {
        match self.0.get(key)? {
            ImageState::Failed {
                retry_at: Some(retry_at),
            } if now >= *retry_at => None,
            state => Some(state.clone()),
        }
    }

    pub fn loading(&mut self, key: String) {
        self.0.insert(key, ImageState::Loading);
    }

    pub fn ready(&mut self, key: String, texture: gdk::Texture) {
        self.0.insert(key, ImageState::Ready(texture));
    }

    pub fn failed(&mut self, key: String, transient: bool, now: Instant) {
        let retry_at = transient.then(|| now + RETRY_AFTER);
        self.0.insert(key, ImageState::Failed { retry_at });
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

    #[test]
    fn garbage_is_not_an_image() {
        assert_eq!(
            decode(b"not an image at all").err(),
            Some(DecodeError::Invalid)
        );
    }
}
