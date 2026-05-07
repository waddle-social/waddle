use super::*;

pub(super) async fn project_groupchat_inbox_event(
    deps: &Deps<'_>,
    owner: BareJid,
    room: BareJid,
    message: Box<Message>,
    is_recipient: bool,
    thread: Option<GroupchatThreadProjection>,
    dispatch_timestamp: i64,
) {
    let Some(inbox_storage) = deps.inbox_storage else {
        debug!(
            owner = %owner,
            room = %room,
            "ProjectGroupchatInbox: no inbox_storage in Deps; skipping (test fixture?)"
        );
        return;
    };
    project_groupchat_inbox(
        inbox_storage,
        deps.connection_registry,
        &owner,
        &room,
        &message,
        is_recipient,
        &thread,
        dispatch_timestamp,
    )
    .await;
}
