//! Accessible names for widgets that carry no text of their own, and the
//! phrases screen readers get for counts and message rows.
//!
//! GTK builds a button's accessible name from its label. An icon-only
//! button has none, so each one is named here (a tooltip alone is not
//! relied on). Colour never carries a state alone: unread rows have a
//! count, failed and pinned messages say so in words, and a story ring
//! that is seen or unseen differs in thickness as well as colour.

use gtk::prelude::*;

/// Name `widget` for assistive technology.
pub(super) fn label(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Label(text)]);
}

/// Add detail to `widget`'s name, read after it.
pub(super) fn describe(widget: &impl IsA<gtk::Widget>, text: &str) {
    widget
        .as_ref()
        .update_property(&[gtk::accessible::Property::Description(text)]);
}

/// Say that `target` is named by `by`, a label beside it.
pub(super) fn labelled_by(target: &impl IsA<gtk::Widget>, by: &impl IsA<gtk::Widget>) {
    target
        .as_ref()
        .update_relation(&[gtk::accessible::Relation::LabelledBy(&[by
            .as_ref()
            .upcast_ref::<gtk::Accessible>(
        )])]);
}

/// "3 unread messages", for a count shown as a number on a badge.
pub fn unread_phrase(count: u32) -> String {
    match count {
        1 => "1 unread message".to_string(),
        count => format!("{count} unread messages"),
    }
}

/// What a message row is called when a screen reader lands on it: who,
/// when, and any state the visual design shows only as a mark.
pub fn message_phrase(
    sender: &str,
    time: &str,
    edited: bool,
    pinned: bool,
    delivery: Option<&str>,
) -> String {
    let mut phrase = format!("{sender}, {time}");
    for (on, text) in [(edited, "edited"), (pinned, "pinned")] {
        if on {
            phrase.push_str(", ");
            phrase.push_str(text);
        }
    }
    if let Some(delivery) = delivery {
        phrase.push_str(", ");
        phrase.push_str(&delivery.to_lowercase());
    }
    phrase
}

/// A story ring's name: whose, how many, and whether any are unseen.
pub fn story_phrase(name: &str, stories: usize, unseen: bool) -> String {
    let count = match stories {
        1 => "1 story".to_string(),
        count => format!("{count} stories"),
    };
    format!(
        "{name}: {count}, {}",
        if unseen { "unseen" } else { "seen" }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::WaddleWindow;
    use adw::prelude::AdwApplicationWindowExt;
    use gtk::gio;

    /// Every widget under `root`, depth first.
    fn descendants(root: &gtk::Widget, found: &mut Vec<gtk::Widget>) {
        found.push(root.clone());
        let mut child = root.first_child();
        while let Some(widget) = child {
            descendants(&widget, found);
            child = widget.next_sibling();
        }
    }

    trait Ancestors {
        fn ancestors_of_toolkit(&self) -> bool;
    }

    impl Ancestors for gtk::Widget {
        fn ancestors_of_toolkit(&self) -> bool {
            let mut parent = self.parent();
            while let Some(widget) = parent {
                let name = widget.type_().name();
                if name == "AdwGizmo" || name == "GtkSearchBar" || name.ends_with("WindowControls")
                {
                    return true;
                }
                parent = widget.parent();
            }
            false
        }
    }

    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run and --ignored --test-threads=1"]
    fn the_window_names_its_controls_and_wires_its_shortcuts() {
        adw::init().expect("GTK display");
        let app = adw::Application::builder()
            .application_id("social.waddle.A11yTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>)
            .expect("register application");
        let window = WaddleWindow::new(&app);

        // Every action the shortcut table names for the window exists.
        for (action, _) in crate::shortcuts::bindings() {
            if let Some(name) = action.strip_prefix("win.") {
                assert!(window.lookup_action(name).is_some(), "{action} missing");
            }
        }

        // The windows and steps they open build without a session.
        for name in [
            "show-shortcuts",
            "quick-switch",
            "next-conversation",
            "focus-composer",
        ] {
            gio::prelude::ActionGroupExt::activate_action(&window, name, None);
        }
        assert!(
            window.visible_dialog().is_some(),
            "the Keyboard Shortcuts window opened"
        );

        // Icon-only buttons have a tooltip (their name for the pointer) and
        // are at least 24 px square, the WCAG 2.2 target size minimum.
        let mut widgets = Vec::new();
        descendants(window.upcast_ref(), &mut widgets);
        let mut icon_only = 0;
        let mut missing = Vec::new();
        for widget in &widgets {
            let icon = widget
                .downcast_ref::<gtk::Button>()
                .and_then(|button| button.icon_name())
                .or_else(|| {
                    widget
                        .downcast_ref::<gtk::MenuButton>()
                        .and_then(|button| button.icon_name())
                });
            let Some(icon) = icon else { continue };
            // libadwaita's own back and window buttons are named by it.
            if widget.ancestors_of_toolkit() {
                continue;
            }
            icon_only += 1;
            if widget.tooltip_text().is_none() {
                let mut chain = String::new();
                let mut up = widget.parent();
                while let Some(parent) = up {
                    chain.push_str(&format!(" < {}", parent.type_().name()));
                    up = parent.parent();
                }
                missing.push(format!("{icon}{chain}"));
            }
            if widget.is_visible() {
                let (_, width, _, _) = widget.measure(gtk::Orientation::Horizontal, -1);
                let (_, height, _, _) = widget.measure(gtk::Orientation::Vertical, -1);
                assert!(width >= 24 && height >= 24, "{icon} is {width}x{height}");
            }
        }
        assert!(icon_only >= 10, "found only {icon_only} icon buttons");
        assert!(missing.is_empty(), "no tooltip: {missing:?}");
    }

    #[test]
    fn counts_are_spoken_in_full() {
        assert_eq!(unread_phrase(1), "1 unread message");
        assert_eq!(unread_phrase(12), "12 unread messages");
    }

    #[test]
    fn a_message_row_names_its_state_in_words() {
        assert_eq!(
            message_phrase("Ana", "10:42", false, false, None),
            "Ana, 10:42"
        );
        assert_eq!(
            message_phrase("Ana", "10:42", true, true, Some("Not sent")),
            "Ana, 10:42, edited, pinned, not sent"
        );
    }

    #[test]
    fn a_story_ring_says_whether_it_is_unseen() {
        assert_eq!(story_phrase("Bo", 1, true), "Bo: 1 story, unseen");
        assert_eq!(story_phrase("Bo", 3, false), "Bo: 3 stories, seen");
    }
}
