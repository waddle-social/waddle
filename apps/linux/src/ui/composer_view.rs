//! The composer's text view and formatting bar.
//!
//! The composer is a `gtk::TextView` that grows with its text (to a
//! limit), so a message can span lines and hold code blocks. Enter sends
//! and Shift+Enter breaks the line (see `actions.rs`). The bar inserts
//! the markdown markers `markup.rs` turns into XEP-0394 spans when the
//! message is sent.

use adw::prelude::{
    AccessibleExtManual, ButtonExt, ObjectExt, TextBufferExt, TextViewExt, ToggleButtonExt,
    WidgetExt,
};
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::prelude::BoxExt;
use waddle_client_session::TimelineItem;

use super::WaddleWindow;
use crate::markup::{self, Format};

/// The text, caret and selection of the composer, in characters.
pub(super) trait ComposerText {
    fn text(&self) -> String;
    fn set_text(&self, text: &str);
    /// The caret, or -1 for the end when setting.
    fn position(&self) -> i32;
    fn set_position(&self, position: i32);
    /// The selection, or an empty range at the caret.
    fn selection(&self) -> (usize, usize);
    fn select(&self, start: usize, end: usize);
}

fn offset(position: i32) -> usize {
    usize::try_from(position).unwrap_or(0)
}

impl ComposerText for gtk::TextView {
    fn text(&self) -> String {
        let buffer = self.buffer();
        buffer
            .text(&buffer.start_iter(), &buffer.end_iter(), false)
            .to_string()
    }

    fn set_text(&self, text: &str) {
        self.buffer().set_text(text);
    }

    fn position(&self) -> i32 {
        self.buffer().cursor_position()
    }

    fn set_position(&self, position: i32) {
        let buffer = self.buffer();
        let iter = if position < 0 {
            buffer.end_iter()
        } else {
            buffer.iter_at_offset(position)
        };
        buffer.place_cursor(&iter);
    }

    fn selection(&self) -> (usize, usize) {
        let buffer = self.buffer();
        match buffer.selection_bounds() {
            Some((start, end)) => (offset(start.offset()), offset(end.offset())),
            None => {
                let caret = offset(buffer.cursor_position());
                (caret, caret)
            }
        }
    }

    fn select(&self, start: usize, end: usize) {
        let buffer = self.buffer();
        let at = |position: usize| buffer.iter_at_offset(i32::try_from(position).unwrap_or(-1));
        buffer.select_range(&at(start), &at(end));
    }
}

/// How a format reads on its button.
fn presentation(format: Format) -> &'static str {
    match format {
        Format::Bold => "<b>B</b>",
        Format::Italic => "<i>I</i>",
        Format::Strikethrough => "<s>S</s>",
        Format::Code => "<tt>&lt;/&gt;</tt>",
        Format::CodeBlock => "<tt>{ }</tt>",
        Format::Quote => "\u{201c}",
    }
}

/// The markup spans recorded for a row of the open conversation.
pub(super) fn spans_of(window: &WaddleWindow, item: &TimelineItem) -> Vec<markup::Span> {
    window
        .current_conversation()
        .map(|conversation| {
            markup::lookup(
                &conversation.jid,
                &[item.correction_id.as_deref(), item.reference_id.as_deref()],
                &item.body,
            )
        })
        .unwrap_or_default()
}

impl WaddleWindow {
    pub(super) fn setup_composer_view(&self) {
        let imp = self.imp();
        let entry = &imp.composer_entry;
        entry.update_property(&[gtk::accessible::Property::Label("Message")]);
        // The placeholder shows while the text is empty.
        let placeholder = imp.composer_placeholder.get();
        entry.buffer().connect_changed(move |buffer| {
            placeholder.set_visible(buffer.char_count() == 0);
        });

        for format in Format::ALL {
            let label = gtk::Label::builder()
                .use_markup(true)
                .label(presentation(format))
                .build();
            let button = gtk::Button::builder()
                .child(&label)
                .tooltip_text(format.title())
                .focus_on_click(false)
                .build();
            button.add_css_class("flat");
            button.update_property(&[gtk::accessible::Property::Label(format.title())]);
            let window = self.downgrade();
            button.connect_clicked(move |_| {
                if let Some(window) = window.upgrade() {
                    window.apply_format(format);
                }
            });
            imp.format_bar.append(&button);
        }
    }

    pub(super) fn show_format_bar(&self, show: bool) {
        let imp = self.imp();
        imp.format_bar.set_visible(show);
        if imp.format_toggle.is_active() != show {
            imp.format_toggle.set_active(show);
        }
    }

    /// Wrap the selection in a format's markers.
    pub(super) fn apply_format(&self, format: Format) {
        let entry = &self.imp().composer_entry;
        // The composer is insensitive until the conversation can be sent to.
        if !entry.is_sensitive() {
            return;
        }
        let edit = markup::apply(format, &entry.text(), entry.selection());
        entry.set_text(&edit.text);
        entry.select(edit.selection.0, edit.selection.1);
        entry.grab_focus();
    }
}
