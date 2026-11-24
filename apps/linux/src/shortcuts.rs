//! The keyboard shortcuts the app offers, in one table.
//!
//! The Keyboard Shortcuts window lists this table and the application
//! installs the accelerators of the entries that name an action, so the
//! two cannot drift apart. Entries without an action document keys a
//! widget handles itself (the composer, the search bar). Widget-free, so
//! the table is tested without a display.

pub struct Shortcut {
    pub title: &'static str,
    /// GTK accelerator syntax, as `gtk::accelerator_parse` reads it.
    pub accel: &'static str,
    /// The application or window action the accelerator activates, when
    /// it is installed through `set_accels_for_action`.
    pub action: Option<&'static str>,
}

pub struct Group {
    pub title: &'static str,
    pub shortcuts: &'static [Shortcut],
}

const fn bound(title: &'static str, accel: &'static str, action: &'static str) -> Shortcut {
    Shortcut {
        title,
        accel,
        action: Some(action),
    }
}

const fn local(title: &'static str, accel: &'static str) -> Shortcut {
    Shortcut {
        title,
        accel,
        action: None,
    }
}

pub const GROUPS: &[Group] = &[
    Group {
        title: "General",
        shortcuts: &[
            bound(
                "Keyboard Shortcuts",
                "<Control>question",
                "win.show-shortcuts",
            ),
            bound("Preferences", "<Control>comma", "app.preferences"),
            bound("Close Window", "<Control>w", "window.close"),
            bound("Quit", "<Control>q", "app.quit"),
        ],
    },
    Group {
        title: "Conversations",
        shortcuts: &[
            bound("Quick Switch", "<Control>k", "win.quick-switch"),
            bound("Next Conversation", "<Alt>Down", "win.next-conversation"),
            bound(
                "Previous Conversation",
                "<Alt>Up",
                "win.previous-conversation",
            ),
            local("Search Messages", "<Control>f"),
            local("Message Menu (on a message)", "<Shift>F10"),
        ],
    },
    Group {
        title: "Composer",
        shortcuts: &[
            bound("Focus the Composer", "<Control>l", "win.focus-composer"),
            local("Send", "Return"),
            local("New Line", "<Shift>Return"),
            local("Bold", "<Control>b"),
            local("Italic", "<Control>i"),
            local("Pick a Suggestion", "Tab"),
            local("Close Suggestions, Cancel Reply or Edit", "Escape"),
        ],
    },
];

/// Every entry that installs an accelerator.
pub fn bindings() -> impl Iterator<Item = (&'static str, &'static str)> {
    GROUPS
        .iter()
        .flat_map(|group| group.shortcuts)
        .filter_map(|shortcut| Some((shortcut.action?, shortcut.accel)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// The `<Modifier>` prefixes and the key name, read without GTK.
    fn split(accel: &str) -> (Vec<&str>, &str) {
        let mut modifiers = Vec::new();
        let mut rest = accel;
        while let Some(after) = rest.strip_prefix('<') {
            let (name, tail) = after.split_once('>').expect("closed modifier");
            modifiers.push(name);
            rest = tail;
        }
        (modifiers, rest)
    }

    #[test]
    fn accelerators_are_well_formed() {
        for shortcut in GROUPS.iter().flat_map(|group| group.shortcuts) {
            let (modifiers, key) = split(shortcut.accel);
            assert!(!key.is_empty(), "{} has no key", shortcut.title);
            assert!(
                modifiers
                    .iter()
                    .all(|m| ["Control", "Alt", "Shift", "Super"].contains(m)),
                "{} has an unknown modifier",
                shortcut.title
            );
        }
    }

    // `gtk::accelerator_parse` needs GTK initialised, so a display:
    // `xvfb-run -a cargo test --locked every_accelerator_parses -- --ignored`.
    #[test]
    #[ignore = "needs a display"]
    fn every_accelerator_parses() {
        gtk::init().expect("a display");
        for shortcut in GROUPS.iter().flat_map(|group| group.shortcuts) {
            assert!(
                gtk::accelerator_parse(shortcut.accel).is_some(),
                "{} ({}) does not parse",
                shortcut.title,
                shortcut.accel
            );
        }
    }

    #[test]
    fn no_accelerator_is_listed_twice() {
        let mut seen = HashSet::new();
        for shortcut in GROUPS.iter().flat_map(|group| group.shortcuts) {
            assert!(
                seen.insert(shortcut.accel),
                "{} is bound twice",
                shortcut.accel
            );
        }
    }

    #[test]
    fn actions_are_scoped() {
        for (action, _) in bindings() {
            assert!(
                ["app.", "win.", "window."]
                    .iter()
                    .any(|scope| action.starts_with(scope)),
                "{action} has no scope"
            );
        }
    }

    #[test]
    fn installed_accelerators_never_swallow_typing() {
        // A plain key, Shift+key or Alt+Shift+key typed in the composer
        // must reach the text view; an application accelerator needs
        // Control (or Alt with a navigation key).
        for (action, accel) in bindings() {
            let (modifiers, key) = split(accel);
            let navigation = ["Up", "Down", "Left", "Right", "Page_Up", "Page_Down"];
            assert!(
                modifiers.contains(&"Control")
                    || (modifiers == ["Alt"] && navigation.contains(&key)),
                "{action} on {accel} would swallow typing"
            );
        }
    }
}
