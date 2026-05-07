use super::*;

pub(super) fn handle_command(
    command: types::CommandInvocation,
) -> Result<Vec<types::ExtensionEffect>, types::ExtensionError> {
    if command.command_node.value != COMMAND_NODE {
        return Ok(vec![]);
    }
    if matches!(command.action, Some(types::CommandAction::Cancel)) {
        return Ok(vec![]);
    }
    if !matches!(
        command.action,
        Some(types::CommandAction::Complete) | Some(types::CommandAction::Next)
    ) || field_value(&command.fields, "question").is_none()
    {
        return Ok(vec![
            types::ExtensionEffect::CommandForm(create_poll_form()),
        ]);
    }

    let Some(room) = command.room.clone() else {
        return Ok(vec![types::ExtensionEffect::HostWarning(display(
            "Decision polls require an active channel.",
        ))]);
    };
    let question = required_field(&command.fields, "question")?;
    let options = poll_options(&command.fields)?;
    let duration = duration_seconds(
        &field_value(&command.fields, "duration").unwrap_or_else(|| "1h".to_string()),
    )?;
    let poll_id = command
        .session_id
        .as_ref()
        .map(|id| id.value.clone())
        .unwrap_or_else(|| "poll".to_string());
    let closes_at = closes_at(duration);
    let poll = Poll {
        poll_id,
        question,
        options,
        closes_at,
        room,
        waddle_id: command.waddle_id,
    };

    send_poll_message(&poll)?;
    Ok(vec![types::ExtensionEffect::PublishPubsub(
        types::PubsubPublish {
            node: polls_node(&poll.room),
            item_id: Some(types::PubsubItemId {
                value: poll.poll_id.clone(),
            }),
            payload: poll_extension_item(&poll),
        },
    )])
}

pub(super) fn handle_vote(launch: types::LaunchInvocation) -> Vec<types::ExtensionEffect> {
    let Some(room) = launch.context.room.clone() else {
        return vec![types::ExtensionEffect::HostWarning(display(
            "Poll votes require a channel context.",
        ))];
    };
    let poll_id = field_value(&launch.fields, "payload#vote-request#poll-id")
        .unwrap_or_else(|| "poll".to_string());
    let option_id = field_value(&launch.fields, "payload#vote-request#option-id")
        .unwrap_or_else(|| launch.launch_id.value.clone());
    let voter = stable_id(bare_jid_value(&launch.requester.value));
    vec![
        types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
            node: votes_node(&room, &poll_id),
            item_id: Some(types::PubsubItemId { value: voter }),
            payload: vote_extension_item(&poll_id, &option_id),
        }),
        types::ExtensionEffect::PublishPubsub(types::PubsubPublish {
            node: results_node(&room),
            item_id: Some(types::PubsubItemId {
                value: poll_id.clone(),
            }),
            payload: results_extension_item(&poll_id, &option_id),
        }),
    ]
}
