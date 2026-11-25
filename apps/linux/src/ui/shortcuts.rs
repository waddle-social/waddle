//! Keyboard navigation: the Keyboard Shortcuts window, quick switching
//! between conversations, and moving to the next or previous one.
//!
//! The accelerators themselves are installed from `crate::shortcuts`.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};

use super::WaddleWindow;
use crate::session::Conversation;
use crate::shortcuts::GROUPS;

impl WaddleWindow {
    pub(super) fn setup_shortcuts(&self) {
        let show = gio::ActionEntry::builder("show-shortcuts")
            .activate(|window: &WaddleWindow, _, _| window.show_shortcuts())
            .build();
        let switch = gio::ActionEntry::builder("quick-switch")
            .activate(|window: &WaddleWindow, _, _| window.show_quick_switch())
            .build();
        let next = gio::ActionEntry::builder("next-conversation")
            .activate(|window: &WaddleWindow, _, _| window.step_conversation(1))
            .build();
        let previous = gio::ActionEntry::builder("previous-conversation")
            .activate(|window: &WaddleWindow, _, _| window.step_conversation(-1))
            .build();
        let focus = gio::ActionEntry::builder("focus-composer")
            .activate(|window: &WaddleWindow, _, _| {
                let composer = &window.imp().composer_entry;
                if composer.is_sensitive() && composer.is_mapped() {
                    composer.grab_focus();
                }
            })
            .build();
        self.add_action_entries([show, switch, next, previous, focus]);
    }

    /// The conversations that can be switched to: rooms and chats, not
    /// the thread views opened from them.
    fn switchable(&self) -> Vec<Conversation> {
        self.imp()
            .conversations
            .borrow()
            .iter()
            .filter(|conversation| conversation.thread.is_none())
            .cloned()
            .collect()
    }

    /// Open the next (or previous) conversation in sidebar order, wrapping.
    fn step_conversation(&self, direction: i32) {
        let list = self.switchable();
        if list.is_empty() {
            return;
        }
        let current = self.current_conversation().map(|c| c.jid);
        let position = current
            .as_deref()
            .and_then(|jid| list.iter().position(|conversation| conversation.jid == jid));
        let len = list.len() as i32;
        let target = match position {
            Some(at) => (at as i32 + direction).rem_euclid(len),
            None if direction > 0 => 0,
            None => len - 1,
        };
        if let Some(conversation) = usize::try_from(target).ok().and_then(|at| list.get(at)) {
            self.open_conversation(&conversation.key());
        }
    }

    fn show_quick_switch(&self) {
        let list = self.switchable();
        if list.is_empty() {
            return;
        }
        let entry = gtk::SearchEntry::builder()
            .placeholder_text("Go to a room or chat")
            .hexpand(true)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label("Go to a room or chat")]);
        let rows = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Browse)
            .build();
        rows.add_css_class("navigation-sidebar");
        for conversation in &list {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&conversation.name))
                .title_lines(1)
                .activatable(true)
                .build();
            row.set_widget_name(&conversation.key());
            rows.append(&row);
        }
        let names: Rc<Vec<String>> = Rc::new(
            list.iter()
                .map(|conversation| conversation.name.to_lowercase())
                .collect(),
        );
        let query = Rc::new(RefCell::new(String::new()));
        {
            let (names, query) = (names.clone(), query.clone());
            rows.set_filter_func(move |row| {
                let query = query.borrow();
                usize::try_from(row.index())
                    .ok()
                    .and_then(|at| names.get(at))
                    .is_some_and(|name| name.contains(query.as_str()))
            });
        }
        select_first(&rows, &names, "");

        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&rows)
            .build();
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(8)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(12)
            .build();
        content.append(&entry);
        content.append(&scroller);
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&content));
        let dialog = adw::Dialog::builder()
            .title("Quick Switch")
            .content_width(420)
            .content_height(420)
            .focus_widget(&entry)
            .child(&view)
            .build();

        {
            let (rows, names, query) = (rows.clone(), names.clone(), query.clone());
            entry.connect_search_changed(move |entry| {
                query.replace(entry.text().to_lowercase());
                rows.invalidate_filter();
                select_first(&rows, &names, &query.borrow());
            });
        }
        {
            let (rows, names, query) = (rows.clone(), names.clone(), query.clone());
            entry.connect_next_match(move |_| move_selection(&rows, &names, &query.borrow(), true));
        }
        {
            let (rows, names, query) = (rows.clone(), names.clone(), query.clone());
            entry.connect_previous_match(move |_| {
                move_selection(&rows, &names, &query.borrow(), false)
            });
        }
        let open = {
            let window = self.downgrade();
            let dialog = dialog.clone();
            move |row: &gtk::ListBoxRow| {
                dialog.close();
                if let Some(window) = window.upgrade() {
                    window.open_conversation(&row.widget_name());
                }
            }
        };
        {
            let (rows, open) = (rows.clone(), open.clone());
            entry.connect_activate(move |_| {
                if let Some(row) = rows.selected_row() {
                    open(&row);
                }
            });
        }
        rows.connect_row_activated(move |_, row| open(row));
        dialog.present(Some(self));
    }

    fn show_shortcuts(&self) {
        let page = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .margin_start(12)
            .margin_end(12)
            .margin_top(12)
            .margin_bottom(18)
            .build();
        for group in GROUPS {
            let section = adw::PreferencesGroup::builder().title(group.title).build();
            for shortcut in group.shortcuts {
                let row = adw::ActionRow::builder()
                    .title(shortcut.title)
                    .activatable(false)
                    .build();
                let label = gtk::accelerator_parse(shortcut.accel)
                    .map(|(key, modifiers)| gtk::accelerator_get_label(key, modifiers))
                    .unwrap_or_else(|| shortcut.accel.into());
                let keys = gtk::Label::builder()
                    .label(label.as_str())
                    .valign(gtk::Align::Center)
                    .build();
                keys.add_css_class("keycap");
                row.add_suffix(&keys);
                section.add(&row);
            }
            page.append(&section);
        }
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&adw::Clamp::builder().maximum_size(520).child(&page).build())
            .build();
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        view.set_content(Some(&scroller));
        let dialog = adw::Dialog::builder()
            .title("Keyboard Shortcuts")
            .content_width(480)
            .content_height(560)
            .child(&view)
            .build();
        dialog.present(Some(self));
    }
}

/// Indexes of the names that contain the (lowercased) query.
fn matching(names: &[String], query: &str) -> Vec<i32> {
    names
        .iter()
        .enumerate()
        .filter(|(_, name)| name.contains(query))
        .filter_map(|(at, _)| i32::try_from(at).ok())
        .collect()
}

fn select_first(rows: &gtk::ListBox, names: &[String], query: &str) {
    let first = matching(names, query).first().copied();
    rows.select_row(first.and_then(|at| rows.row_at_index(at)).as_ref());
}

fn move_selection(rows: &gtk::ListBox, names: &[String], query: &str, forward: bool) {
    let visible = matching(names, query);
    let current = rows.selected_row().map(|row| row.index());
    let target = match current.and_then(|at| visible.iter().position(|&v| v == at)) {
        Some(at) if forward => visible.get(at + 1),
        Some(at) => at.checked_sub(1).and_then(|at| visible.get(at)),
        None => visible.first(),
    };
    if let Some(row) = target.and_then(|&at| rows.row_at_index(at)) {
        rows.select_row(Some(&row));
    }
}

#[cfg(test)]
mod tests {
    use super::matching;

    #[test]
    fn matching_keeps_list_order() {
        let names: Vec<String> = ["general", "design", "random"]
            .iter()
            .map(|name| name.to_string())
            .collect();
        assert_eq!(matching(&names, ""), vec![0, 1, 2]);
        assert_eq!(matching(&names, "g"), vec![0, 1]);
        assert_eq!(matching(&names, "zzz"), Vec::<i32>::new());
    }
}
