use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{ClientId, ContractError, Manifest, MessageId, Phase, Plan, PlannedMessage};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ObservationSet {
    pub schema_version: u16,
    pub manifest: Manifest,
    /// Milliseconds from the same monotonic run origin used by the plan.
    pub closed_at_ms: u64,
    pub offers: Vec<OfferObservation>,
    pub receipts: Vec<Receipt>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OfferObservation {
    pub message: MessageId,
    pub outcome: OfferOutcome,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OfferOutcome {
    Sent { at_ms: u64 },
    NotSent { at_ms: u64 },
}

impl OfferOutcome {
    fn at_ms(self) -> u64 {
        match self {
            Self::Sent { at_ms } | Self::NotSent { at_ms } => at_ms,
        }
    }
}

/// A protocol receipt observation, never an assertion of UI visibility or durability.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Receipt {
    pub message: MessageId,
    pub recipient: ClientId,
    pub at_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccountingStatus {
    Complete,
    Inconclusive,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Qualification {
    NotAssessed,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationCoverage {
    Unverified,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GeneratorOutcome {
    Sent { at_ms: u64, lag_ms: u64 },
    NotSent { at_ms: u64 },
    MissingObservation,
    ScheduledAfterClose,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairOutcome {
    OnTime { protocol_receipt_latency_ms: u64 },
    Late { protocol_receipt_latency_ms: u64 },
    TimedOut,
    Censored,
    NotYetOffered,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct PairResult {
    pub recipient: ClientId,
    pub outcome: PairOutcome,
    pub receipt_times_ms: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MessageResult {
    pub message: MessageId,
    pub generator: GeneratorOutcome,
    pub recipients: Vec<PairResult>,
}

#[derive(Clone, Debug, Default, Serialize, PartialEq, Eq)]
pub struct Summary {
    pub expected_pairs: u64,
    pub on_time_pairs: u64,
    pub late_pairs: u64,
    pub timed_out_pairs: u64,
    pub censored_pairs: u64,
    pub not_yet_offered_pairs: u64,
    pub duplicate_receipts: u64,
    pub lagged_messages: u64,
    pub not_sent_messages: u64,
    pub missing_offer_observations: u64,
    pub scheduled_after_close_messages: u64,
}

impl Summary {
    fn add(&mut self, message: &MessageResult, lag_budget_ms: u64) {
        match message.generator {
            GeneratorOutcome::Sent { lag_ms, .. } if lag_ms > lag_budget_ms => {
                self.lagged_messages += 1;
            }
            GeneratorOutcome::NotSent { .. } => self.not_sent_messages += 1,
            GeneratorOutcome::MissingObservation => self.missing_offer_observations += 1,
            GeneratorOutcome::ScheduledAfterClose => self.scheduled_after_close_messages += 1,
            GeneratorOutcome::Sent { .. } => {}
        }
        for pair in &message.recipients {
            self.expected_pairs += 1;
            self.duplicate_receipts += pair.receipt_times_ms.len().saturating_sub(1) as u64;
            match pair.outcome {
                PairOutcome::OnTime { .. } => self.on_time_pairs += 1,
                PairOutcome::Late { .. } => self.late_pairs += 1,
                PairOutcome::TimedOut => self.timed_out_pairs += 1,
                PairOutcome::Censored => self.censored_pairs += 1,
                PairOutcome::NotYetOffered => self.not_yet_offered_pairs += 1,
            }
        }
    }

    fn inconclusive(&self) -> bool {
        self.lagged_messages > 0
            || self.not_sent_messages > 0
            || self.missing_offer_observations > 0
            || self.scheduled_after_close_messages > 0
            || self.censored_pairs > 0
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Report {
    pub schema_version: u16,
    pub qualification: Qualification,
    pub observation_coverage: ObservationCoverage,
    pub accounting_status: AccountingStatus,
    pub closed_at_ms: u64,
    pub plan: Plan,
    pub all_phases: Summary,
    pub measurement: Summary,
    pub messages: Vec<MessageResult>,
}

pub fn account(plan: Plan, observations: ObservationSet) -> Result<Report, ContractError> {
    let invalid = ContractError::Observations;
    if observations.schema_version != 1 || &observations.manifest != plan.manifest() {
        return Err(invalid(
            "schema_version must be 1 and the complete observation manifest must match the plan",
        ));
    }
    if observations.closed_at_ms > plan.totals().duration_ms {
        return Err(invalid("collection close exceeds the planned run duration"));
    }
    if observations.offers.len() as u64 > plan.totals().offered_messages
        || observations.receipts.len() as u64 > plan.totals().expected_pairs * 4
    {
        return Err(invalid(
            "exceeds one offer per message or four receipts per expected pair in total",
        ));
    }
    let planned: BTreeMap<_, _> = plan.messages().iter().map(|m| (m.id, m)).collect();
    let mut offers = BTreeMap::new();
    for observation in observations.offers {
        let Some(message) = planned.get(&observation.message) else {
            return Err(invalid("offer references an unknown message"));
        };
        let at_ms = observation.outcome.at_ms();
        if at_ms < message.due_at_ms || at_ms > observations.closed_at_ms {
            return Err(invalid(
                "offer timestamp is before its due time or after collection close",
            ));
        }
        if offers
            .insert(observation.message, observation.outcome)
            .is_some()
        {
            return Err(invalid("duplicate offer observation"));
        }
    }
    let mut receipts: BTreeMap<_, Vec<u64>> = BTreeMap::new();
    for receipt in observations.receipts {
        let Some(message) = planned.get(&receipt.message) else {
            return Err(invalid("receipt references an unknown message"));
        };
        if !message.recipients.contains(&receipt.recipient) {
            return Err(invalid("receipt references an unexpected recipient"));
        }
        let Some(OfferOutcome::Sent { at_ms }) = offers.get(&receipt.message) else {
            return Err(invalid("receipt requires a sent observation"));
        };
        if receipt.at_ms < *at_ms || receipt.at_ms > observations.closed_at_ms {
            return Err(invalid(
                "receipt timestamp is before send or after collection close",
            ));
        }
        receipts
            .entry((receipt.message, receipt.recipient))
            .or_default()
            .push(receipt.at_ms);
    }
    let mut all_phases = Summary::default();
    let mut measurement = Summary::default();
    let mut messages = Vec::new();
    for message in plan.messages() {
        let generator = match offers.get(&message.id) {
            Some(OfferOutcome::Sent { at_ms }) => GeneratorOutcome::Sent {
                at_ms: *at_ms,
                lag_ms: at_ms - message.due_at_ms,
            },
            Some(OfferOutcome::NotSent { at_ms }) => GeneratorOutcome::NotSent { at_ms: *at_ms },
            None if message.due_at_ms > observations.closed_at_ms => {
                GeneratorOutcome::ScheduledAfterClose
            }
            None => GeneratorOutcome::MissingObservation,
        };
        let recipients = message
            .recipients
            .iter()
            .map(|recipient| {
                let mut receipt_times_ms = receipts
                    .remove(&(message.id, *recipient))
                    .unwrap_or_default();
                receipt_times_ms.sort_unstable();
                PairResult {
                    recipient: *recipient,
                    outcome: pair_outcome(message, &receipt_times_ms, observations.closed_at_ms),
                    receipt_times_ms,
                }
            })
            .collect();
        let result = MessageResult {
            message: message.id,
            generator,
            recipients,
        };
        all_phases.add(&result, plan.manifest().max_generator_lag_ms);
        if message.phase == Phase::Measurement {
            measurement.add(&result, plan.manifest().max_generator_lag_ms);
        }
        messages.push(result);
    }
    let accounting_status =
        if observations.closed_at_ms < plan.totals().duration_ms || all_phases.inconclusive() {
            AccountingStatus::Inconclusive
        } else {
            AccountingStatus::Complete
        };
    Ok(Report {
        schema_version: 1,
        qualification: Qualification::NotAssessed,
        observation_coverage: ObservationCoverage::Unverified,
        accounting_status,
        closed_at_ms: observations.closed_at_ms,
        plan,
        all_phases,
        measurement,
        messages,
    })
}

fn pair_outcome(message: &PlannedMessage, receipts: &[u64], closed_at_ms: u64) -> PairOutcome {
    match receipts.first() {
        Some(at_ms) if *at_ms <= message.deadline_ms => PairOutcome::OnTime {
            protocol_receipt_latency_ms: at_ms - message.due_at_ms,
        },
        Some(at_ms) => PairOutcome::Late {
            protocol_receipt_latency_ms: at_ms - message.due_at_ms,
        },
        None if closed_at_ms < message.due_at_ms => PairOutcome::NotYetOffered,
        None if closed_at_ms < message.deadline_ms => PairOutcome::Censored,
        None => PairOutcome::TimedOut,
    }
}
