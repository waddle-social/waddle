//! Server extension commands (XEP-0050) in the composer, and the XEP-0004
//! forms they answer with.
//!
//! The server advertises commands with an optional slash word, an
//! optional field the text after the word fills, and whether the bare
//! word runs at once. This module decides what typing `/word text` does
//! and how a form's values are checked and submitted, mirroring the
//! Apple client; the dialog that shows a form is `ui/commands.rs`.

use waddle_xmpp_client_ffi::{
    WaddleAdhocAction, WaddleAdhocStatus, WaddleExtensionCommand, WaddleExtensionCommandForm,
    WaddleExtensionCommandFormField, WaddleExtensionCommandResult, WaddleExtensionCommandScope,
    WaddleExtensionFieldType, WaddleExtensionFormField,
};

use crate::composer;

/// The lower-cased slash word of a command, when it has one.
pub fn keyword(command: &WaddleExtensionCommand) -> Option<String> {
    let word = command
        .composer_prefix
        .as_deref()?
        .trim()
        .trim_start_matches('/');
    (!word.is_empty() && !word.contains(char::is_whitespace)).then(|| word.to_lowercase())
}

/// Commands the composer may offer here: with a slash word no built-in
/// command already holds, and, for a room-only command, in a room.
pub fn eligible(
    commands: &[WaddleExtensionCommand],
    in_room: bool,
) -> Vec<(String, &WaddleExtensionCommand)> {
    commands
        .iter()
        .filter(|command| in_room || command.scope != WaddleExtensionCommandScope::Channel)
        .filter_map(|command| Some((keyword(command)?, command)))
        .filter(|(word, _)| !composer::is_builtin(word))
        .collect()
}

/// The command an exactly typed `/word` names, and the text after it.
/// Only a word exactly one command answers to resolves, so an ambiguous
/// one is sent as text rather than guessed.
pub fn resolve<'a>(
    text: &str,
    commands: &'a [WaddleExtensionCommand],
    in_room: bool,
) -> Option<(&'a WaddleExtensionCommand, String)> {
    let rest = text.strip_prefix('/')?;
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (word, trailing) = rest.split_at(end);
    let word = word.to_lowercase();
    let mut matches = eligible(commands, in_room)
        .into_iter()
        .filter(|(keyword, _)| *keyword == word);
    let (_, command) = matches.next()?;
    matches
        .next()
        .is_none()
        .then(|| (command, trailing.trim().to_string()))
}

/// How a typed command runs, given the text after its word.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Submit the form with `field` set to `value`.
    InlineSubmit { field: String, value: String },
    /// Execute without showing a form.
    Execute,
    /// Show the form, filling its first required field with the text.
    OpenForm { prefill: Option<String> },
}

pub fn invocation(command: &WaddleExtensionCommand, trailing: &str) -> Invocation {
    let value = trailing.trim();
    match (&command.inline_field, value.is_empty()) {
        (Some(field), false) => Invocation::InlineSubmit {
            field: field.clone(),
            value: value.to_string(),
        },
        (_, true) if command.composer_execute => Invocation::Execute,
        (None, false) => Invocation::OpenForm {
            prefill: Some(value.to_string()),
        },
        _ => Invocation::OpenForm { prefill: None },
    }
}

/// What to do once an inline command has been started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlinePlan {
    /// Nothing more to ask: show the result.
    Finished(WaddleExtensionCommandResult),
    /// The command needs the form (several stages, a blocked or other
    /// required field); the form is filled with the typed value if it
    /// has the field.
    NeedsInput(WaddleExtensionCommandResult),
    /// Complete the single-stage form as it now stands.
    Submit(WaddleExtensionCommandResult),
}

pub fn plan_inline(invoked: WaddleExtensionCommandResult, field: &str, value: &str) -> InlinePlan {
    let plain = |result: WaddleExtensionCommandResult| {
        if result.status == WaddleAdhocStatus::Executing {
            InlinePlan::NeedsInput(result)
        } else {
            InlinePlan::Finished(result)
        }
    };
    let Some(form) = invoked
        .form
        .as_ref()
        .filter(|form| form.fields.iter().any(|f| f.var == field))
        .filter(|_| invoked.status == WaddleAdhocStatus::Executing)
    else {
        return plain(invoked);
    };
    let filled = setting(form, field, vec![value.to_string()]);
    let completes = invoked.actions.contains(&WaddleAdhocAction::Complete)
        && blocked_field(&filled).is_none()
        && missing_required(&filled).is_empty();
    let stage = WaddleExtensionCommandResult {
        form: Some(filled),
        ..invoked
    };
    if completes {
        InlinePlan::Submit(stage)
    } else {
        InlinePlan::NeedsInput(stage)
    }
}

// ---------------------------------------------------------------------
// Forms.

/// Why a form cannot be submitted forward.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmitError {
    /// A field the client must never fill (a secret) is present.
    Forbidden(String),
    Missing(Vec<String>),
}

/// The first field that forbids submitting forward.
pub fn blocked_field(
    form: &WaddleExtensionCommandForm,
) -> Option<&WaddleExtensionCommandFormField> {
    form.fields.iter().find(|field| field.blocked)
}

fn is_filled(value: &str) -> bool {
    !value.trim().is_empty()
}

/// XEP-0004 `<required/>` is met. `fixed` fields carry no input.
fn has_required_value(field: &WaddleExtensionCommandFormField) -> bool {
    match field.field_type {
        WaddleExtensionFieldType::Fixed => true,
        WaddleExtensionFieldType::JidMulti
        | WaddleExtensionFieldType::ListMulti
        | WaddleExtensionFieldType::TextMulti => field.values.iter().any(|v| is_filled(v)),
        _ => field.values.first().is_some_and(|v| is_filled(v)),
    }
}

/// Visible required fields that still lack a value.
pub fn missing_required(
    form: &WaddleExtensionCommandForm,
) -> Vec<&WaddleExtensionCommandFormField> {
    form.fields
        .iter()
        .filter(|f| {
            f.field_type != WaddleExtensionFieldType::Hidden && f.required && !has_required_value(f)
        })
        .collect()
}

/// The form with `var` set. Blocked and `fixed` fields are never filled.
pub fn setting(
    form: &WaddleExtensionCommandForm,
    var: &str,
    values: Vec<String>,
) -> WaddleExtensionCommandForm {
    let mut form = form.clone();
    for field in &mut form.fields {
        if field.var == var && field.field_type != WaddleExtensionFieldType::Fixed && !field.blocked
        {
            field.values = values.clone();
        }
    }
    form
}

/// The form with `value` in its first required, empty text or JID field,
/// so text typed after a slash word is kept.
pub fn prefill_first_required(
    form: &WaddleExtensionCommandForm,
    value: &str,
) -> WaddleExtensionCommandForm {
    let target = form.fields.iter().find(|field| {
        field.required
            && !field.blocked
            && !has_required_value(field)
            && matches!(
                field.field_type,
                WaddleExtensionFieldType::TextSingle
                    | WaddleExtensionFieldType::TextMulti
                    | WaddleExtensionFieldType::JidSingle
                    | WaddleExtensionFieldType::JidMulti
            )
    });
    match target {
        Some(field) => setting(form, &field.var.clone(), vec![value.to_string()]),
        None => form.clone(),
    }
}

/// Whether `action` carries the form: `cancel` and `prev` never do.
pub fn submits_form(action: WaddleAdhocAction) -> bool {
    !matches!(action, WaddleAdhocAction::Cancel | WaddleAdhocAction::Prev)
}

/// The values `action` submits. A forward action is refused while a
/// blocked field is present or a required field is empty.
pub fn submission(
    form: Option<&WaddleExtensionCommandForm>,
    action: WaddleAdhocAction,
) -> Result<Vec<WaddleExtensionFormField>, SubmitError> {
    let Some(form) = form.filter(|_| submits_form(action)) else {
        return Ok(Vec::new());
    };
    if let Some(blocked) = blocked_field(form) {
        return Err(SubmitError::Forbidden(blocked.var.clone()));
    }
    let missing = missing_required(form);
    if !missing.is_empty() {
        return Err(SubmitError::Missing(
            missing.iter().map(|f| f.var.clone()).collect(),
        ));
    }
    Ok(form
        .fields
        .iter()
        .filter_map(|field| {
            let values = match field.field_type {
                WaddleExtensionFieldType::Fixed => return None,
                WaddleExtensionFieldType::JidMulti => field
                    .values
                    .iter()
                    .filter(|v| is_filled(v))
                    .cloned()
                    .collect(),
                WaddleExtensionFieldType::Hidden
                | WaddleExtensionFieldType::ListMulti
                | WaddleExtensionFieldType::TextMulti => field.values.clone(),
                _ => field.values.iter().take(1).cloned().collect(),
            };
            Some(WaddleExtensionFormField {
                var: field.var.clone(),
                values,
            })
        })
        .collect())
}

/// The forward action a stage's button takes: `complete` when allowed,
/// else `next`, else `execute`.
pub fn forward_action(actions: &[WaddleAdhocAction]) -> WaddleAdhocAction {
    [
        WaddleAdhocAction::Complete,
        WaddleAdhocAction::Next,
        WaddleAdhocAction::Execute,
    ]
    .into_iter()
    .find(|action| actions.contains(action))
    .unwrap_or(WaddleAdhocAction::Complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp_client_ffi::{WaddleExtensionCommandNote, WaddleExtensionFieldOption};

    fn command(prefix: Option<&str>, scope: WaddleExtensionCommandScope) -> WaddleExtensionCommand {
        WaddleExtensionCommand {
            service_jid: "ext.example.com".into(),
            node: "poll".into(),
            name: "Poll".into(),
            scope,
            composer_prefix: prefix.map(Into::into),
            inline_field: None,
            composer_execute: false,
        }
    }

    fn field(
        var: &str,
        field_type: WaddleExtensionFieldType,
        required: bool,
    ) -> WaddleExtensionCommandFormField {
        WaddleExtensionCommandFormField {
            var: var.into(),
            label: None,
            field_type,
            required,
            blocked: false,
            options: Vec::<WaddleExtensionFieldOption>::new(),
            values: Vec::new(),
        }
    }

    fn form(fields: Vec<WaddleExtensionCommandFormField>) -> WaddleExtensionCommandForm {
        WaddleExtensionCommandForm {
            title: None,
            instructions: None,
            fields,
        }
    }

    fn stage(
        form: Option<WaddleExtensionCommandForm>,
        actions: Vec<WaddleAdhocAction>,
    ) -> WaddleExtensionCommandResult {
        WaddleExtensionCommandResult {
            status: WaddleAdhocStatus::Executing,
            session_id: Some("s1".into()),
            actions,
            form,
            notes: Vec::<WaddleExtensionCommandNote>::new(),
        }
    }

    #[test]
    fn built_ins_shadow_commands_and_room_commands_need_a_room() {
        let commands = [
            command(Some("poll"), WaddleExtensionCommandScope::Channel),
            command(Some("/Away"), WaddleExtensionCommandScope::Global),
            command(Some("remind"), WaddleExtensionCommandScope::Global),
            command(None, WaddleExtensionCommandScope::Global),
            command(Some(""), WaddleExtensionCommandScope::Global),
        ];
        let words = |in_room| -> Vec<String> {
            eligible(&commands, in_room)
                .into_iter()
                .map(|c| c.0)
                .collect()
        };
        assert_eq!(words(true), ["poll", "remind"]);
        assert_eq!(words(false), ["remind"]);
    }

    #[test]
    fn exact_words_resolve_when_unambiguous() {
        let commands = [
            command(Some("poll"), WaddleExtensionCommandScope::Global),
            command(Some("Poll"), WaddleExtensionCommandScope::Global),
            command(Some("remind"), WaddleExtensionCommandScope::Global),
        ];
        assert!(resolve("/poll x", &commands, true).is_none(), "ambiguous");
        let (found, trailing) = resolve("/REMIND  in 5  ", &commands, false).expect("resolves");
        assert_eq!(found.composer_prefix.as_deref(), Some("remind"));
        assert_eq!(trailing, "in 5");
        assert!(resolve("/rem", &commands, false).is_none());
        assert!(resolve("/me hi", &commands, false).is_none());
        assert!(resolve("remind", &commands, false).is_none());
    }

    #[test]
    fn trailing_text_picks_how_a_command_runs() {
        let mut cmd = command(Some("poll"), WaddleExtensionCommandScope::Global);
        assert_eq!(invocation(&cmd, ""), Invocation::OpenForm { prefill: None });
        assert_eq!(
            invocation(&cmd, " lunch? "),
            Invocation::OpenForm {
                prefill: Some("lunch?".into())
            }
        );
        cmd.inline_field = Some("question".into());
        assert_eq!(
            invocation(&cmd, "lunch?"),
            Invocation::InlineSubmit {
                field: "question".into(),
                value: "lunch?".into()
            }
        );
        assert_eq!(
            invocation(&cmd, "  "),
            Invocation::OpenForm { prefill: None }
        );
        cmd.composer_execute = true;
        assert_eq!(invocation(&cmd, ""), Invocation::Execute);
    }

    #[test]
    fn required_fields_gate_forward_submission_only() {
        let mut secret = field("token", WaddleExtensionFieldType::TextPrivate, false);
        secret.blocked = true;
        let open = form(vec![
            field("q", WaddleExtensionFieldType::TextSingle, true),
            field("note", WaddleExtensionFieldType::Fixed, true),
        ]);
        assert_eq!(
            submission(Some(&open), WaddleAdhocAction::Complete),
            Err(SubmitError::Missing(vec!["q".into()]))
        );
        assert_eq!(
            submission(Some(&open), WaddleAdhocAction::Cancel),
            Ok(Vec::new())
        );
        assert_eq!(
            submission(Some(&open), WaddleAdhocAction::Prev),
            Ok(Vec::new())
        );
        let filled = setting(&open, "q", vec!["lunch?".into()]);
        assert_eq!(
            submission(Some(&filled), WaddleAdhocAction::Complete),
            Ok(vec![WaddleExtensionFormField {
                var: "q".into(),
                values: vec!["lunch?".into()]
            }])
        );
        let blocked = form(vec![secret]);
        assert_eq!(
            submission(Some(&blocked), WaddleAdhocAction::Next),
            Err(SubmitError::Forbidden("token".into()))
        );
        // A blocked field is never filled.
        assert!(setting(&blocked, "token", vec!["x".into()]).fields[0]
            .values
            .is_empty());
    }

    #[test]
    fn submitted_values_follow_the_field_type() {
        let mut jids = field("who", WaddleExtensionFieldType::JidMulti, false);
        jids.values = vec!["a@x".into(), "  ".into(), "b@x".into()];
        let mut single = field("one", WaddleExtensionFieldType::ListSingle, false);
        single.values = vec!["a".into(), "b".into()];
        let mut hidden = field("h", WaddleExtensionFieldType::Hidden, false);
        hidden.values = vec!["keep".into()];
        let fixed = field("", WaddleExtensionFieldType::Fixed, false);
        let sent = submission(
            Some(&form(vec![jids, single, hidden, fixed])),
            WaddleAdhocAction::Complete,
        )
        .expect("submits");
        let values: Vec<_> = sent
            .iter()
            .map(|f| (f.var.as_str(), f.values.len()))
            .collect();
        assert_eq!(values, [("who", 2), ("one", 1), ("h", 1)]);
    }

    #[test]
    fn typed_text_fills_the_first_empty_required_text_field() {
        let mut done = field("a", WaddleExtensionFieldType::TextSingle, true);
        done.values = vec!["x".into()];
        let f = form(vec![
            done,
            field("pw", WaddleExtensionFieldType::TextPrivate, true),
            field("opt", WaddleExtensionFieldType::TextSingle, false),
            field("b", WaddleExtensionFieldType::JidSingle, true),
        ]);
        let filled = prefill_first_required(&f, "me@x");
        assert_eq!(filled.fields[3].values, ["me@x"]);
        assert!(filled.fields[1].values.is_empty() && filled.fields[2].values.is_empty());
        let none = form(vec![field("l", WaddleExtensionFieldType::ListSingle, true)]);
        assert_eq!(prefill_first_required(&none, "v"), none);
    }

    #[test]
    fn a_single_stage_form_completes_inline() {
        let f = form(vec![field("q", WaddleExtensionFieldType::TextSingle, true)]);
        let invoked = stage(Some(f), vec![WaddleAdhocAction::Complete]);
        match plan_inline(invoked, "q", "lunch?") {
            InlinePlan::Submit(stage) => {
                assert_eq!(stage.form.expect("form").fields[0].values, ["lunch?"]);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn other_forms_open_prefilled_or_finish() {
        // Another required field is empty.
        let f = form(vec![
            field("q", WaddleExtensionFieldType::TextSingle, true),
            field("when", WaddleExtensionFieldType::TextSingle, true),
        ]);
        match plan_inline(
            stage(Some(f.clone()), vec![WaddleAdhocAction::Complete]),
            "q",
            "v",
        ) {
            InlinePlan::NeedsInput(stage) => {
                assert_eq!(stage.form.expect("form").fields[0].values, ["v"]);
            }
            other => panic!("{other:?}"),
        }
        // More stages follow: no complete.
        let one = form(vec![field("q", WaddleExtensionFieldType::TextSingle, true)]);
        assert!(matches!(
            plan_inline(stage(Some(one), vec![WaddleAdhocAction::Next]), "q", "v"),
            InlinePlan::NeedsInput(_)
        ));
        // The field is not in the form, or there is no form.
        assert!(matches!(
            plan_inline(
                stage(Some(f), vec![WaddleAdhocAction::Complete]),
                "zzz",
                "v"
            ),
            InlinePlan::NeedsInput(_)
        ));
        let mut done = stage(None, Vec::new());
        done.status = WaddleAdhocStatus::Completed;
        assert!(matches!(
            plan_inline(done, "q", "v"),
            InlinePlan::Finished(_)
        ));
    }

    #[test]
    fn the_forward_button_prefers_complete_then_next() {
        use WaddleAdhocAction::{Cancel, Complete, Execute, Next};
        assert_eq!(forward_action(&[Cancel, Next, Complete]), Complete);
        assert_eq!(forward_action(&[Cancel, Next]), Next);
        assert_eq!(forward_action(&[Execute]), Execute);
        assert_eq!(forward_action(&[]), Complete);
    }
}
