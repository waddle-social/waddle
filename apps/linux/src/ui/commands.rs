//! Server extension commands (XEP-0050) from the composer.
//!
//! The server's commands are discovered when the session connects and
//! offered beside the built-in slash commands. Typing `/word text` runs
//! one: a command that takes its argument inline is submitted with the
//! text; anything that needs more (several stages, other required
//! fields) opens its XEP-0004 form in a dialog. What decides this is in
//! `extension_commands.rs`.

use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::glib;
use waddle_xmpp_client_ffi::{
    WaddleAdhocAction, WaddleAdhocStatus, WaddleExtensionCommand, WaddleExtensionCommandForm,
    WaddleExtensionCommandFormField, WaddleExtensionCommandNote, WaddleExtensionCommandResult,
    WaddleExtensionFieldType, WaddleExtensionNoteType,
};

use super::composer_view::ComposerText;
use super::WaddleWindow;
use crate::extension_commands::{self, InlinePlan, Invocation, SubmitError};
use crate::session::{Conversation, Kind};

/// What a form field's widget holds, to read back on submit.
enum Reader {
    Line(adw::EntryRow),
    Lines(gtk::TextView),
    Switch(adw::SwitchRow),
    Choice(adw::ComboRow, Vec<String>),
    Choices(Vec<(gtk::CheckButton, String)>),
    /// Shown or hidden as the service sent it; submitted unchanged.
    Keep,
}

impl Reader {
    fn read(&self) -> Option<Vec<String>> {
        Some(match self {
            Self::Line(row) => {
                let text = row.text().to_string();
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![text]
                }
            }
            Self::Lines(view) => view.text().lines().map(str::to_string).collect(),
            Self::Switch(row) => vec![if row.is_active() { "1" } else { "0" }.to_string()],
            Self::Choice(row, values) => values
                .get(row.selected() as usize)
                .filter(|value| !value.is_empty())
                .cloned()
                .into_iter()
                .collect(),
            Self::Choices(checks) => checks
                .iter()
                .filter(|(check, _)| check.is_active())
                .map(|(_, value)| value.clone())
                .collect(),
            Self::Keep => return None,
        })
    }
}

fn label_of(field: &WaddleExtensionCommandFormField) -> String {
    field
        .label
        .clone()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| field.var.clone())
}

fn title_of(field: &WaddleExtensionCommandFormField) -> String {
    let label = label_of(field);
    if field.required {
        format!("{label} *")
    } else {
        label
    }
}

/// One field's widget in `page`, and how to read it.
fn field_widget(field: &WaddleExtensionCommandFormField, page: &gtk::Box) -> Reader {
    let group = |title: Option<&str>| {
        let group = adw::PreferencesGroup::new();
        if let Some(title) = title {
            group.set_title(&glib::markup_escape_text(title));
        }
        page.append(&group);
        group
    };
    if field.blocked || field.field_type == WaddleExtensionFieldType::TextPrivate {
        // Never an input: the client does not collect secrets.
        let note = gtk::Label::builder()
            .label(format!(
                "{} is a secret, which this client does not collect.",
                label_of(field)
            ))
            .wrap(true)
            .xalign(0.0)
            .build();
        note.add_css_class("dim-label");
        page.append(&note);
        return Reader::Keep;
    }
    let first = field.values.first().cloned().unwrap_or_default();
    match field.field_type {
        WaddleExtensionFieldType::Hidden => Reader::Keep,
        WaddleExtensionFieldType::Fixed => {
            let text = gtk::Label::builder()
                .label(field.values.join("\n"))
                .wrap(true)
                .xalign(0.0)
                .selectable(true)
                .build();
            page.append(&text);
            Reader::Keep
        }
        WaddleExtensionFieldType::Boolean => {
            let row = adw::SwitchRow::builder()
                .title(glib::markup_escape_text(&title_of(field)))
                .active(matches!(first.as_str(), "1" | "true"))
                .build();
            group(None).add(&row);
            Reader::Switch(row)
        }
        WaddleExtensionFieldType::ListSingle => {
            let mut labels: Vec<String> = Vec::new();
            let mut values: Vec<String> = Vec::new();
            if !field.required {
                labels.push("None".to_string());
                values.push(String::new());
            }
            for option in &field.options {
                labels.push(option.label.clone().unwrap_or_else(|| option.value.clone()));
                values.push(option.value.clone());
            }
            let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
            let row = adw::ComboRow::builder()
                .title(glib::markup_escape_text(&title_of(field)))
                .model(&gtk::StringList::new(&refs))
                .build();
            if let Some(at) = values.iter().position(|value| *value == first) {
                row.set_selected(u32::try_from(at).unwrap_or(0));
            }
            group(None).add(&row);
            Reader::Choice(row, values)
        }
        WaddleExtensionFieldType::ListMulti => {
            let group = group(Some(&title_of(field)));
            let checks = field
                .options
                .iter()
                .map(|option| {
                    let check = gtk::CheckButton::builder()
                        .active(field.values.contains(&option.value))
                        .valign(gtk::Align::Center)
                        .build();
                    let row = adw::ActionRow::builder()
                        .title(glib::markup_escape_text(
                            option.label.as_deref().unwrap_or(&option.value),
                        ))
                        .activatable_widget(&check)
                        .build();
                    row.add_prefix(&check);
                    group.add(&row);
                    (check, option.value.clone())
                })
                .collect();
            Reader::Choices(checks)
        }
        WaddleExtensionFieldType::TextMulti | WaddleExtensionFieldType::JidMulti => {
            let group = group(Some(&title_of(field)));
            let view = gtk::TextView::builder()
                .wrap_mode(gtk::WrapMode::WordChar)
                .top_margin(8)
                .bottom_margin(8)
                .left_margin(8)
                .right_margin(8)
                .build();
            view.set_text(&field.values.join("\n"));
            let scroller = gtk::ScrolledWindow::builder()
                .min_content_height(80)
                .max_content_height(200)
                .child(&view)
                .build();
            scroller.add_css_class("card");
            group.add(&scroller);
            Reader::Lines(view)
        }
        _ => {
            let row = adw::EntryRow::builder()
                .title(glib::markup_escape_text(&title_of(field)))
                .text(first)
                .build();
            group(None).add(&row);
            Reader::Line(row)
        }
    }
}

/// What a finished command says: its notes, else a plain word.
fn outcome_text(result: &WaddleExtensionCommandResult) -> String {
    let notes: Vec<&str> = result
        .notes
        .iter()
        .map(|note: &WaddleExtensionCommandNote| note.value.trim())
        .filter(|note| !note.is_empty())
        .collect();
    if !notes.is_empty() {
        return notes.join(" ");
    }
    let failed = result
        .notes
        .iter()
        .any(|note| note.note_type == WaddleExtensionNoteType::Error);
    match (result.status, failed) {
        (_, true) => "The command failed.".to_string(),
        (WaddleAdhocStatus::Canceled, _) => "The command was canceled.".to_string(),
        (WaddleAdhocStatus::Completed, _) => "Done.".to_string(),
        (WaddleAdhocStatus::Executing, _) => {
            "The command needs input this client cannot give.".to_string()
        }
    }
}

impl WaddleWindow {
    /// Ask the server for its extension commands, and keep them for the
    /// composer's suggestions. A failure keeps what is known.
    pub(super) fn discover_extension_commands(&self, generation: u64, connect_epoch: u64) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let window = self.clone();
        glib::spawn_future_local(async move {
            let Some(commands) = session.extension_commands().await else {
                return;
            };
            let imp = window.imp();
            if imp.generation.get() != generation
                || imp.connect_epoch.get() != connect_epoch
                || !imp.connected.get()
            {
                return;
            }
            imp.extension_commands.replace(commands);
            window.refresh_suggestions();
        });
    }

    /// Run a command typed as `/word trailing` in `conversation`.
    pub(super) fn run_extension_command(
        &self,
        command: WaddleExtensionCommand,
        trailing: String,
        conversation: &Conversation,
    ) {
        let imp = self.imp();
        let Some(session) = imp.session.borrow().clone() else {
            return;
        };
        let room = (conversation.kind == Kind::Room).then(|| conversation.jid.clone());
        let invocation = extension_commands::invocation(&command, &trailing);
        let window = self.clone();
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            let started = session
                .invoke_extension_command(&command, room.clone())
                .await;
            if window.imp().generation.get() != generation {
                return;
            }
            let Some(invoked) = started else {
                return window.toast("The command could not be run.");
            };
            let result = match invocation {
                Invocation::Execute => invoked,
                Invocation::OpenForm { prefill } => WaddleExtensionCommandResult {
                    form: invoked.form.map(|form| match &prefill {
                        Some(text) => extension_commands::prefill_first_required(&form, text),
                        None => form,
                    }),
                    ..invoked
                },
                Invocation::InlineSubmit { field, value } => {
                    match extension_commands::plan_inline(invoked, &field, &value) {
                        InlinePlan::Finished(result) | InlinePlan::NeedsInput(result) => result,
                        InlinePlan::Submit(stage) => {
                            let Ok(fields) = extension_commands::submission(
                                stage.form.as_ref(),
                                WaddleAdhocAction::Complete,
                            ) else {
                                return window.present_command_result(command, room, stage);
                            };
                            let submitted = session
                                .submit_extension_command(
                                    &command,
                                    stage.session_id.clone(),
                                    fields,
                                    WaddleAdhocAction::Complete,
                                    room.clone(),
                                )
                                .await;
                            if window.imp().generation.get() != generation {
                                return;
                            }
                            match submitted {
                                Some(result) => result,
                                None => return window.toast("The command could not be run."),
                            }
                        }
                    }
                }
            };
            window.present_command_result(command, room, result);
        });
    }

    /// A command's answer: its form in a dialog while it wants input,
    /// else what it says as a toast.
    fn present_command_result(
        &self,
        command: WaddleExtensionCommand,
        room: Option<String>,
        result: WaddleExtensionCommandResult,
    ) {
        if result.status == WaddleAdhocStatus::Executing && result.form.is_some() {
            let title = result
                .form
                .as_ref()
                .and_then(|form| form.title.clone())
                .unwrap_or_else(|| command.name.clone());
            let dialog = adw::Dialog::builder()
                .title(glib::markup_escape_text(&title))
                .content_width(440)
                .content_height(480)
                .build();
            fill_dialog(self, &dialog, command, room, result);
            dialog.present(Some(self));
        } else {
            self.toast(&outcome_text(&result));
        }
    }
}

/// Build one stage of a command's form into `dialog`.
fn fill_dialog(
    window: &WaddleWindow,
    dialog: &adw::Dialog,
    command: WaddleExtensionCommand,
    room: Option<String>,
    stage: WaddleExtensionCommandResult,
) {
    let form = stage.form.clone().unwrap_or(WaddleExtensionCommandForm {
        title: None,
        instructions: None,
        fields: Vec::new(),
    });
    let page = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    if let Some(instructions) = form.instructions.as_deref().filter(|text| !text.is_empty()) {
        let label = gtk::Label::builder()
            .label(instructions)
            .wrap(true)
            .xalign(0.0)
            .build();
        page.append(&label);
    }
    for note in &stage.notes {
        let label = gtk::Label::builder()
            .label(note.value.as_str())
            .wrap(true)
            .xalign(0.0)
            .build();
        label.add_css_class(match note.note_type {
            WaddleExtensionNoteType::Error => "error",
            WaddleExtensionNoteType::Warn => "warning",
            WaddleExtensionNoteType::Info => "dim-label",
        });
        page.append(&label);
    }
    let readers: Vec<Reader> = form
        .fields
        .iter()
        .map(|field| field_widget(field, &page))
        .collect();
    let readers = Rc::new(readers);
    let blocked = extension_commands::blocked_field(&form).is_some();

    let status = gtk::Label::builder()
        .wrap(true)
        .xalign(0.0)
        .visible(false)
        .build();
    status.add_css_class("error");
    page.append(&status);

    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build();

    // Advance the session with `action`, then show the next stage or the end.
    let step: Rc<dyn Fn(WaddleAdhocAction)> = {
        let (window, dialog, command, room, stage, readers, status, scroller) = (
            window.clone(),
            dialog.clone(),
            command.clone(),
            room.clone(),
            stage.clone(),
            Rc::clone(&readers),
            status.clone(),
            scroller.clone(),
        );
        Rc::new(move |action| {
            let mut form = stage.form.clone();
            if let Some(form) = form.as_mut() {
                for (field, reader) in form.fields.iter_mut().zip(readers.iter()) {
                    if let Some(values) = reader.read() {
                        field.values = values;
                    }
                }
            }
            let fields = match extension_commands::submission(form.as_ref(), action) {
                Ok(fields) => fields,
                Err(error) => {
                    let text = match (error, form.as_ref()) {
                        (SubmitError::Missing(vars), Some(form)) => {
                            let names: Vec<String> = form
                                .fields
                                .iter()
                                .filter(|field| vars.contains(&field.var))
                                .map(label_of)
                                .collect();
                            format!("Fill in: {}.", names.join(", "))
                        }
                        _ => "This command asks for something this client cannot send.".to_string(),
                    };
                    status.set_label(&text);
                    status.set_visible(true);
                    return;
                }
            };
            status.set_visible(false);
            scroller.set_sensitive(false);
            let Some(session) = window.imp().session.borrow().clone() else {
                return;
            };
            let generation = window.imp().generation.get();
            let (window, dialog, command, room, stage, status, scroller) = (
                window.clone(),
                dialog.clone(),
                command.clone(),
                room.clone(),
                stage.clone(),
                status.clone(),
                scroller.clone(),
            );
            glib::spawn_future_local(async move {
                let result = session
                    .submit_extension_command(
                        &command,
                        stage.session_id.clone(),
                        fields,
                        action,
                        room.clone(),
                    )
                    .await;
                if window.imp().generation.get() != generation {
                    dialog.close();
                    return;
                }
                scroller.set_sensitive(true);
                let Some(next) = result else {
                    status.set_label("The command could not be sent.");
                    status.set_visible(true);
                    return;
                };
                if action == WaddleAdhocAction::Cancel {
                    dialog.close();
                    return;
                }
                if next.status == WaddleAdhocStatus::Executing && next.form.is_some() {
                    fill_dialog(&window, &dialog, command, room, next);
                } else {
                    dialog.close();
                    window.toast(&outcome_text(&next));
                }
            });
        })
    };

    let buttons = gtk::Box::builder()
        .spacing(8)
        .halign(gtk::Align::End)
        .margin_top(8)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let cancel = gtk::Button::with_label("Cancel");
    let on_cancel = Rc::clone(&step);
    cancel.connect_clicked(move |_| on_cancel(WaddleAdhocAction::Cancel));
    buttons.append(&cancel);
    if stage.actions.contains(&WaddleAdhocAction::Prev) {
        let back = gtk::Button::with_label("Back");
        let on_back = Rc::clone(&step);
        back.connect_clicked(move |_| on_back(WaddleAdhocAction::Prev));
        buttons.append(&back);
    }
    let forward = extension_commands::forward_action(&stage.actions);
    let forward_button = gtk::Button::with_label(match forward {
        WaddleAdhocAction::Next => "Next",
        WaddleAdhocAction::Execute => "Run",
        _ => "Submit",
    });
    forward_button.add_css_class("suggested-action");
    forward_button.set_sensitive(!blocked);
    let on_forward = Rc::clone(&step);
    forward_button.connect_clicked(move |_| on_forward(forward));
    buttons.append(&forward_button);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .build();
    content.append(&scroller);
    content.append(&buttons);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(&content));
    dialog.set_child(Some(&view));
}
