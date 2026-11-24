//! Avatars (XEP-0084) on screen: lookups, redraws when a picture arrives
//! or changes, and publishing or removing this account's own.
//!
//! What is held and when to ask again is `crate::avatars`; here the bytes
//! are decoded (off the main thread, with the same limits as inline
//! images) and the widgets that show them are redrawn.

use std::cell::Cell;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::gdk_pixbuf::prelude::PixbufLoaderExt;
use gtk::{gdk, gdk_pixbuf, gio, glib};

use super::images;
use super::WaddleWindow;
use crate::avatars::{self, Outcome, Request};
use crate::session_profile::describe;

/// Formats read when choosing a picture; others never reach the decoder.
const FORMATS: [&str; 4] = ["png", "jpeg", "gif", "webp"];

impl WaddleWindow {
    /// The picture to show for `jid` now, asking for it when it is not
    /// known yet. `None` shows initials.
    pub(super) fn avatar_texture(&self, jid: &str) -> Option<gdk::Texture> {
        let state = &self.imp().profile;
        // Nobody to ask before the session starts, and a lookup that was
        // never made must not be remembered as made.
        self.imp().session.borrow().as_ref()?;
        let request = state.avatars.borrow_mut().want(jid);
        let texture = state.avatars.borrow().get(jid).cloned();
        if let Some(request) = request {
            self.fetch_avatar(request);
        }
        texture
    }

    /// Show `jid`'s picture in an avatar that stays on screen (a dialog's,
    /// not a recycled list row), and keep it current.
    pub(super) fn watch_avatar(&self, avatar: &adw::Avatar, jid: &str) {
        let state = &self.imp().profile;
        avatar.set_custom_image(self.avatar_texture(jid).as_ref());
        let mut watchers = state.watchers.borrow_mut();
        watchers.retain(|(_, weak)| weak.upgrade().is_some());
        watchers.push((jid.to_lowercase(), avatar.downgrade()));
    }

    fn fetch_avatar(&self, request: Request) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let Request { jid, known_ids } = request;
            let outcome = match session.avatar(jid.clone(), known_ids).await {
                Outcome::Found {
                    id,
                    picture: Some(bytes),
                } => match images::decode_off_main(bytes).await {
                    Ok(decoded) => Outcome::Found {
                        id,
                        picture: Some(decoded.texture()),
                    },
                    // Bytes that do not decode are as good as no picture.
                    Err(_) => Outcome::NoPicture,
                },
                Outcome::Found { id, picture: None } => Outcome::Found { id, picture: None },
                Outcome::NoPicture => Outcome::NoPicture,
                Outcome::Failed => Outcome::Failed,
            };
            if window.imp().generation.get() != generation {
                return;
            }
            let changed = window
                .imp()
                .profile
                .avatars
                .borrow_mut()
                .resolved(&jid, outcome);
            if changed {
                window.avatar_updated(&jid);
            }
        });
    }

    /// XEP-0084 `+notify`: `avatar_id` is the new item id, or none when
    /// the person removed their picture.
    pub(super) fn on_avatar_changed(&self, jid: &str, avatar_id: Option<&str>) {
        let (shown_changed, request) = self
            .imp()
            .profile
            .avatars
            .borrow_mut()
            .changed(jid, avatar_id);
        if shown_changed {
            self.avatar_updated(jid);
        }
        if let Some(request) = request {
            self.fetch_avatar(request);
        }
    }

    /// After a reconnect events may have been missed: check every held
    /// picture again.
    pub(super) fn revalidate_avatars(&self) {
        let requests = self.imp().profile.avatars.borrow_mut().revalidate_all();
        for request in requests {
            self.fetch_avatar(request);
        }
    }

    /// Redraw everything that shows `jid`'s picture.
    fn avatar_updated(&self, jid: &str) {
        let imp = self.imp();
        self.refresh_author_rows(jid);
        if imp
            .conversations
            .borrow()
            .iter()
            .any(|conversation| conversation.jid.eq_ignore_ascii_case(jid))
        {
            self.render_conversations();
        }
        if self
            .own_jid()
            .is_some_and(|own| own.eq_ignore_ascii_case(jid))
        {
            self.refresh_account_row();
        }
        let texture = imp.profile.avatars.borrow().get(jid).cloned();
        let wanted = jid.to_lowercase();
        imp.profile.watchers.borrow_mut().retain(|(watched, weak)| {
            let Some(avatar) = weak.upgrade() else {
                return false;
            };
            if *watched == wanted {
                avatar.set_custom_image(texture.as_ref());
            }
            true
        });
        self.refresh_people();
    }

    /// Choose an image file and publish it as this account's avatar.
    pub(super) fn choose_picture(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Images"));
        filter.add_mime_type("image/png");
        filter.add_mime_type("image/jpeg");
        filter.add_mime_type("image/gif");
        filter.add_mime_type("image/webp");
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Choose a Picture")
            .filters(&filters)
            .build();
        let window = self.clone();
        dialog.open(Some(self), None::<&gio::Cancellable>, move |chosen| {
            // Dismissing the chooser is not an error.
            let Ok(file) = chosen else {
                return;
            };
            let window = window.clone();
            glib::spawn_future_local(async move { window.publish_picture(file).await });
        });
    }

    async fn publish_picture(&self, file: gio::File) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let bytes = match file.load_bytes_future().await {
            Ok((bytes, _)) => bytes,
            Err(error) => {
                return self.toast(&format!("Could not read the file: {}", error.message()))
            }
        };
        if u64::try_from(bytes.len()).map_or(true, |len| len > avatars::MAX_FILE_BYTES) {
            return self.toast("That picture is too large.");
        }
        let encoded = gio::spawn_blocking(move || encode_avatar(&bytes)).await;
        let (png, width, height) = match encoded {
            Ok(Ok(encoded)) => encoded,
            Ok(Err(message)) => return self.toast(&message),
            Err(_) => return self.toast("The picture could not be prepared."),
        };
        let published = session
            .publish_avatar(
                png,
                u32::try_from(width).unwrap_or(0),
                u32::try_from(height).unwrap_or(0),
            )
            .await;
        if imp.generation.get() != generation {
            return;
        }
        match published {
            Ok(id) => {
                self.toast("Picture updated.");
                self.own_picture_published(&id);
            }
            Err(error) => self.toast(&format!(
                "Could not update your picture. {}",
                describe(&error)
            )),
        }
    }

    /// Fetch what was just published, to show it.
    fn own_picture_published(&self, id: &str) {
        let Some(own) = self.own_jid() else {
            return;
        };
        let request = {
            let mut cache = self.imp().profile.avatars.borrow_mut();
            cache.want(&own).or_else(|| cache.changed(&own, Some(id)).1)
        };
        if let Some(request) = request {
            self.fetch_avatar(request);
        }
    }

    /// Remove this account's avatar (XEP-0084 §4.3).
    pub(super) fn remove_picture(&self) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let generation = imp.generation.get();
        let window = self.clone();
        glib::spawn_future_local(async move {
            let removed = session.remove_avatar().await;
            if window.imp().generation.get() != generation {
                return;
            }
            match removed {
                Ok(()) => {
                    window.toast("Picture removed.");
                    if let Some(own) = window.own_jid() {
                        let mut cache = window.imp().profile.avatars.borrow_mut();
                        let _ = cache.want(&own);
                        let (changed, _) = cache.changed(&own, None);
                        drop(cache);
                        if changed {
                            window.avatar_updated(&own);
                        }
                    }
                }
                Err(error) => window.toast(&format!(
                    "Could not remove your picture. {}",
                    describe(&error)
                )),
            }
        });
    }
}

/// Scale an image file's bytes to fit 512 pixels on a side (never
/// enlarging) and encode it as the PNG XEP-0084 requires. Blocking: run it
/// off the main thread. Images over the inline-image pixel limit and
/// formats other than PNG, JPEG, GIF and WebP are refused before decoding.
fn encode_avatar(bytes: &[u8]) -> Result<(Vec<u8>, i32, i32), String> {
    let loader = gdk_pixbuf::PixbufLoader::new();
    let refusal = std::rc::Rc::new(Cell::new(None::<&'static str>));
    let flag = refusal.clone();
    loader.connect_size_prepared(move |loader, width, height| {
        let format = loader
            .format()
            .and_then(|format| format.name())
            .map(|name| name.to_string());
        let pixels = u64::try_from(width)
            .unwrap_or(u64::MAX)
            .saturating_mul(u64::try_from(height).unwrap_or(u64::MAX));
        if !format.is_some_and(|name| FORMATS.contains(&name.as_str())) {
            flag.set(Some("That kind of file cannot be used as a picture."));
        } else if pixels > images::MAX_PIXELS || width <= 0 || height <= 0 {
            flag.set(Some("That picture is too large."));
        }
        if flag.get().is_some() {
            // Stops the module before it allocates the image.
            loader.set_size(0, 0);
            return;
        }
        let (fit_width, fit_height) = avatars::publish_size(width, height);
        if (fit_width, fit_height) != (width, height) {
            loader.set_size(fit_width, fit_height);
        }
    });
    let unreadable = || "The picture could not be read.".to_string();
    for chunk in bytes.chunks(64 * 1024) {
        let written = loader.write(chunk);
        if let Some(message) = refusal.get() {
            let _ = loader.close();
            return Err(message.to_string());
        }
        written.map_err(|_| unreadable())?;
    }
    let closed = loader.close();
    if let Some(message) = refusal.get() {
        return Err(message.to_string());
    }
    closed.map_err(|_| unreadable())?;
    let pixbuf = loader.pixbuf().ok_or_else(unreadable)?;
    let png = pixbuf
        .save_to_bufferv("png", &[])
        .map_err(|_| "The picture could not be prepared.".to_string())?;
    Ok((png, pixbuf.width(), pixbuf.height()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: i32, height: i32) -> Vec<u8> {
        let pixbuf = gdk_pixbuf::Pixbuf::new(gdk_pixbuf::Colorspace::Rgb, true, 8, width, height)
            .expect("pixbuf");
        pixbuf.fill(0x3366_99ff);
        pixbuf.save_to_bufferv("png", &[]).expect("png")
    }

    #[test]
    fn a_large_picture_is_scaled_to_512_and_encoded_as_png() {
        let (encoded, width, height) = encode_avatar(&png(1024, 768)).expect("encoded");
        assert_eq!((width, height), (512, 384));
        assert_eq!(&encoded[..8], b"\x89PNG\r\n\x1a\n");
    }

    #[test]
    fn a_small_picture_is_not_enlarged() {
        let (_, width, height) = encode_avatar(&png(64, 32)).expect("encoded");
        assert_eq!((width, height), (64, 32));
    }

    #[test]
    fn files_that_are_not_pictures_are_refused() {
        assert!(encode_avatar(b"not an image at all").is_err());
        assert!(encode_avatar(b"").is_err());
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10"/>"#;
        assert!(encode_avatar(svg).is_err());
    }
}
