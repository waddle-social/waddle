use super::*;

#[test]
fn manifest_declares_decision_poll_commands_as_channel_scoped() {
    let manifest = manifest();
    assert_eq!(manifest.commands.len(), 2);
    for command in &manifest.commands {
        assert!(
            matches!(command.scope, types::CommandScope::Channel),
            "command {} should require an active channel context",
            command.node.value,
        );
    }
}

#[test]
fn poll_options_accept_multiple_xep0004_values() {
    let options = poll_options(&[form_field("options", &["Ship it", "Revise it", "Block it"])])
        .expect("valid options");

    assert_eq!(
        option_labels(&options),
        vec!["Ship it", "Revise it", "Block it"]
    );
}

#[test]
fn poll_options_accept_newline_delimited_value() {
    let options = poll_options(&[form_field("options", &["Ship it\nRevise it\n\nBlock it"])])
        .expect("valid options");

    assert_eq!(
        option_labels(&options),
        vec!["Ship it", "Revise it", "Block it"]
    );
}

fn form_field(name: &str, values: &[&str]) -> types::FormFieldValue {
    types::FormFieldValue {
        name: types::UiActionId {
            value: name.to_string(),
        },
        values: values
            .iter()
            .map(|value| types::DataFormValue {
                value: value.to_string(),
            })
            .collect(),
    }
}

fn option_labels(options: &[PollOption]) -> Vec<&str> {
    options.iter().map(|option| option.label.as_str()).collect()
}
