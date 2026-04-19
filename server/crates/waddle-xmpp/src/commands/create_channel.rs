//! Create Channel Command Handler (XEP-0050)

use crate::commands::{CommandContext, CommandResult, NoteType};
use crate::xep::xep0004::{DataForm, Field, FieldOption, FieldType, FormType};
use crate::xep::xep0050::Note;
use crate::ChannelType;
use std::sync::Arc;
use tracing::debug;

pub const NODE_CREATE_CHANNEL: &str = "waddle:create-channel";

#[derive(Debug)]
pub struct ChannelMetadata {
    pub name: String,
    pub description: Option<String>,
    pub channel_type: String,
    pub position: i32,
}

pub struct CreateChannelResult {
    pub channel_id: String,
    pub channel_jid: String,
}

pub trait CreateChannelDeps: Send + Sync {
    fn create_channel(
        &self,
        user_id: &str,
        waddle_id: &str,
        metadata: ChannelMetadata,
    ) -> impl std::future::Future<Output = Result<CreateChannelResult, String>> + Send;
}

fn parse_channel_form(form: &DataForm) -> Result<(String, ChannelMetadata), String> {
    let mut waddle_id: Option<String> = None;
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    let mut channel_type: Option<String> = None;
    let mut position: Option<i32> = None;

    for field in &form.fields {
        let var = match field.var.as_deref() {
            Some(v) => v,
            None => continue,
        };

        match var {
            "waddle_id" => waddle_id = field.value().map(|v| v.to_string()),
            "name" => name = field.value().map(|v| v.to_string()),
            "description" => description = field.value().map(|v| v.to_string()),
            "channel_type" => channel_type = field.value().map(|v| v.to_string()),
            "position" => position = field.value().and_then(|v| v.parse().ok()),
            _ => {}
        }
    }

    let waddle_id = waddle_id.ok_or_else(|| "Missing required field: waddle_id".to_string())?;
    let name = name.ok_or_else(|| "Missing required field: name".to_string())?;
    let channel_type = channel_type.unwrap_or_else(|| "text".to_string());
    let position = position.unwrap_or(0);

    if name.trim().is_empty() {
        return Err("Channel name cannot be empty".to_string());
    }

    let channel_type = ChannelType::parse(&channel_type)
        .map(|kind| kind.as_str().to_string())
        .ok_or_else(|| {
            format!(
                "Unsupported channel_type '{}'. Expected one of: text, forum",
                channel_type
            )
        })?;

    Ok((
        waddle_id,
        ChannelMetadata {
            name,
            description,
            channel_type,
            position,
        },
    ))
}

fn build_request_form() -> DataForm {
    DataForm::new(FormType::Form)
        .add_field(Field::hidden("waddle_id", "").with_label("Waddle ID (required)"))
        .add_field(
            Field::text_single("name", "")
                .with_label("Channel Name")
                .with_required(),
        )
        .add_field(
            Field::new("description", FieldType::TextMulti)
                .with_label("Channel Description")
                .with_desc("Optional description for the channel"),
        )
        .add_field(
            Field::new("channel_type", FieldType::ListSingle)
                .with_label("Channel Type")
                .with_value("text")
                .add_option(FieldOption::with_label("Text Channel", "text"))
                .add_option(FieldOption::with_label("Forum Channel", "forum")),
        )
        .add_field(
            Field::text_single("position", "0")
                .with_label("Position")
                .with_desc("Display position in the channel list (default: 0)"),
        )
}

fn build_result_form(channel_id: &str, channel_jid: &str) -> DataForm {
    DataForm::new(FormType::Result)
        .add_field(Field::hidden("channel_id", channel_id))
        .add_field(Field::hidden("channel_jid", channel_jid))
}

pub async fn handle_create_channel<D: CreateChannelDeps>(
    deps: Arc<D>,
    ctx: CommandContext,
) -> CommandResult {
    let from = &ctx.from;
    let action = ctx
        .command
        .action
        .unwrap_or(crate::xep::xep0050::Action::Execute);

    match action {
        crate::xep::xep0050::Action::Execute => {
            debug!(from = %from, "create-channel: execute action");
            let form = build_request_form();
            let session_id = ctx.command.session_id.clone().unwrap_or_default();
            CommandResult::Executing {
                form,
                session_id,
                notes: vec![Note::new(
                    NoteType::Info,
                    "Fill in the channel details and submit",
                )],
            }
        }
        crate::xep::xep0050::Action::Complete => {
            debug!(from = %from, "create-channel: complete action");
            let form = match &ctx.command.form {
                Some(f) => f,
                None => {
                    return CommandResult::Error(crate::XmppError::bad_request(Some(
                        "Form submission required".to_string(),
                    )));
                }
            };

            let (waddle_id, metadata) = match parse_channel_form(form) {
                Ok(parsed) => parsed,
                Err(err) => {
                    return CommandResult::Completed {
                        form: None,
                        notes: vec![Note::new(NoteType::Error, &err)],
                    };
                }
            };

            let user_id = match ctx.authenticated_user_id.as_deref() {
                Some(user_id) => user_id,
                None => {
                    return CommandResult::Error(crate::XmppError::not_authorized(Some(
                        "Authenticated session required".to_string(),
                    )));
                }
            };

            match deps.create_channel(user_id, &waddle_id, metadata).await {
                Ok(result) => {
                    let result_form = build_result_form(&result.channel_id, &result.channel_jid);
                    CommandResult::Completed {
                        form: Some(result_form),
                        notes: vec![Note::new(
                            NoteType::Info,
                            format!("Channel created successfully: {}", result.channel_jid),
                        )],
                    }
                }
                Err(err) => CommandResult::Completed {
                    form: None,
                    notes: vec![Note::new(NoteType::Error, &err)],
                },
            }
        }
        _ => CommandResult::Error(crate::XmppError::bad_request(Some(
            "Unsupported action for create-channel command".to_string(),
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::Action;
    use crate::xep::xep0050::Command;
    use jid::Jid;
    use minidom::Element;
    use std::sync::Mutex;
    use xmpp_parsers::iq::{Iq, IqType};

    #[derive(Default)]
    struct MockDeps {
        captured_user_id: Mutex<Option<String>>,
    }

    impl CreateChannelDeps for MockDeps {
        async fn create_channel(
            &self,
            user_id: &str,
            _waddle_id: &str,
            _metadata: ChannelMetadata,
        ) -> Result<CreateChannelResult, String> {
            *self.captured_user_id.lock().unwrap() = Some(user_id.to_string());
            Ok(CreateChannelResult {
                channel_id: "channel-123".to_string(),
                channel_jid: "test-waddle_channel-123@muc.localhost".to_string(),
            })
        }
    }

    fn test_command_context(
        action: Action,
        session_id: Option<&str>,
        form: Option<DataForm>,
        authenticated_user_id: Option<&str>,
    ) -> CommandContext {
        CommandContext {
            from: "alice@localhost/resource".parse::<Jid>().unwrap(),
            authenticated_user_id: authenticated_user_id.map(str::to_string),
            iq: Iq {
                from: None,
                to: None,
                id: "test-iq".to_string(),
                payload: IqType::Set("<query xmlns='urn:test'/>".parse::<Element>().unwrap()),
            },
            command: {
                let mut command = Command::new(NODE_CREATE_CHANNEL).with_action(action);
                command.session_id = session_id.map(str::to_string);
                command.form = form;
                command
            },
        }
    }

    #[test]
    fn test_parse_channel_form_minimal() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::hidden("waddle_id", "test-waddle"))
            .add_field(Field::text_single("name", "General"));

        let (waddle_id, metadata) = parse_channel_form(&form).unwrap();
        assert_eq!(waddle_id, "test-waddle");
        assert_eq!(metadata.name, "General");
        assert_eq!(metadata.channel_type, "text");
        assert_eq!(metadata.position, 0);
    }

    #[test]
    fn test_parse_channel_form_full() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::hidden("waddle_id", "test-waddle"))
            .add_field(Field::text_single("name", "Announcements"))
            .add_field(
                Field::new("description", FieldType::TextMulti).with_value("Important updates"),
            )
            .add_field(Field::new("channel_type", FieldType::ListSingle).with_value("forum"))
            .add_field(Field::text_single("position", "10"));

        let (waddle_id, metadata) = parse_channel_form(&form).unwrap();
        assert_eq!(waddle_id, "test-waddle");
        assert_eq!(metadata.name, "Announcements");
        assert_eq!(metadata.description, Some("Important updates".to_string()));
        assert_eq!(metadata.channel_type, "forum");
        assert_eq!(metadata.position, 10);
    }

    #[test]
    fn test_parse_channel_form_invalid_type() {
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::hidden("waddle_id", "test-waddle"))
            .add_field(Field::text_single("name", "General"))
            .add_field(Field::new("channel_type", FieldType::ListSingle).with_value("invalid"));

        let result = parse_channel_form(&form);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Unsupported channel_type"));
    }

    #[test]
    fn test_parse_channel_form_missing_name() {
        let form =
            DataForm::new(FormType::Submit).add_field(Field::hidden("waddle_id", "test-waddle"));

        let result = parse_channel_form(&form);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Missing required field: name"));
    }

    #[tokio::test]
    async fn test_execute_returns_session_id_from_context() {
        let result = handle_create_channel(
            Arc::new(MockDeps::default()),
            test_command_context(Action::Execute, Some("session-123"), None, Some("user-123")),
        )
        .await;

        match result {
            CommandResult::Executing { session_id, .. } => assert_eq!(session_id, "session-123"),
            _ => panic!("expected executing result"),
        }
    }

    #[tokio::test]
    async fn test_complete_uses_authenticated_user_id_for_create_channel() {
        let deps = Arc::new(MockDeps::default());
        let form = DataForm::new(FormType::Submit)
            .add_field(Field::hidden("waddle_id", "test-waddle"))
            .add_field(Field::text_single("name", "General"));

        let result = handle_create_channel(
            Arc::clone(&deps),
            test_command_context(
                Action::Complete,
                Some("session-123"),
                Some(form),
                Some("user-123"),
            ),
        )
        .await;

        match result {
            CommandResult::Completed { .. } => {}
            _ => panic!("expected completed result"),
        }

        assert_eq!(
            deps.captured_user_id.lock().unwrap().as_deref(),
            Some("user-123")
        );
    }
}
