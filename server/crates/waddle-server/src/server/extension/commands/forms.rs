use waddle_extensions::{
    CommandAction as ExtensionCommandAction, CommandSessionId, DataForm as ExtensionDataForm,
    DataFormField as ExtensionDataFormField, DataFormType as ExtensionDataFormType, DataFormValue,
    FormFieldOption, FormFieldType as ExtensionFormFieldType, FormFieldValue, UiActionId,
};

pub(crate) fn extension_command_fields(
    form: Option<&waddle_xmpp::xep::xep0004::DataForm>,
) -> Vec<FormFieldValue> {
    form.map(|form| {
        form.fields
            .iter()
            .filter_map(|field| {
                let name = UiActionId::new(field.var.clone()?).ok()?;
                let values = field
                    .values
                    .iter()
                    .map(|value| DataFormValue::new(value.clone()))
                    .collect();
                Some(FormFieldValue { name, values })
            })
            .collect()
    })
    .unwrap_or_default()
}

pub(crate) fn extension_data_form(
    form: &waddle_xmpp::xep::xep0004::DataForm,
) -> Option<ExtensionDataForm> {
    Some(ExtensionDataForm {
        form_type: extension_data_form_type(form.form_type),
        title: form
            .title
            .clone()
            .and_then(|title| waddle_extensions::DisplayText::new(title).ok()),
        instructions: form
            .instructions
            .iter()
            .filter_map(|instruction| waddle_extensions::DisplayText::new(instruction.clone()).ok())
            .collect(),
        fields: form
            .fields
            .iter()
            .filter_map(extension_data_form_field)
            .collect(),
    })
}

fn extension_data_form_type(
    form_type: waddle_xmpp::xep::xep0004::FormType,
) -> ExtensionDataFormType {
    match form_type {
        waddle_xmpp::xep::xep0004::FormType::Form => ExtensionDataFormType::Form,
        waddle_xmpp::xep::xep0004::FormType::Submit => ExtensionDataFormType::Submit,
        waddle_xmpp::xep::xep0004::FormType::Cancel => ExtensionDataFormType::Cancel,
        waddle_xmpp::xep::xep0004::FormType::Result => ExtensionDataFormType::Result,
    }
}

fn extension_data_form_field(
    field: &waddle_xmpp::xep::xep0004::Field,
) -> Option<ExtensionDataFormField> {
    Some(ExtensionDataFormField {
        name: UiActionId::new(field.var.clone()?).ok()?,
        field_type: extension_form_field_type(field.field_type),
        label: field
            .label
            .clone()
            .and_then(|label| waddle_extensions::DisplayText::new(label).ok()),
        required: field.required,
        values: field
            .values
            .iter()
            .map(|value| DataFormValue::new(value.clone()))
            .collect(),
        options: field
            .options
            .iter()
            .map(|option| FormFieldOption {
                label: option
                    .label
                    .clone()
                    .and_then(|label| waddle_extensions::DisplayText::new(label).ok()),
                value: DataFormValue::new(option.value.clone()),
            })
            .collect(),
    })
}

fn extension_form_field_type(
    field_type: waddle_xmpp::xep::xep0004::FieldType,
) -> ExtensionFormFieldType {
    match field_type {
        waddle_xmpp::xep::xep0004::FieldType::Boolean => ExtensionFormFieldType::Boolean,
        waddle_xmpp::xep::xep0004::FieldType::Fixed => ExtensionFormFieldType::Fixed,
        waddle_xmpp::xep::xep0004::FieldType::Hidden => ExtensionFormFieldType::Hidden,
        waddle_xmpp::xep::xep0004::FieldType::JidMulti => ExtensionFormFieldType::JidMulti,
        waddle_xmpp::xep::xep0004::FieldType::JidSingle => ExtensionFormFieldType::JidSingle,
        waddle_xmpp::xep::xep0004::FieldType::ListMulti => ExtensionFormFieldType::ListMulti,
        waddle_xmpp::xep::xep0004::FieldType::ListSingle => ExtensionFormFieldType::ListSingle,
        waddle_xmpp::xep::xep0004::FieldType::TextMulti => ExtensionFormFieldType::TextMulti,
        waddle_xmpp::xep::xep0004::FieldType::TextPrivate => ExtensionFormFieldType::TextPrivate,
        waddle_xmpp::xep::xep0004::FieldType::TextSingle => ExtensionFormFieldType::TextSingle,
    }
}

pub(crate) fn extension_session_id(session_id: Option<String>) -> Option<CommandSessionId> {
    session_id.and_then(|session_id| CommandSessionId::new(session_id).ok())
}

pub(crate) fn extension_command_action(
    action: waddle_xmpp::xep::xep0050::Action,
) -> ExtensionCommandAction {
    match action {
        waddle_xmpp::xep::xep0050::Action::Execute => ExtensionCommandAction::Execute,
        waddle_xmpp::xep::xep0050::Action::Next => ExtensionCommandAction::Next,
        waddle_xmpp::xep::xep0050::Action::Prev => ExtensionCommandAction::Prev,
        waddle_xmpp::xep::xep0050::Action::Complete => ExtensionCommandAction::Complete,
        waddle_xmpp::xep::xep0050::Action::Cancel => ExtensionCommandAction::Cancel,
    }
}

pub(crate) fn extension_data_form_to_xmpp(
    form: waddle_extensions::DataForm,
) -> waddle_xmpp::xep::xep0004::DataForm {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FieldOption, FieldType, FormType};

    let form_type = match form.form_type {
        ExtensionDataFormType::Form => FormType::Form,
        ExtensionDataFormType::Submit => FormType::Submit,
        ExtensionDataFormType::Cancel => FormType::Cancel,
        ExtensionDataFormType::Result => FormType::Result,
    };
    let mut out = DataForm::new(form_type);
    if let Some(title) = form.title {
        out = out.with_title(title.into_string());
    }
    for instruction in form.instructions {
        out = out.add_instructions(instruction.into_string());
    }
    for field in form.fields {
        let field_type = match field.field_type {
            ExtensionFormFieldType::Boolean => FieldType::Boolean,
            ExtensionFormFieldType::Fixed => FieldType::Fixed,
            ExtensionFormFieldType::Hidden => FieldType::Hidden,
            ExtensionFormFieldType::JidMulti => FieldType::JidMulti,
            ExtensionFormFieldType::JidSingle => FieldType::JidSingle,
            ExtensionFormFieldType::ListMulti => FieldType::ListMulti,
            ExtensionFormFieldType::ListSingle => FieldType::ListSingle,
            ExtensionFormFieldType::TextMulti => FieldType::TextMulti,
            ExtensionFormFieldType::TextPrivate => FieldType::TextPrivate,
            ExtensionFormFieldType::TextSingle => FieldType::TextSingle,
        };
        let mut xmpp_field = Field::new(field.name.into_string(), field_type);
        if let Some(label) = field.label {
            xmpp_field = xmpp_field.with_label(label.into_string());
        }
        if field.required {
            xmpp_field = xmpp_field.with_required();
        }
        for value in field.values {
            xmpp_field.values.push(value.into_string());
        }
        for option in field.options {
            let value = option.value.into_string();
            let xmpp_option = match option.label {
                Some(label) => FieldOption::with_label(label.into_string(), value),
                None => FieldOption::new(value),
            };
            xmpp_field = xmpp_field.add_option(xmpp_option);
        }
        out = out.add_field(xmpp_field);
    }
    out
}

pub(crate) fn extension_enrichment_texts(
    envelope: &waddle_extensions::ExtensionEnvelope,
) -> Vec<String> {
    envelope
        .enrichments
        .iter()
        .flat_map(|enrichment| enrichment.ui.iter())
        .flat_map(|view| view.blocks.iter())
        .filter_map(|block| {
            if let waddle_extensions::types::UiBlock::Text(text) = block {
                Some(text.text.as_str().to_string())
            } else {
                None
            }
        })
        .collect()
}

pub(crate) fn extension_enrichment_result_form(
    envelope: &waddle_extensions::ExtensionEnvelope,
) -> waddle_xmpp::xep::xep0004::DataForm {
    use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType};

    let mut form = DataForm::new(FormType::Result)
        .with_title("Extension result")
        .add_field(Field::form_type("urn:waddle:extension:1:result"));
    let Some(enrichment) = envelope.enrichments.first() else {
        return form;
    };
    form = form
        .add_field(Field::text_single("extension#id", enrichment.id.as_str()))
        .add_field(Field::text_single(
            "extension#plugin",
            enrichment.plugin.as_str(),
        ))
        .add_field(Field::text_single(
            "extension#title",
            enrichment
                .ui
                .first()
                .and_then(|view| view.title.as_ref())
                .map(|title| title.as_str())
                .unwrap_or_else(|| enrichment.plugin.as_str()),
        ))
        .add_field(Field::text_single(
            "extension#summary",
            enrichment.payload_namespace.as_str(),
        ))
        .add_field(Field::text_single(
            "launch-count",
            enrichment.launches.len().to_string(),
        ));
    for (view_index, view) in enrichment.ui.iter().enumerate() {
        for (block_index, block) in view.blocks.iter().enumerate() {
            if let waddle_extensions::types::UiBlock::Text(text) = block {
                form = form.add_field(Field::text_single(
                    format!("view#{view_index}#text#{block_index}"),
                    text.text.as_str(),
                ));
            }
        }
    }
    for (index, launch) in enrichment.launches.iter().enumerate() {
        let prefix = format!("launch#{index}");
        form = form
            .add_field(Field::text_single(
                format!("{prefix}#id"),
                launch.id.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#plugin"),
                launch.plugin.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#action"),
                launch.action.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#command-node"),
                launch.command_node.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#label"),
                launch.label.as_str(),
            ))
            .add_field(Field::text_single(
                format!("{prefix}#waddle-id"),
                launch.context.waddle_id.as_str(),
            ));
        if let Some(stanza_id) = &launch.context.source_stanza_id {
            form = form.add_field(Field::text_single(
                format!("{prefix}#source-stanza-id"),
                stanza_id.as_str(),
            ));
        }
        if let Some(token) = &launch.token {
            form = form.add_field(Field::text_single(
                format!("{prefix}#token"),
                token.as_str(),
            ));
        }
        if let Some(expires_at) = &launch.expires_at {
            form = form.add_field(Field::text_single(
                format!("{prefix}#expires-at"),
                expires_at.as_str(),
            ));
        }
        for (payload_index, payload) in launch.payloads.iter().enumerate() {
            let payload_prefix = format!("{prefix}#payload#{payload_index}");
            form = form
                .add_field(Field::text_single(
                    format!("{payload_prefix}#namespace"),
                    payload.namespace.as_str(),
                ))
                .add_field(Field::text_single(
                    format!("{payload_prefix}#name"),
                    payload.root.local_name.as_str(),
                ));
            for child in &payload.root.children {
                if let waddle_extensions::XmlNode::Text(text) = child {
                    form = form.add_field(Field::text_single(
                        format!("{payload_prefix}#text"),
                        text.as_str(),
                    ));
                }
            }
            for attribute in &payload.root.attributes {
                if attribute.local_name == "xmlns" {
                    continue;
                }
                form = form.add_field(Field::text_single(
                    format!("{payload_prefix}#attr#{}", attribute.local_name),
                    attribute.value.as_str(),
                ));
            }
        }
    }
    form
}
