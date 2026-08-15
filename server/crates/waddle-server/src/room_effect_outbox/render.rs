use jid::{BareJid, FullJid};
use waddle_xmpp::muc::{
    build_ban_presence, build_config_change_message, build_destroy_notification,
    build_kick_presence, build_membership_removal_presence, build_role_change_presence,
    AdminPresenceKind, DestroyRequest, MucPresenceStatus, OccupantPresenceUpdate,
    OccupantVoiceChange, RoomEffect,
};
use waddle_xmpp::xep::xep0421::{OccupantIdSecret, OccupantIdentity};
use waddle_xmpp::Stanza;

const STATUS_AFFILIATION_CHANGE_REMOVAL: &str = "321";
const STATUS_MEMBERS_ONLY_CONFIG_REMOVAL: &str = "322";

pub fn rebuild_effect(
    room_jid: &BareJid,
    effect: &RoomEffect,
    occupant_id_secret: &OccupantIdSecret,
) -> Vec<(FullJid, Stanza)> {
    match effect {
        RoomEffect::ConfigChanged {
            status_codes,
            recipients,
        } => recipients
            .iter()
            .cloned()
            .map(|recipient| {
                let message = build_config_change_message(room_jid, &recipient, status_codes);
                (recipient, Stanza::Message(message))
            })
            .collect(),
        RoomEffect::AdminSelfNotify { updates } => updates
            .iter()
            .map(|update| rebuild_admin_update(update, occupant_id_secret))
            .collect(),
        RoomEffect::AdminRemainingBroadcast {
            presence_updates, ..
        } => presence_updates
            .iter()
            .map(|update| rebuild_admin_update(update, occupant_id_secret))
            .collect(),
        RoomEffect::DestroyNotification {
            reason,
            alternate_venue,
            password,
            recipients,
        } => {
            let request = DestroyRequest {
                reason: reason.as_ref().map(|value| value.as_str().to_owned()),
                alternate_venue: alternate_venue.clone(),
                password: password.as_ref().map(|value| value.as_str().to_owned()),
            };
            let mut stanzas = Vec::new();
            for recipient in recipients {
                for session in &recipient.sessions {
                    let session_bare = session.to_bare();
                    let identity = OccupantIdentity {
                        bare_jid: &session_bare,
                        real_jid: Some(session),
                        secret: occupant_id_secret,
                    };
                    let presence = build_destroy_notification(
                        room_jid,
                        recipient.nick.as_str(),
                        session,
                        &request,
                        true,
                        &identity,
                    );
                    stanzas.push((session.clone(), Stanza::Presence(presence)));
                }
            }
            stanzas
        }
    }
}

pub fn effect_voice_changes(effect: &RoomEffect) -> &[OccupantVoiceChange] {
    match effect {
        RoomEffect::AdminRemainingBroadcast { voice_changes, .. } => voice_changes,
        _ => &[],
    }
}

fn rebuild_admin_update(
    update: &OccupantPresenceUpdate,
    occupant_id_secret: &OccupantIdSecret,
) -> (FullJid, Stanza) {
    let occupant_identity = OccupantIdentity {
        bare_jid: &update.occupant_bare_jid,
        real_jid: update.disclosed_real_jid.as_ref(),
        secret: occupant_id_secret,
    };
    let reason = update.reason.as_ref().map(|value| value.as_str());
    let actor = update.actor.as_ref();
    let presence = match update.kind {
        AdminPresenceKind::Banned => build_ban_presence(
            &update.occupant,
            &update.recipient,
            MucPresenceStatus::new(update.is_self, false),
            reason,
            actor,
            &occupant_identity,
        ),
        AdminPresenceKind::Kicked => build_kick_presence(
            &update.occupant,
            &update.recipient,
            update.affiliation,
            MucPresenceStatus::new(update.is_self, false),
            reason,
            actor,
            &occupant_identity,
        ),
        AdminPresenceKind::AffiliationRemoved => build_membership_removal_presence(
            &update.occupant,
            &update.recipient,
            STATUS_AFFILIATION_CHANGE_REMOVAL,
            MucPresenceStatus::new(update.is_self, false),
            actor,
            &occupant_identity,
        ),
        AdminPresenceKind::MembersOnlyRemoved => build_membership_removal_presence(
            &update.occupant,
            &update.recipient,
            STATUS_MEMBERS_ONLY_CONFIG_REMOVAL,
            MucPresenceStatus::new(update.is_self, false),
            actor,
            &occupant_identity,
        ),
        AdminPresenceKind::RoleChanged(role) => build_role_change_presence(
            &update.occupant,
            &update.recipient,
            update.affiliation,
            role,
            MucPresenceStatus::new(update.is_self, false),
            &occupant_identity,
        ),
    };
    (update.recipient.clone(), Stanza::Presence(presence))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;
    use waddle_xmpp::muc::{
        DestroyPassword, DestroyReason, DestroyRecipient, MucConfigStatusCode, MucOccupantNick,
    };
    use waddle_xmpp::{Affiliation, Role, Voice};

    fn occupant_id_secret() -> OccupantIdSecret {
        OccupantIdSecret::new(b"room-effect-render-test-secret-is-32b".as_slice().to_vec())
            .expect("test occupant-id secret")
    }

    fn bare_jid(value: &str) -> BareJid {
        BareJid::from_str(value).expect("bare JID")
    }

    fn full_jid(value: &str) -> FullJid {
        FullJid::from_str(value).expect("full JID")
    }

    fn room_jid() -> BareJid {
        bare_jid("room@conference.example.test")
    }

    fn occupant_jid() -> FullJid {
        full_jid("room@conference.example.test/alice")
    }

    fn real_jid() -> FullJid {
        full_jid("alice@example.test/phone")
    }

    fn peer_jid() -> FullJid {
        full_jid("bob@example.test/web")
    }

    fn actor_jid() -> jid::BareJid {
        bare_jid("mod@example.test")
    }

    fn xml(stanza: &Stanza) -> String {
        crate::server::routes::websocket::stanza_to_xml(stanza)
    }

    #[test]
    fn rebuild_config_change_reuses_wire_builder() {
        let effect = RoomEffect::ConfigChanged {
            status_codes: vec![
                MucConfigStatusCode::LoggingEnabled,
                MucConfigStatusCode::NonPrivacyConfigurationChange,
            ],
            recipients: vec![real_jid(), peer_jid()],
        };

        let rendered = rebuild_effect(&room_jid(), &effect, &occupant_id_secret());

        let expected = [
            (
                real_jid(),
                Stanza::Message(build_config_change_message(
                    &room_jid(),
                    &real_jid(),
                    &[
                        MucConfigStatusCode::LoggingEnabled,
                        MucConfigStatusCode::NonPrivacyConfigurationChange,
                    ],
                )),
            ),
            (
                peer_jid(),
                Stanza::Message(build_config_change_message(
                    &room_jid(),
                    &peer_jid(),
                    &[
                        MucConfigStatusCode::LoggingEnabled,
                        MucConfigStatusCode::NonPrivacyConfigurationChange,
                    ],
                )),
            ),
        ];

        assert_eq!(rendered.len(), expected.len());
        for ((recipient, actual), (expected_recipient, expected_stanza)) in
            rendered.iter().zip(expected.iter())
        {
            assert_eq!(recipient, expected_recipient);
            assert_eq!(xml(actual), xml(expected_stanza));
        }
    }

    #[test]
    fn rebuild_admin_effects_reuse_presence_builders() {
        let secret = occupant_id_secret();
        let alice_bare = bare_jid("alice@example.test");
        let alice_real = real_jid();
        let self_update = OccupantPresenceUpdate {
            recipient: alice_real.clone(),
            is_self: true,
            occupant: occupant_jid(),
            nick: MucOccupantNick::new("alice".to_owned()).expect("nick"),
            occupant_bare_jid: alice_bare.clone(),
            disclosed_real_jid: Some(alice_real.clone()),
            affiliation: Affiliation::Member,
            kind: AdminPresenceKind::Kicked,
            actor: Some(actor_jid()),
            reason: Some(DestroyReason::new("cleanup".to_owned()).expect("reason")),
        };
        let broadcast_update = OccupantPresenceUpdate {
            recipient: peer_jid(),
            is_self: false,
            occupant: occupant_jid(),
            nick: MucOccupantNick::new("alice".to_owned()).expect("nick"),
            occupant_bare_jid: alice_bare.clone(),
            // Disclosure is per recipient: this peer must not inherit the
            // removed occupant's real JID from the self-notification.
            disclosed_real_jid: None,
            affiliation: Affiliation::Member,
            kind: AdminPresenceKind::RoleChanged(Role::Participant),
            actor: None,
            reason: None,
        };
        let effect = RoomEffect::AdminRemainingBroadcast {
            presence_updates: vec![self_update.clone(), broadcast_update.clone()],
            voice_changes: vec![waddle_xmpp::muc::OccupantVoiceChange {
                session: peer_jid(),
                voice: Voice::Muted,
            }],
        };

        let rendered = rebuild_effect(&room_jid(), &effect, &secret);

        let expected_identity = OccupantIdentity {
            bare_jid: &alice_bare,
            real_jid: Some(&alice_real),
            secret: &secret,
        };
        let expected_peer_identity = OccupantIdentity {
            bare_jid: &alice_bare,
            real_jid: None,
            secret: &secret,
        };
        let expected = [
            (
                alice_real.clone(),
                Stanza::Presence(build_kick_presence(
                    &occupant_jid(),
                    &alice_real,
                    Affiliation::Member,
                    MucPresenceStatus::new(true, false),
                    Some("cleanup"),
                    Some(&actor_jid()),
                    &expected_identity,
                )),
            ),
            (
                peer_jid(),
                Stanza::Presence(build_role_change_presence(
                    &occupant_jid(),
                    &peer_jid(),
                    Affiliation::Member,
                    Role::Participant,
                    MucPresenceStatus::new(false, false),
                    &expected_peer_identity,
                )),
            ),
        ];

        assert_eq!(rendered.len(), expected.len());
        for ((recipient, actual), (expected_recipient, expected_stanza)) in
            rendered.iter().zip(expected.iter())
        {
            assert_eq!(recipient, expected_recipient);
            assert_eq!(xml(actual), xml(expected_stanza));
        }
    }

    #[test]
    fn rebuild_destroy_notification_reuses_wire_builder() {
        let secret = occupant_id_secret();
        let alice_bare = bare_jid("alice@example.test");
        let alice_real = real_jid();
        let effect = RoomEffect::DestroyNotification {
            reason: Some(DestroyReason::new("closed".to_owned()).expect("reason")),
            alternate_venue: Some(bare_jid("newroom@conference.example.test")),
            password: Some(DestroyPassword::new("secret".to_owned()).expect("password")),
            recipients: vec![DestroyRecipient {
                nick: MucOccupantNick::new("alice".to_owned()).expect("nick"),
                sessions: vec![alice_real.clone()],
            }],
        };

        let rendered = rebuild_effect(&room_jid(), &effect, &secret);

        let expected_identity = OccupantIdentity {
            bare_jid: &alice_bare,
            real_jid: Some(&alice_real),
            secret: &secret,
        };
        let expected = Stanza::Presence(build_destroy_notification(
            &room_jid(),
            "alice",
            &alice_real,
            &DestroyRequest {
                reason: Some("closed".to_owned()),
                alternate_venue: Some(bare_jid("newroom@conference.example.test")),
                password: Some("secret".to_owned()),
            },
            true,
            &expected_identity,
        ));

        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].0, alice_real);
        assert_eq!(xml(&rendered[0].1), xml(&expected));
    }

    #[test]
    fn voice_changes_are_exposed_separately_from_rendered_stanzas() {
        let voice_changes = vec![OccupantVoiceChange {
            session: peer_jid(),
            voice: Voice::Muted,
        }];
        let effect = RoomEffect::AdminRemainingBroadcast {
            presence_updates: Vec::new(),
            voice_changes: voice_changes.clone(),
        };

        assert_eq!(effect_voice_changes(&effect), voice_changes.as_slice());
        assert!(effect_voice_changes(&RoomEffect::ConfigChanged {
            status_codes: Vec::new(),
            recipients: Vec::new(),
        })
        .is_empty());
    }

    #[test]
    fn rebuild_admin_update_uses_recipient_scoped_identity_snapshot() {
        let effect = RoomEffect::AdminSelfNotify {
            updates: vec![OccupantPresenceUpdate {
                recipient: real_jid(),
                is_self: true,
                occupant: occupant_jid(),
                nick: MucOccupantNick::new("alice".to_owned()).expect("nick"),
                occupant_bare_jid: bare_jid("alice@example.test"),
                disclosed_real_jid: Some(real_jid()),
                affiliation: Affiliation::Member,
                kind: AdminPresenceKind::Banned,
                actor: None,
                reason: None,
            }],
        };

        let rendered = rebuild_effect(&room_jid(), &effect, &occupant_id_secret());
        assert_eq!(rendered.len(), 1);
    }

    #[test]
    fn rebuild_admin_update_uses_producer_is_self_for_same_bare_sibling_session() {
        let sibling_bare = bare_jid("alice@example.test");
        let sibling_recipient = full_jid("alice@example.test/laptop");
        let effect = RoomEffect::AdminRemainingBroadcast {
            presence_updates: vec![OccupantPresenceUpdate {
                recipient: sibling_recipient.clone(),
                is_self: false,
                occupant: occupant_jid(),
                nick: MucOccupantNick::new("alice".to_owned()).expect("nick"),
                occupant_bare_jid: sibling_bare,
                disclosed_real_jid: None,
                affiliation: Affiliation::Member,
                kind: AdminPresenceKind::MembersOnlyRemoved,
                actor: None,
                reason: None,
            }],
            voice_changes: Vec::new(),
        };

        let rendered = rebuild_effect(&room_jid(), &effect, &occupant_id_secret());
        let frame = xml(&rendered[0].1);

        assert!(
            frame.contains("code=\"322\"") || frame.contains("code='322'"),
            "expected members-only removal status: {frame}"
        );
        assert!(
            !frame.contains("code=\"110\"") && !frame.contains("code='110'"),
            "sibling session under the same bare JID must not be stamped as self: {frame}"
        );
    }
}
