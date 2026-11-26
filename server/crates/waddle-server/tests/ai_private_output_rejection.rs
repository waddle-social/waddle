//! T2-M6-01: Room `/ai` rejects `output=private` at the server command
//! boundary before any extension inference or reply executes, while permitting
//! `output=channel` (and omitted output defaulting to channel).

use waddle_extensions::{DataFormValue, FormFieldValue, RoomJid as ExtensionRoomJid, UiActionId};
use waddle_server::server::validate_room_ai_output_mode;

fn field(name: &str, value: &str) -> FormFieldValue {
    FormFieldValue {
        name: UiActionId::try_new(name).expect("field name"),
        values: vec![DataFormValue::new(value)],
    }
}

#[test]
fn room_ai_rejects_private_output_mode_and_allows_channel_mode() {
    let ai_node = "urn:waddle:extension:1:ai-chatbot";
    let room = ExtensionRoomJid::try_new("general@muc.example.com").expect("room");

    // `output=private` in a room is unconditionally rejected before execution.
    let private_err = validate_room_ai_output_mode(
        ai_node,
        Some(&room),
        &[field("prompt", "summarize"), field("output", "private")],
    )
    .expect_err("room /ai must reject output=private");
    assert!(
        private_err.contains("private output mode is forbidden"),
        "unexpected error message: {private_err}"
    );

    // Case-insensitive & unknown non-channel modes in a room are also rejected.
    validate_room_ai_output_mode(ai_node, Some(&room), &[field("output", "PRIVATE")])
        .expect_err("room /ai must reject output=PRIVATE");
    validate_room_ai_output_mode(ai_node, Some(&room), &[field("output", "dm")])
        .expect_err("room /ai must reject non-channel output modes");

    // `output=channel` and omitted `output` are accepted inside a room.
    validate_room_ai_output_mode(
        ai_node,
        Some(&room),
        &[field("prompt", "summarize"), field("output", "channel")],
    )
    .expect("room /ai with output=channel must succeed");
    validate_room_ai_output_mode(ai_node, Some(&room), &[field("prompt", "summarize")])
        .expect("room /ai without explicit output field must succeed");

    // Non-room command invocation (`room: None`) and unrelated command nodes are not blocked here.
    validate_room_ai_output_mode(ai_node, None, &[field("output", "private")])
        .expect("non-room invocation is unaffected by room output check");
    validate_room_ai_output_mode(
        "urn:waddle:extension:1:other",
        Some(&room),
        &[field("output", "private")],
    )
    .expect("non-ai command is unaffected");
}
