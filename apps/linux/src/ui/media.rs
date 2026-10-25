//! Inline linked images. Each bind owns its widget; late loads cannot paint a recycled row.
use std::time::{Duration, SystemTime};

static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

use adw::prelude::{
    BoxExt, CastNone, ObjectExt, PixbufAnimationExt, PixbufAnimationExtManual, PixbufLoaderExt,
    WidgetExt,
};
use gtk::{gdk, gdk_pixbuf, glib};
use url::Url;

use crate::runtime;

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_IMAGES: usize = 4;
/// The largest an image shows, in logical pixels.
const MAX_WIDTH: f64 = 320.0;
const MAX_HEIGHT: f64 = 256.0;

/// A whole word that links an HTTPS image without credentials.
fn image_url(word: &str) -> Option<Url> {
    let url = Url::parse(word).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && [".gif", ".png", ".jpg", ".jpeg", ".webp"]
            .iter()
            .any(|ext| url.path().to_ascii_lowercase().ends_with(ext)))
    .then_some(url)
}

fn image_urls(body: &str) -> Vec<Url> {
    body.split_whitespace()
        .filter_map(image_url)
        .take(MAX_IMAGES)
        .collect()
}

/// A body that is nothing but the images it shows: the images stand in
/// for the text, as on the web and phones.
pub(super) fn is_media_only(body: &str) -> bool {
    let words: Vec<&str> = body.split_whitespace().collect();
    !words.is_empty()
        && words.len() <= MAX_IMAGES
        && words.iter().all(|word| image_url(word).is_some())
}

async fn download(url: Url) -> Result<Vec<u8>, reqwest::Error> {
    let _permit = DOWNLOADS.acquire().await.expect("download semaphore open");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    let mut response = client.get(url).send().await?.error_for_status()?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BYTES as u64)
    {
        return Ok(Vec::new());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if bytes.len() + chunk.len() > MAX_BYTES {
            return Ok(Vec::new());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn decode(bytes: &[u8]) -> Result<gdk_pixbuf::PixbufAnimation, glib::Error> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    loader.connect_size_prepared(|loader, width, height| {
        let scale = (MAX_WIDTH / f64::from(width))
            .min(MAX_HEIGHT / f64::from(height))
            .min(1.0);
        loader.set_size(
            (f64::from(width) * scale).max(1.0) as i32,
            (f64::from(height) * scale).max(1.0) as i32,
        );
    });
    let written = loader.write(bytes);
    let closed = loader.close();
    written?;
    closed?;
    loader
        .animation()
        .ok_or_else(|| glib::Error::new(gdk_pixbuf::PixbufError::Failed, "No image"))
}

/// Show the body's images: a placeholder while each loads, then the
/// image (animated for a GIF), or its link when it cannot be shown.
pub(super) fn bind(container: &gtk::Box, body: &str, retracted: bool) {
    let urls = if retracted {
        Vec::new()
    } else {
        image_urls(body)
    };
    let mut child = container.first_child();
    let unchanged = urls.iter().all(|url| {
        let matches = child
            .as_ref()
            .is_some_and(|child| child.widget_name() == url.as_str());
        child = child.as_ref().and_then(|child| child.next_sibling());
        matches
    }) && child.is_none();
    container.set_visible(!urls.is_empty());
    if unchanged {
        return;
    }
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    for url in urls {
        let slot = slot(&url);
        container.append(&slot);
        load(&slot, url);
    }
}

/// The placeholder, the picture and the link, one visible at a time.
fn slot(url: &Url) -> gtk::Box {
    let slot = gtk::Box::builder().halign(gtk::Align::Start).build();
    slot.set_widget_name(url.as_str());

    let placeholder = gtk::Box::builder()
        .width_request(240)
        .height_request(160)
        .build();
    placeholder.add_css_class("media-placeholder");
    let spinner = adw::Spinner::builder()
        .hexpand(true)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .build();
    placeholder.append(&spinner);
    slot.append(&placeholder);

    let picture = gtk::Picture::builder()
        .halign(gtk::Align::Start)
        .can_shrink(true)
        .content_fit(gtk::ContentFit::Cover)
        .alternative_text("Shared image")
        .overflow(gtk::Overflow::Hidden)
        .visible(false)
        .build();
    picture.add_css_class("media");
    slot.append(&picture);

    let link = gtk::Label::builder()
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .use_markup(true)
        .label(link_markup(url.as_str()))
        .visible(false)
        .build();
    slot.append(&link);
    slot
}

fn link_markup(url: &str) -> String {
    let escaped = glib::markup_escape_text(url);
    format!("<a href=\"{escaped}\">{escaped}</a>")
}

fn load(slot: &gtk::Box, url: Url) {
    let weak = slot.downgrade();
    glib::spawn_future_local(async move {
        let download = runtime::spawn(download(url));
        let cancel = download.abort_handle();
        if let Some(slot) = weak.upgrade() {
            slot.add_weak_ref_notify_local(move || cancel.abort());
        } else {
            download.abort();
            return;
        }
        let result = download.await;
        let Some(slot) = weak.upgrade() else {
            return;
        };
        let animation = match result {
            Ok(Ok(bytes)) if !bytes.is_empty() => decode(&bytes).ok(),
            _ => None,
        };
        let (Some(placeholder), Some(picture), Some(link)) = (
            slot.first_child(),
            slot.first_child()
                .and_then(|child| child.next_sibling())
                .and_downcast::<gtk::Picture>(),
            slot.last_child(),
        ) else {
            return;
        };
        placeholder.set_visible(false);
        match animation {
            Some(animation) => {
                play(&picture, &animation);
                picture.set_visible(true);
            }
            None => link.set_visible(true),
        }
    });
}

/// Paint the first frame, then advance frames while the picture lives.
fn play(picture: &gtk::Picture, animation: &gdk_pixbuf::PixbufAnimation) {
    // The list sizes rows by their minimum height, which is zero for a
    // picture that can shrink: hold the height and let only the width
    // give way (cropping the sides) in a narrow window.
    picture.set_size_request(-1, animation.height());
    let iter = animation.iter(Some(SystemTime::now()));
    picture.set_paintable(Some(&gdk::Texture::for_pixbuf(&iter.pixbuf())));
    if animation.is_static_image() || !picture.settings().is_gtk_enable_animations() {
        return;
    }
    let weak = picture.downgrade();
    let mut elapsed = Duration::ZERO;
    glib::timeout_add_local(Duration::from_millis(20), move || {
        let Some(picture) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        elapsed += Duration::from_millis(20);
        if elapsed >= iter.delay_time().unwrap_or(Duration::MAX) {
            elapsed = Duration::ZERO;
            if iter.advance(SystemTime::now()) {
                picture.set_paintable(Some(&gdk::Texture::for_pixbuf(&iter.pixbuf())));
            }
        }
        glib::ControlFlow::Continue
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_https_images_with_queries_and_caps_each_message() {
        let urls = image_urls("https://media.giphy.com/a.gif?x=1 https://example.com/X.PNG http://example.com/a.gif https://user:pass@example.com/a.gif https://example.com/page");
        assert_eq!(urls.len(), 2);
        assert_eq!(urls[0].query(), Some("x=1"));
        assert_eq!(
            image_urls(&"https://example.com/a.gif ".repeat(20)).len(),
            4
        );
    }

    #[test]
    fn hides_only_bodies_that_are_just_images() {
        assert!(is_media_only("https://media.giphy.com/a.gif"));
        assert!(is_media_only(
            "  https://a.example/x.png\nhttps://b.example/y.webp "
        ));
        assert!(!is_media_only("look https://media.giphy.com/a.gif"));
        assert!(!is_media_only("https://example.com/page"));
        assert!(!is_media_only("http://example.com/a.gif"));
        assert!(!is_media_only(""));
        assert!(!is_media_only(&"https://example.com/a.gif ".repeat(5)));
    }

    #[test]
    fn decodes_an_animated_gif() {
        let mut bytes = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff".to_vec();
        let frame = b"\x21\xf9\x04\x00\x05\x00\x00\x00\x2c\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02\x44\x01\x00";
        bytes.extend_from_slice(frame);
        bytes.extend_from_slice(frame);
        bytes.push(0x3b);
        let animation = decode(&bytes).expect("valid two-frame GIF");
        assert!(!animation.is_static_image());
        assert_eq!(animation.width(), 1);
    }
}
