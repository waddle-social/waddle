//! The composer's suggestion popover: `@` mentions and slash commands.
//!
//! It never takes focus from the entry. Up and Down move the selection,
//! Tab or Enter picks it, and Escape dismisses the popover.

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::gdk;

use super::WaddleWindow;
use crate::composer::{self, RoomPeople, Suggestion};
use crate::session::Kind;

impl WaddleWindow {
    pub(super) fn setup_suggestions(&self) {
        let imp = self.imp();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .can_focus(false)
            .build();
        list.add_css_class("navigation-sidebar");
        let window = self.downgrade();
        list.connect_row_activated(move |_, row| {
            if let Some(window) = window.upgrade() {
                window.pick_suggestion(row.index());
            }
        });
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .propagate_natural_height(true)
            .max_content_height(320)
            .margin_top(4)
            .margin_bottom(4)
            .margin_start(4)
            .margin_end(4)
            .child(&list)
            .build();
        let popover = gtk::Popover::builder()
            .autohide(false)
            .has_arrow(false)
            .can_focus(false)
            .position(gtk::PositionType::Top)
            .child(&scroller)
            .build();
        popover.set_parent(&*imp.composer);
        imp.suggestion_popover.replace(Some((popover, list)));

        let entry = &imp.composer_entry;
        let window = self.downgrade();
        entry.connect_changed(move |_| {
            if let Some(window) = window.upgrade() {
                window.refresh_suggestions();
            }
        });
        let window = self.downgrade();
        entry.connect_cursor_position_notify(move |_| {
            if let Some(window) = window.upgrade() {
                window.refresh_suggestions();
            }
        });
        // The popover cannot take focus, so leaving means the user moved on.
        let focus = gtk::EventControllerFocus::new();
        let window = self.downgrade();
        focus.connect_leave(move |_| {
            if let Some(window) = window.upgrade() {
                window.hide_suggestions();
            }
        });
        entry.add_controller(focus);
    }

    /// Focus sits in the entry's inner text widget, not the entry itself.
    fn composer_focused(&self) -> bool {
        let entry = self.imp().composer_entry.get();
        gtk::prelude::GtkWindowExt::focus(self).is_some_and(|focus| {
            focus == entry.clone().upcast::<gtk::Widget>() || focus.is_ancestor(&entry)
        })
    }

    /// Offer what the token at the cursor could become, or hide.
    pub(super) fn refresh_suggestions(&self) {
        let imp = self.imp();
        let Some((popover, list)) = imp.suggestion_popover.borrow().clone() else {
            return;
        };
        let entry = &imp.composer_entry;
        let editing = matches!(*imp.compose.borrow(), super::actions::Compose::Edit { .. });
        let found = match self.current_conversation() {
            Some(conversation) if self.composer_focused() => {
                let text = entry.text();
                let cursor = usize::try_from(entry.position()).unwrap_or(0);
                let occupants = self.mentionable(&conversation);
                let directory = composer::mention_query(&text, cursor)
                    .map(|query| self.directory_mentionable(&conversation, &query))
                    .unwrap_or_default();
                let room = (conversation.kind == Kind::Room).then_some(RoomPeople {
                    occupants: &occupants,
                    directory: &directory,
                });
                composer::suggestions(&text, cursor, room)
                    .into_iter()
                    // A correction replaces a body; it cannot run a command.
                    .filter(|suggestion| !(editing && suggestion.title.starts_with('/')))
                    .collect()
            }
            _ => Vec::new(),
        };
        if found.is_empty() {
            self.hide_suggestions();
            return;
        }
        if *imp.suggestions.borrow() == found && popover.is_visible() {
            return;
        }
        while let Some(row) = list.first_child() {
            list.remove(&row);
        }
        for suggestion in &found {
            list.append(&suggestion_row(suggestion));
        }
        list.select_row(list.row_at_index(0).as_ref());
        imp.suggestions.replace(found);
        let width = imp.composer.width();
        popover.set_size_request(width.max(240), -1);
        if !popover.is_visible() {
            popover.popup();
        }
    }

    pub(super) fn hide_suggestions(&self) {
        let imp = self.imp();
        imp.suggestions.replace(Vec::new());
        if let Some((popover, _)) = imp.suggestion_popover.borrow().as_ref() {
            popover.popdown();
        }
    }

    fn suggestions_shown(&self) -> bool {
        let imp = self.imp();
        !imp.suggestions.borrow().is_empty()
            && imp
                .suggestion_popover
                .borrow()
                .as_ref()
                .is_some_and(|(popover, _)| popover.is_visible())
    }

    /// Handle a composer key while suggestions show; true when consumed.
    pub(super) fn suggestion_key(&self, key: gdk::Key) -> bool {
        if !self.suggestions_shown() {
            return false;
        }
        let Some((_, list)) = self.imp().suggestion_popover.borrow().clone() else {
            return false;
        };
        let count = self.imp().suggestions.borrow().len() as i32;
        let selected = list.selected_row().map_or(0, |row| row.index());
        match key {
            gdk::Key::Up | gdk::Key::KP_Up => {
                let at = (selected - 1).rem_euclid(count);
                list.select_row(list.row_at_index(at).as_ref());
            }
            gdk::Key::Down | gdk::Key::KP_Down => {
                let at = (selected + 1).rem_euclid(count);
                list.select_row(list.row_at_index(at).as_ref());
            }
            gdk::Key::Tab | gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                self.pick_suggestion(selected);
            }
            gdk::Key::Escape => self.hide_suggestions(),
            _ => return false,
        }
        true
    }

    fn pick_suggestion(&self, index: i32) {
        let imp = self.imp();
        let Some(suggestion) = usize::try_from(index)
            .ok()
            .and_then(|at| imp.suggestions.borrow().get(at).cloned())
        else {
            return;
        };
        let entry = &imp.composer_entry;
        let (text, cursor) = composer::apply(&entry.text(), &suggestion);
        self.hide_suggestions();
        entry.set_text(&text);
        entry.grab_focus_without_selecting();
        entry.set_position(i32::try_from(cursor).unwrap_or(-1));
    }
}

fn suggestion_row(suggestion: &Suggestion) -> gtk::ListBoxRow {
    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .margin_top(2)
        .margin_bottom(2)
        .build();
    let title = gtk::Label::builder()
        .label(suggestion.title.as_str())
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    title.add_css_class("heading");
    content.append(&title);
    if !suggestion.subtitle.is_empty() {
        let subtitle = gtk::Label::builder()
            .label(suggestion.subtitle.as_str())
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        subtitle.add_css_class("caption");
        subtitle.add_css_class("dim-label");
        content.append(&subtitle);
    }
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .can_focus(false)
        .build();
    row.update_property(&[gtk::accessible::Property::Label(&format!(
        "{}, {}",
        suggestion.title, suggestion.subtitle
    ))]);
    row
}
