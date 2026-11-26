//! Initials avatars tinted as on the web and phones: DJB2 over the display
//! name picks one of the six Polar Huddle avatar tints (see `data/style.css`).

use adw::prelude::WidgetExt;

const TINTS: u32 = 6;

pub(super) fn avatar(size: i32) -> adw::Avatar {
    let avatar = adw::Avatar::new(size, None, true);
    avatar.add_css_class("waddle-avatar");
    avatar
}

pub(super) fn set_name(avatar: &adw::Avatar, name: &str) {
    if avatar.text().as_deref() == Some(name) {
        return;
    }
    avatar.set_text(Some(name));
    for tint in 0..TINTS {
        avatar.remove_css_class(&format!("tint-{tint}"));
    }
    avatar.add_css_class(&format!("tint-{}", tint(name)));
}

fn tint(name: &str) -> u32 {
    name.chars().fold(5381u32, |hash, c| {
        hash.wrapping_mul(33).wrapping_add(u32::from(c))
    }) % TINTS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tints_follow_the_shared_hash() {
        // djb2("a") = 5381 * 33 + 97 = 177670, 177670 % 6 = 4.
        assert_eq!(tint("a"), 4);
        assert_eq!(tint(""), 5381 % 6);
    }
}
