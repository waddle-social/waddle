//! Walks the manifest: gates each row on its slices and the peer's
//! availability, runs its driver, records the outcome and restores the
//! standard configuration before the next row.

use anyhow::Result;

use crate::evidence::{self, EvidenceLog, Outcome, Record, Section};
use crate::manifest::{self, Drive, Peer, Row, Slice};
use crate::scenarios::harness::{Component, Harness};
use crate::scenarios::negative_cert::Bad;
use crate::scenarios::{
    muc, negative_addr, negative_admit, negative_cert, negative_dialback, negative_route, observe,
    restart, social, spaces, transport,
};
use crate::topology::Home;

/// Which rows a run includes: every row, or those whose id starts with
/// one of the given prefixes.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub prefixes: Vec<String>,
}

impl Filter {
    pub fn includes(&self, row: &Row) -> bool {
        self.prefixes.is_empty()
            || self
                .prefixes
                .iter()
                .any(|prefix| row.id.starts_with(prefix.as_str()))
    }
}

fn missing_slices(row: &Row, peer: Peer) -> Vec<Slice> {
    let mut slices: Vec<Slice> = row.requires.to_vec();
    if peer == Peer::Waddle {
        slices.extend_from_slice(row.waddle_also_requires);
    }
    slices.sort();
    slices.dedup();
    slices
        .into_iter()
        .filter(|slice| !manifest::landed(*slice))
        .collect()
}

async fn dispatch(h: &mut Harness, id: &str, peer: Peer) -> Result<Outcome> {
    match id {
        "s2s-off.no-listener-dial-read-advertisement" => restart::s2s_disabled(h, peer).await,
        "flow.s2s-tls-external" => transport::tls_external(h, peer).await,
        "flow.dialback" => transport::dialback(h, peer).await,
        "flow.presence" => social::presence(h, peer).await,
        "flow.direct-messages" => social::direct_messages(h, peer).await,
        "flow.local-joins-remote-muc" => muc::local_joins_remote(h, peer).await,
        "flow.remote-joins-waddle-muc" => muc::remote_joins_waddle(h, peer).await,
        "flow.two-resources-remote-room" => muc::two_resources(h, peer).await,
        "flow.bookmarks-restore" => spaces::bookmarks_restore(h, peer).await,
        "flow.remote-moderator-trusted-home" => muc::remote_moderator(h, peer).await,
        "flow.community-admin-trusted-home" => spaces::community_admin(h, peer).await,
        "flow.disco-ping" => transport::disco_ping(h, peer).await,
        "flow.space-on-peer-pubsub" => spaces::space_on_peer_pubsub(h, peer).await,
        "flow.chord-reads-waddle-space" => spaces::chord_reads_waddle_space(h).await,
        "flow.ejabberd-receives-enrolled-space-deletion" => {
            spaces::ejabberd_receives_enrolled_space_deletion(h).await
        }
        "flow.waddle-reads-chord-space" => spaces::waddle_reads_chord_space(h).await,
        "flow.peer-reads-waddle-space" => spaces::peer_reads_waddle_space(h, peer).await,
        "flow.child-rooms-by-muc-jid" => spaces::child_rooms(h, peer).await,
        "flow.no-avatar-banner-fetch" => spaces::no_avatar_fetch(h, peer).await,
        "obs.pre-tls-from" => observe::pre_tls_from(h, peer).await,
        "obs.whitespace-before-header" => observe::whitespace(h, peer).await,
        "obs.ejabberd-starttls" => observe::ejabberd_starttls(h, peer).await,
        "obs.mixed-case-to" => observe::mixed_case_to(h, peer).await,
        "obs.ejabberd-cluster-streams" => observe::ejabberd_cluster_streams(h, peer).await,
        "neg.cert.wrong-name" => negative_cert::bad_certificate(h, peer, Bad::WrongName).await,
        "neg.cert.unknown-issuer" => {
            negative_cert::bad_certificate(h, peer, Bad::UnknownIssuer).await
        }
        "neg.cert.expired" => negative_cert::bad_certificate(h, peer, Bad::Expired).await,
        "neg.cert.parent-domain-for-subdomain" => negative_cert::parent_domain(h, peer).await,
        "neg.cert.server-auth-only-both-roles" => negative_cert::server_auth_only(h, peer).await,
        "neg.cert.private-ca-for-public-profile" => {
            negative_cert::private_ca_for_public_profile(h, peer).await
        }
        "neg.cert.tls-version-outside-profile" => {
            negative_cert::tls_version_outside_profile(h, peer).await
        }
        "neg.cert.no-pre-tls-from" => negative_cert::no_pre_tls_from(h, peer).await,
        "neg.cert.inbound-direct-tls" => negative_cert::inbound_direct_tls(h, peer).await,
        "neg.dialback.unlisted-domain" => negative_dialback::unlisted_domain(h, peer).await,
        "neg.dialback.after-certificate-failure" => {
            negative_dialback::after_certificate_failure(h, peer).await
        }
        "neg.dialback.unsolicited" => negative_dialback::unsolicited(h, peer).await,
        "neg.dialback.extra-domains" => negative_dialback::extra_domains(h, peer).await,
        "neg.dialback.stock-authority-certificate" => {
            transport::stock_dialback_wrong_authority(h, peer).await
        }
        "neg.dialback.verify-over-plaintext" => {
            negative_dialback::verify_over_plaintext(h, peer).await
        }
        "neg.addr.header-refused-forms" => negative_addr::header_forms(h, peer).await,
        "neg.addr.outbound-destination" => negative_addr::outbound_destinations(h, peer).await,
        "neg.addr.stanza-address" => negative_addr::stanza_addresses(h, peer).await,
        "neg.addr.see-other-host" => negative_addr::see_other_host(h, peer).await,
        "neg.route.spoofed-from" => negative_route::spoofed_from(h, peer).await,
        "neg.route.forged-iq-result" => negative_route::forged_iq_result(h, peer).await,
        "neg.route.replayed-iq-result" => negative_route::replayed_iq_result(h, peer).await,
        "neg.route.relay-unserved-domain" => negative_route::relay_unserved(h, peer).await,
        "neg.route.no-error-to-error" => negative_route::no_error_to_error(h, peer).await,
        "neg.route.account-handler-refused" => {
            negative_route::account_handler_refused(h, peer).await
        }
        "neg.d10.server-domain-services" => negative_route::d10_services(h, peer).await,
        "neg.admit.denylisted-with-open-stream" => {
            negative_admit::denylisted_with_open_stream(h, peer).await
        }
        "neg.admit.denylisted-subdomain" => negative_admit::denylisted_subdomain(h, peer).await,
        "neg.admit.denylisted-spellings" => negative_admit::denylisted_spellings(h, peer).await,
        "neg.admit.denylisted-each-mode" => negative_admit::denylisted_each_mode(h, peer).await,
        "neg.admit.denylisted-listed-fails-to-load" => {
            negative_admit::listed_fails_to_load(h, peer).await
        }
        "neg.admit.listed-subdomain-not-covered" => negative_admit::listed_subdomain(h, peer).await,
        "neg.admit.removal-and-closed-mode" => negative_admit::removal_and_closed(h, peer).await,
        "neg.admit.open-mode-slot-exhaustion" => {
            negative_admit::open_mode_exhaustion(h, peer).await
        }
        "neg.authority.privileged-pkix-vs-dialback" => {
            muc::privileged_pkix_vs_dialback(h, peer).await
        }
        "neg.authority.moderation-dialback-only" => muc::moderation_dialback_only(h, peer).await,
        "neg.authority.untrusted-home" => muc::untrusted_home(h, peer).await,
        "neg.authority.home-removed-from-trusted" => muc::home_removed(h, peer).await,
        "neg.authority.own-edit-delete" => muc::own_edit_delete(h, peer).await,
        "neg.authority.private-discovery-and-mam" => muc::private_discovery_and_mam(h, peer).await,
        "neg.identity.same-room-names" => muc::same_room_names(h, peer).await,
        "neg.identity.foreign-reassignment" => muc::foreign_reassignment(h, peer).await,
        "neg.res.oversized-stanza" => negative_route::oversized(h, peer).await,
        "neg.res.deep-nesting" => negative_route::deep_nesting(h, peer).await,
        "neg.res.preauth-large-element" => negative_route::preauth_large_element(h, peer).await,
        "neg.res.peer-queue-exhaustion" => negative_route::queue_exhaustion(h, peer).await,
        "restart.each-side" => restart::each_side(h, peer).await,
        "restart.partition" => restart::partition(h, peer).await,
        "restart.crash" => restart::crash(h, peer).await,
        other => Ok(Outcome::not_driven(
            ["no driver is registered for ", other].concat(),
        )),
    }
}

/// Run one row against one peer.
pub async fn run_row(h: &mut Harness, row: &Row, peer: Peer) -> Outcome {
    let missing = missing_slices(row, peer);
    if !missing.is_empty() {
        let names: Vec<String> = missing
            .iter()
            .map(|slice| [slice.name(), " (", slice.describe(), ")"].concat())
            .collect();
        return Outcome::not_driven(["requires ", &names.join(", "), ", not landed"].concat());
    }
    if let Drive::NotDriven(reason) = row.drive {
        return Outcome::not_driven(reason);
    }
    if let Err(reason) = h.available(peer) {
        return Outcome::not_driven(reason);
    }
    let driven_by = ["driver for ", row.id].concat();
    let result = dispatch(h, row.id, peer).await;
    let outcome = evidence::settle(result, &driven_by);
    if let Err(error) = h.restore_standard().await {
        let mut outcome = outcome;
        outcome.note(format!(
            "restoring the standard configuration failed: {error:#}"
        ));
        return outcome;
    }
    outcome
}

/// Run every included row of `sections`.
pub async fn run(h: &mut Harness, log: &mut EvidenceLog, filter: &Filter, sections: &[Section]) {
    for row in manifest::ROWS {
        if !sections.contains(&row.section) || !filter.includes(row) {
            continue;
        }
        // The child-room row checks MUC fallback. Start with fresh streams
        // so previous flows cannot consume its connection capacity.
        let fresh_streams = row.id == "flow.child-rooms-by-muc-jid";
        let setup = if fresh_streams {
            h.dirty.extend([
                Component::Waddle(Home::A),
                Component::Waddle(Home::B),
                Component::Prosody,
                Component::Ejabberd,
            ]);
            h.restore_standard().await
        } else {
            Ok(())
        };
        for peer in row.peers {
            let mut outcome = match &setup {
                Ok(()) => run_row(h, row, *peer).await,
                Err(error) => Outcome::not_driven(format!("fresh stream setup failed: {error:#}")),
            };
            if fresh_streams && setup.is_ok() {
                outcome.note("Servers restarted before this flow group to clear earlier streams.");
            }
            log.record(Record {
                id: row.id.to_owned(),
                section: row.section,
                title: row.title.to_owned(),
                peer: peer.label().to_owned(),
                g20: row.g20 && *peer != Peer::Waddle,
                outcome,
            });
        }
    }
}

/// Record every included row of `sections` as UNVERIFIED with `reason`,
/// for a run that could not start its environment.
pub fn record_all_unverified(
    log: &mut EvidenceLog,
    filter: &Filter,
    sections: &[Section],
    reason: &str,
) {
    for row in manifest::ROWS {
        if !sections.contains(&row.section) || !filter.includes(row) {
            continue;
        }
        for peer in row.peers {
            log.record(Record {
                id: row.id.to_owned(),
                section: row.section,
                title: row.title.to_owned(),
                peer: peer.label().to_owned(),
                g20: row.g20 && *peer != Peer::Waddle,
                outcome: Outcome::not_driven(reason),
            });
        }
    }
}

/// Record every included row of `sections` that has no record yet as
/// UNVERIFIED with `reason`, for a run that stopped part way.
pub fn record_unrun(log: &mut EvidenceLog, filter: &Filter, sections: &[Section], reason: &str) {
    for row in manifest::ROWS {
        if !sections.contains(&row.section) || !filter.includes(row) {
            continue;
        }
        for peer in row.peers {
            if log.has(row.id, peer.label()) {
                continue;
            }
            log.record(Record {
                id: row.id.to_owned(),
                section: row.section,
                title: row.title.to_owned(),
                peer: peer.label().to_owned(),
                g20: row.g20 && *peer != Peer::Waddle,
                outcome: Outcome::not_driven(reason),
            });
        }
    }
}
