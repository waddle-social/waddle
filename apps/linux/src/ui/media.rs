//! Inline linked images. Each bind owns its widget; late loads cannot paint a recycled row.
use std::time::{Duration, SystemTime};

static DOWNLOADS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

use gtk::{gdk, gdk_pixbuf, glib, prelude::*};
use url::Url;

use crate::runtime;

const MAX_BYTES: usize = 8 * 1024 * 1024;

fn image_urls(body: &str) -> Vec<Url> {
    body.split_whitespace()
        .filter_map(|word| Url::parse(word).ok())
        .filter(|url| {
            url.scheme() == "https"
                && url.username().is_empty()
                && url.password().is_none()
                && [".gif", ".png", ".jpg", ".jpeg", ".webp"]
                    .iter()
                    .any(|ext| url.path().to_ascii_lowercase().ends_with(ext))
        })
        .take(4)
        .collect()
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
        let scale = (480.0 / f64::from(width))
            .min(360.0 / f64::from(height))
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
    if unchanged {
        return;
    }
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }
    if retracted {
        container.set_visible(false);
        return;
    }
    container.set_visible(!urls.is_empty());
    for url in urls {
        let picture = gtk::Picture::builder()
            .halign(gtk::Align::Start)
            .can_shrink(true)
            .alternative_text("Shared image")
            .visible(false)
            .build();
        picture.set_widget_name(url.as_str());
        container.append(&picture);
        let weak = picture.downgrade();
        glib::spawn_future_local(async move {
            let download = runtime::spawn(download(url));
            let cancel = download.abort_handle();
            if let Some(picture) = weak.upgrade() {
                picture.add_weak_ref_notify_local(move || cancel.abort());
            } else {
                download.abort();
                return;
            }
            let result = download.await;
            let Some(picture) = weak.upgrade() else {
                return;
            };
            let Ok(Ok(bytes)) = result else {
                return;
            };
            let Ok(animation) = decode(&bytes) else {
                return;
            };
            picture.set_size_request(-1, animation.height().min(240));
            let iter = animation.iter(Some(SystemTime::now()));
            picture.set_paintable(Some(&gdk::Texture::for_pixbuf(&iter.pixbuf())));
            picture.set_visible(true);
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
        });
    }
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
