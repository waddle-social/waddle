//! Collision resistance of qualified identities: equal local names or ids
//! under different issuers must stay distinct.

use std::collections::HashSet;

use jid::BareJid;
use waddle_client_session::{
    HistoryPage, IncomingMessage, QualifiedMessageId, RoomId, SpaceId, Timeline,
};

fn bare(value: &str) -> BareJid {
    BareJid::new(value).expect("test JID is valid")
}

#[test]
fn same_room_localpart_on_two_domains_is_distinct() {
    let a = RoomId::new(bare("general@muc.a.test"));
    let b = RoomId::new(bare("general@muc.b.test"));
    assert_ne!(a, b);
    assert_eq!(a, RoomId::new(bare("General@MUC.A.test")));
    let set: HashSet<_> = [a, b].into_iter().collect();
    assert_eq!(set.len(), 2);
}

#[test]
fn same_space_node_on_two_services_is_distinct() {
    let a = SpaceId::new(bare("spaces.a.test"), "main").expect("node");
    let b = SpaceId::new(bare("spaces.b.test"), "main").expect("node");
    assert_ne!(a, b);
    assert_eq!(a.node(), b.node());
    assert_ne!(a.service(), b.service());
}

#[test]
fn space_node_is_opaque_and_case_sensitive() {
    let a = SpaceId::new(bare("spaces.a.test"), "Main").expect("node");
    let b = SpaceId::new(bare("spaces.a.test"), "main").expect("node");
    assert_ne!(a, b);
    assert!(SpaceId::new(bare("spaces.a.test"), "").is_none());
}

#[test]
fn same_message_id_in_two_rooms_or_from_two_authorities_is_distinct() {
    let room_a = bare("general@muc.a.test");
    let room_b = bare("general@muc.b.test");
    let in_a = QualifiedMessageId::new(room_a.clone(), room_a.clone(), "1").expect("id");
    let in_b = QualifiedMessageId::new(room_b.clone(), room_b.clone(), "1").expect("id");
    let spoofed = QualifiedMessageId::new(room_a.clone(), room_b, "1").expect("id");
    assert_ne!(in_a, in_b);
    assert_ne!(in_a, spoofed);
    assert!(QualifiedMessageId::new(room_a.clone(), room_a, "").is_none());
}

fn archived(stanza: &str, origin: &str) -> IncomingMessage {
    IncomingMessage {
        stanza_id: Some(stanza.into()),
        origin_id: Some(origin.into()),
        reference_id: Some(stanza.into()),
        sender: "ann".into(),
        body: Some("hi".into()),
        timestamp_ms: 1,
        ..IncomingMessage::default()
    }
}

#[test]
fn timelines_qualify_the_same_stanza_id_per_room() {
    let room_a = bare("general@muc.a.test");
    let room_b = bare("general@muc.b.test");
    let mut a = Timeline::scoped(room_a.clone(), room_a);
    let mut b = Timeline::scoped(room_b.clone(), room_b);
    a.apply_history_page(vec![archived("1", "o1")], None, HistoryPage::Complete);
    b.apply_history_page(vec![archived("1", "o1")], None, HistoryPage::Complete);
    let id_a = a.qualified_id(&a.items()[0]).expect("stanza id");
    let id_b = b.qualified_id(&b.items()[0]).expect("stanza id");
    assert_eq!(id_a.id(), id_b.id());
    assert_ne!(id_a, id_b);
}

#[test]
fn unscoped_timeline_and_origin_only_rows_have_no_qualified_id() {
    let mut unscoped = Timeline::new();
    unscoped.apply_history_page(vec![archived("1", "o1")], None, HistoryPage::Complete);
    assert!(unscoped.qualified_id(&unscoped.items()[0]).is_none());

    let room = bare("general@muc.a.test");
    let mut scoped = Timeline::scoped(room.clone(), room);
    let mut origin_only = archived("1", "o1");
    origin_only.stanza_id = None;
    scoped.apply_history_page(vec![origin_only], None, HistoryPage::Complete);
    assert!(scoped.qualified_id(&scoped.items()[0]).is_none());
}
