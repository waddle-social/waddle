//! The router's half of Spaces over S2S (ADR-0020 D10; XEP-0503 over
//! XEP-0060): requests remote principals address to the Spaces PubSub
//! service, handed to the Spaces host within the bounds of their domain and
//! their account, and the event notifications of local Spaces for
//! subscribers at other servers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use jid::{BareJid, Jid};
use waddle_xmpp::identity::CanonicalDomain;
use waddle_xmpp::Stanza;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

use super::super::local_spaces::{SpaceReply, SpaceRequest, MAX_SPACE_REPLIES};
use super::super::principal::RemotePrincipal;
use super::super::reach::SpaceOperation;
use super::super::replies;
use super::super::room_delivery::{RoomWork, SpaceTraffic};
use super::super::telemetry;
use super::hosting::{bucket, spend_refusal};
use super::{lock, Allowance, Inner, Pair, Purpose, Queued};
use crate::s2s::conn::{SinkVerdict, VerifiedPeerStream};

/// Remote domains whose allowance of requests for the Spaces service is
/// kept. A domain that finds the table full of domains still spending
/// theirs is refused, so the table never grows with the domains that ask.
const DOMAIN_ALLOWANCES: usize = 4096;

/// Remote accounts whose share of their domain's allowance is kept, on the
/// same terms.
const ACCOUNT_ALLOWANCES: usize = 16_384;

/// What remote domains and their accounts may still ask of the Spaces
/// service.
#[derive(Debug, Default)]
pub(super) struct SpaceAllowances {
    domains: HashMap<String, Allowance>,
    accounts: HashMap<BareJid, Allowance>,
}

/// What became of one request's claim on the allowances.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spend {
    /// One request of the domain's allowance and of the account's share
    /// was taken.
    Taken,
    /// An allowance is spent: the request is not taken, and is answered
    /// with an error only when `answered`.
    Spent { answered: bool },
}

/// The places event notifications of local Spaces hold on each pair's
/// queue.
pub(super) type EventShares = Arc<Mutex<HashMap<Pair, usize>>>;

/// One place of a pair's share of event notifications, given back when the
/// notification holding it is sent or dropped.
#[derive(Debug)]
pub(super) struct EventShare {
    shares: EventShares,
    pair: Pair,
}

impl Drop for EventShare {
    fn drop(&mut self) {
        super::decrement(&mut lock(&self.shares), &self.pair);
    }
}

impl Inner {
    /// Take one request of `peer`'s allowance for the Spaces service and
    /// of `account`'s share of it. The account's share is decided first and
    /// spends nothing of the domain's, so one account cannot spend what the
    /// other accounts of its domain may ask. A request beyond either is
    /// answered within the domain's allowance of answered refusals and
    /// dropped beyond that; it never holds its stream.
    fn take_space_allowance(&self, peer: &str, account: &BareJid) -> Spend {
        let rate = f64::from(self.limits.space_request_rate.max(1));
        let burst = f64::from(self.limits.space_request_burst.max(1));
        let account_rate = f64::from(self.limits.space_request_rate_per_account.max(1)).min(rate);
        let account_burst =
            f64::from(self.limits.space_request_burst_per_account.max(1)).min(burst);
        let now = Instant::now();
        let peer = peer.to_owned();
        let mut allowances = lock(&self.space_allowances);
        let SpaceAllowances { domains, accounts } = &mut *allowances;
        let share = bucket(
            accounts,
            account,
            account_rate,
            account_burst,
            now,
            ACCOUNT_ALLOWANCES,
        );
        let Some(share) = share.filter(|share| share.tokens >= 1.0) else {
            let answered =
                spend_refusal(bucket(domains, &peer, rate, burst, now, DOMAIN_ALLOWANCES));
            return Spend::Spent { answered };
        };
        match bucket(domains, &peer, rate, burst, now, DOMAIN_ALLOWANCES) {
            Some(domain) if domain.tokens >= 1.0 => {
                domain.tokens -= 1.0;
                share.tokens -= 1.0;
                Spend::Taken
            }
            spent => Spend::Spent {
                answered: spend_refusal(spent),
            },
        }
    }

    /// Take a request for the Spaces service: within its account's and its
    /// domain's allowance it is queued for the Spaces host, in its sender's
    /// order and under the bounds of the queues account traffic takes. A
    /// request that finds those queues full holds its stream until it
    /// fits; one beyond an allowance does not.
    pub(super) fn space_request(
        self: &Arc<Self>,
        stream: &VerifiedPeerStream,
        principal: RemotePrincipal,
        operation: SpaceOperation,
        stanza: Stanza,
    ) -> SinkVerdict {
        if self.spaces.get().is_none() {
            return SinkVerdict::Accepted;
        }
        let peer = stream.remote().as_domain_ref().as_str().to_owned();
        let sender = principal.address().to_bare();
        match self.take_space_allowance(&peer, &sender) {
            Spend::Taken => {}
            Spend::Spent { answered } => {
                telemetry::space_request_limited();
                if answered {
                    let error = replies::error(
                        self.home(),
                        ErrorType::Wait,
                        DefinedCondition::ResourceConstraint,
                        "Too many requests; try again later.",
                    );
                    if let Some(reply) = replies::error_reply(&stanza, error) {
                        self.reply(stream, reply);
                    }
                }
                return SinkVerdict::Accepted;
            }
        }
        let work = RoomWork::Space(Box::new(SpaceTraffic {
            stream: stream.clone(),
            principal,
            operation,
            stanza,
        }));
        self.queue_inbound(sender, work, peer)
    }

    /// Hand one queued request to the Spaces host and send its answer back
    /// to the sender's domain. The host's work runs in a task of its own,
    /// so that a worker that stops waiting for it never drops it in the
    /// middle of a write; an answer that comes after the deadline is
    /// dropped, and the sender's next request is decided meanwhile.
    pub(super) async fn space_traffic(self: &Arc<Self>, traffic: SpaceTraffic) {
        let SpaceTraffic {
            stream,
            principal,
            operation,
            stanza,
        } = traffic;
        let Some(host) = self.spaces.get().cloned() else {
            return;
        };
        let request = SpaceRequest {
            principal,
            operation,
            stanza,
        };
        let task = self.tasks.spawn(async move { host.handle(request).await });
        let Ok(Ok(answers)) = tokio::time::timeout(self.limits.space_timeout, task).await else {
            telemetry::space_request_timed_out();
            return;
        };
        match answers {
            SpaceReply::Ordinary(answers) => {
                for answer in answers.into_iter().take(MAX_SPACE_REPLIES) {
                    self.reply(&stream, answer);
                }
            }
            SpaceReply::Protected(reply) => {
                let (stanza, grant) = *reply;
                self.reply_with_space_grant(&stream, stanza, Some(Arc::new(grant)));
            }
        }
    }

    /// Queue an event notification of the local Space service `service`
    /// for `subscriber`, without waiting. It leaves only for a domain peer
    /// admission admits now, opens its pair's queue only within the bound
    /// on queues remote senders open, and holds one place of the pair's
    /// share of event notifications; it keeps no queue alive once it is
    /// sent. `false` when it was dropped instead.
    pub(super) fn offer_space_event(
        self: &Arc<Self>,
        service: &BareJid,
        subscriber: &Jid,
        stanza: Stanza,
        grant: Option<Arc<crate::permissions::host_grant::ManagedSpaceGrant>>,
    ) -> bool {
        let pair = match (
            CanonicalDomain::parse_canonical(service.domain().as_str()),
            CanonicalDomain::parse_canonical(subscriber.domain().as_str()),
        ) {
            (Ok(local), Ok(remote)) => Pair { local, remote },
            _ => return false,
        };
        if !self.admits(&pair.remote) {
            telemetry::space_event_dropped();
            return false;
        }
        let share = {
            let mut shares = lock(&self.space_event_shares);
            let held = shares.get(&pair).copied().unwrap_or(0);
            if held >= self.limits.space_events_per_peer {
                None
            } else {
                shares.insert(pair.clone(), held + 1);
                Some(EventShare {
                    shares: Arc::clone(&self.space_event_shares),
                    pair: pair.clone(),
                })
            }
        };
        let Some(share) = share else {
            telemetry::space_event_dropped();
            return false;
        };
        // Opened as a reply's queue is: within the share of the pair queues
        // that what remote senders ask for may open.
        let Some(permit) = self.reserve(pair, true) else {
            telemetry::space_event_dropped();
            return false;
        };
        permit.send(Queued {
            stanza,
            purpose: Purpose::SpaceEvent {
                grant,
                _share: share,
            },
        });
        telemetry::space_event_queued();
        true
    }
}
