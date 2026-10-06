//! Production serialization → durable custody → resume byte assertions (#1912).

use std::sync::Arc;
use std::time::Instant;

use chrono::{DateTime, TimeZone, Utc};
use jid::FullJid;
use minidom::Element;
use waddle_xmpp::parser::element_to_string;
use waddle_xmpp::stream_management::{
    stamp_replay_delay, DetachedSession, InMemorySmSessionRegistry, SmClaimCompletion,
    SmIngressAppendKey, SmIngressReceiptKind, SmKeyedAppendOutcome, SmSessionRegistry,
    StreamManagementState,
};
use waddle_xmpp::xep::xep0203::{build_delay_element, DelayInfo};
use waddle_xmpp::xep::NS_DELAY;
use waddle_xmpp::xep::{build_link_metadata_element, LinkMetadata};
use waddle_xmpp::Stanza;
use xmpp_parsers::message::{Id, Lang, Message, MessageType, Thread};

const EXTENSION_NS: &str = "urn:waddle:test:replay:0";
const DOMAIN: &str = "example.com";

pub struct ReplayBytesFixture {
    session: DetachedSession,
    stanzas: Vec<Stanza>,
    keys: Vec<SmIngressAppendKey>,
    wire: Vec<String>,
    receipt: DateTime<Utc>,
}

impl ReplayBytesFixture {
    pub fn new(stream: &str, jid: FullJid) -> Self {
        let receipt = Utc.with_ymd_and_hms(2026, 7, 1, 9, 15, 30).unwrap();
        let extension = Element::builder("metadata", EXTENSION_NS)
            .attr(minidom::rxml::xml_ncname!("label").to_owned(), "<&\"'>")
            .append("escaped <&> text; café 🦆")
            .append(
                Element::builder("nested", EXTENSION_NS)
                    .append("tail")
                    .build(),
            )
            .build();
        let mut message = Message::new(Some(jid.clone().into()));
        message.from = Some("bob@example.com/phone".parse().expect("sender JID"));
        message.id = Some(Id("replay-<&\"'>".to_owned()));
        message.type_ = MessageType::Chat;
        message
            .bodies
            .insert(Lang::new(), "escaped <&> text; café 🦆".into());
        message.thread = Some(Thread {
            id: "thread-<&>".into(),
            parent: Some("parent-<&>".into()),
        });
        message.payloads.push(extension.clone());
        message.payloads.push(build_link_metadata_element(
            &LinkMetadata::new("https://example.com/?a=1&b=2".parse().expect("URL"))
                .with_title("escaped <&> link")
                .with_description("namespaced RDF and OpenGraph payload"),
        ));
        let mut self_delayed = message.clone();
        self_delayed.payloads.push(build_delay_element(&DelayInfo {
            from: Some(DOMAIN.into()),
            stamp: receipt - chrono::Duration::days(1),
            reason: Some("Offline <&> storage".into()),
        }));
        let mut upstream_delayed = message.clone();
        upstream_delayed
            .payloads
            .push(build_delay_element(&DelayInfo {
                from: Some("upstream.example.org".into()),
                stamp: receipt - chrono::Duration::minutes(1),
                reason: Some("Upstream <&> delay".into()),
            }));
        let mut presence = xmpp_parsers::presence::Presence::available();
        presence.payloads.push(extension);
        let stanzas = vec![
            Stanza::Message(message),
            Stanza::Message(self_delayed),
            Stanza::Message(upstream_delayed),
            Stanza::Presence(presence),
            Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::empty_result(
                jid.clone().into(),
                "iq-<&>",
            ))),
        ];
        // Capture the production serializer's output BEFORE any persistence
        // parser can normalize it. Expected bytes are never read back from SQL.
        let wire = stanzas
            .iter()
            .map(|stanza| {
                element_to_string(&stanza.to_element()).expect("production serialization")
            })
            .collect();
        let keys = stanzas
            .iter()
            .map(|_| SmIngressAppendKey {
                message_key: waddle_xmpp::ingress::MessageKey::new(),
                kind: SmIngressReceiptKind::from_storage(
                    waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag(),
                ),
                semantic_identity_hash: [19; 32],
                resource: jid.clone(),
            })
            .collect();
        Self {
            session: DetachedSession {
                stream_id: stream.into(),
                user_id: jid.to_bare().to_string(),
                jid,
                occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
                inbound_count: 0,
                outbound_count: 0,
                last_acked: 0,
                replay_gap_through: None,
                unacked_stanzas: Vec::new(),
                max_resume_time: Some(300),
                detached_at: Instant::now(),
                carbons_enabled: false,
                roster_interested: false,
                blocklist_interested: false,
                presence_available: false,
                presence_show: None,
                presence_status: None,
                presence_priority: 0,
                presence_payloads: Vec::new(),
                pending_subscribes_flushed: false,
            },
            stanzas,
            keys,
            wire,
            receipt,
        }
    }

    pub async fn store_and_append(&self, registry: &Arc<InMemorySmSessionRegistry>) {
        registry
            .store_session(self.session.clone())
            .await
            .expect("persist detached session");
        for (index, (stanza, key)) in self.stanzas.iter().zip(&self.keys).enumerate() {
            assert!(matches!(
                registry
                    .record_keyed_outbound_for_detached_stream_at(
                        &self.session.stream_id,
                        u32::try_from(index + 1).unwrap(),
                        stanza,
                        self.receipt,
                        key.clone(),
                    )
                    .await
                    .expect("append production payload"),
                SmKeyedAppendOutcome::Appended { .. }
            ));
        }
        self.assert_replay(
            &registry
                .peek_session(&self.session.stream_id)
                .await
                .expect("read original queue")
                .expect("original session"),
        );
    }

    pub async fn retry_appends(&self, registry: &Arc<InMemorySmSessionRegistry>) {
        // The retried operation supplies a later receipt time. Durable custody
        // must preserve the first payload, counter, and timestamp instead.
        for (stanza, key) in self.stanzas.iter().zip(&self.keys) {
            assert!(matches!(
                registry
                    .record_keyed_outbound_for_detached_stream_at(
                        &self.session.stream_id,
                        99,
                        stanza,
                        self.receipt + chrono::Duration::hours(1),
                        key.clone(),
                    )
                    .await
                    .expect("retry committed append"),
                SmKeyedAppendOutcome::AlreadyAppended { .. }
            ));
        }
    }

    pub async fn complete_resume_and_detach(&self, registry: &Arc<InMemorySmSessionRegistry>) {
        let completion = registry
            .complete_claim_if_resumable(&self.session.stream_id, 0)
            .await
            .expect("complete resume")
            .expect("claimed session");
        let SmClaimCompletion::Resumed(mut session) = completion else {
            panic!("complete replay window must resume");
        };
        self.assert_replay(&session);
        // No client ACK was received before the resumed connection detached.
        // Its original unacked queue (not the newly stamped frames) is stored.
        session.detached_at = Instant::now();
        registry
            .store_session(session)
            .await
            .expect("detach resumed stream");
    }

    pub fn assert_replay(&self, session: &DetachedSession) {
        assert_eq!(session.outbound_count as usize, self.wire.len());
        assert_eq!(
            session.stanzas_to_resend(0),
            self.wire,
            "durable restoration must preserve the exact pre-delay serializer bytes"
        );
        let mut resumed = StreamManagementState::new();
        resumed.restore_from_session(session);
        let replay = resumed.get_stanzas_to_resend(0);
        assert_eq!(replay.len(), self.wire.len());
        for (index, entry) in replay.iter().enumerate() {
            assert_eq!(
                entry.original_receipt_at, self.receipt,
                "retry/resume must not refresh the original timestamp"
            );
            let expected = stamp_replay_delay(&self.wire[index], DOMAIN, self.receipt);
            let actual = stamp_replay_delay(&entry.stanza_xml, DOMAIN, entry.original_receipt_at);
            assert_eq!(
                actual, expected,
                "final replay frame bytes must survive storage"
            );
            assert_eq!(
                stamp_replay_delay(&actual, DOMAIN, self.receipt + chrono::Duration::days(1)),
                actual,
                "repeated replay must retain the original stamp and exact bytes"
            );
            if index == 4 {
                assert_eq!(
                    actual, self.wire[index],
                    "IQ replay must remain byte-identical"
                );
                continue;
            }
            let element: Element = actual.parse().expect("replay XML");
            let delays: Vec<_> = element
                .children()
                .filter(|child| child.is("delay", NS_DELAY))
                .collect();
            let own: Vec<_> = delays
                .iter()
                .filter(|delay| delay.attr("from") == Some(DOMAIN))
                .collect();
            assert_eq!(own.len(), 1);
            assert_eq!(
                own[0].attr("stamp"),
                Some(if index == 1 {
                    "2026-06-30T09:15:30Z"
                } else {
                    "2026-07-01T09:15:30Z"
                })
            );
            if index == 1 {
                assert_eq!(
                    actual, self.wire[index],
                    "an existing own delay forbids any byte changes"
                );
                assert_eq!(own[0].text(), "Offline <&> storage");
            }
            if index == 2 {
                assert_eq!(delays.len(), 2);
                let upstream = delays
                    .iter()
                    .find(|delay| delay.attr("from") == Some("upstream.example.org"))
                    .expect("upstream delay retained");
                assert_eq!(upstream.attr("stamp"), Some("2026-07-01T09:14:30Z"));
                assert_eq!(upstream.text(), "Upstream <&> delay");
            }
        }
    }
}
