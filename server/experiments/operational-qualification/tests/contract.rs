use serde_json::json;
use waddle_operational_qualification::{
    account, AccountingStatus, ClientId, ContractError, GeneratorOutcome, Manifest, MessageId,
    ObservationCoverage, ObservationSet, OfferObservation, OfferOutcome, PairOutcome, Phase,
    Phases, Plan, Qualification, Receipt, RetryPolicy, Room, RoomId,
};

fn manifest() -> Manifest {
    Manifest {
        schema_version: 1,
        run_id: 42,
        client_count: 3,
        rooms: vec![Room {
            id: RoomId(7),
            senders: vec![ClientId(1)],
            recipients: vec![ClientId(2), ClientId(3)],
        }],
        phases: Phases {
            warmup_ms: 0,
            measurement_ms: 300,
            drain_ms: 200,
        },
        offer_interval_ms: 100,
        payload_bytes: 512,
        delivery_timeout_ms: 100,
        max_generator_lag_ms: 0,
        retries: RetryPolicy::Disabled,
        repetitions: 1,
        cooldown_ms: 0,
    }
}

fn message(sequence: u32) -> MessageId {
    MessageId {
        repetition: 1,
        sequence,
    }
}

fn observations() -> ObservationSet {
    ObservationSet {
        schema_version: 1,
        manifest: manifest(),
        closed_at_ms: 500,
        offers: (0..3)
            .map(|sequence| OfferObservation {
                message: message(sequence),
                outcome: OfferOutcome::Sent {
                    at_ms: u64::from(sequence) * 100,
                },
            })
            .collect(),
        receipts: Vec::new(),
    }
}

fn receipt(sequence: u32, recipient: u16, at_ms: u64) -> Receipt {
    Receipt {
        message: message(sequence),
        recipient: ClientId(recipient),
        at_ms,
    }
}

#[test]
fn schedule_freezes_phases_distribution_repetitions_and_costs() {
    let mut input = manifest();
    input.phases.warmup_ms = 100;
    input.repetitions = 2;
    input.cooldown_ms = 100;
    input.rooms[0].senders.push(ClientId(2));
    input.rooms.push(Room {
        id: RoomId(9),
        senders: vec![ClientId(3)],
        recipients: vec![ClientId(1)],
    });
    let plan = Plan::build(input.clone()).expect("valid manifest");
    let actual: Vec<_> = plan
        .messages()
        .iter()
        .map(|m| {
            (
                m.id,
                m.phase,
                m.due_at_ms,
                m.deadline_ms,
                m.room.0,
                m.sender.0,
            )
        })
        .collect();
    assert_eq!(
        actual,
        vec![
            (
                MessageId {
                    repetition: 1,
                    sequence: 0
                },
                Phase::Warmup,
                0,
                100,
                7,
                1
            ),
            (
                MessageId {
                    repetition: 1,
                    sequence: 1
                },
                Phase::Measurement,
                100,
                200,
                9,
                3
            ),
            (
                MessageId {
                    repetition: 1,
                    sequence: 2
                },
                Phase::Measurement,
                200,
                300,
                7,
                2
            ),
            (
                MessageId {
                    repetition: 1,
                    sequence: 3
                },
                Phase::Measurement,
                300,
                400,
                9,
                3
            ),
            (
                MessageId {
                    repetition: 2,
                    sequence: 0
                },
                Phase::Warmup,
                700,
                800,
                7,
                1
            ),
            (
                MessageId {
                    repetition: 2,
                    sequence: 1
                },
                Phase::Measurement,
                800,
                900,
                9,
                3
            ),
            (
                MessageId {
                    repetition: 2,
                    sequence: 2
                },
                Phase::Measurement,
                900,
                1000,
                7,
                2
            ),
            (
                MessageId {
                    repetition: 2,
                    sequence: 3
                },
                Phase::Measurement,
                1000,
                1100,
                9,
                3
            ),
        ]
    );
    assert_eq!(plan.totals().duration_ms, 1300);
    assert_eq!(plan.totals().offered_messages, 8);
    assert_eq!(plan.totals().expected_pairs, 12);
    assert_eq!(plan.totals().offered_payload_bytes, 4096);
    assert_eq!(plan.totals().recipient_payload_bytes, 6144);
    assert_eq!(
        plan.messages()[0].recipients,
        vec![ClientId(2), ClientId(3)]
    );
    assert_eq!(plan.messages()[1].recipients, vec![ClientId(1)]);
    assert_eq!(
        serde_json::to_vec(&plan).expect("serialize"),
        serde_json::to_vec(&Plan::build(input).expect("repeat")).expect("serialize")
    );
}

#[test]
fn invalid_manifests_reject_ambiguous_or_unbounded_runs() {
    let cases: Vec<fn(&mut Manifest)> = vec![
        |m| m.schema_version = 2,
        |m| m.run_id = 0,
        |m| m.client_count = 0,
        |m| m.client_count = 1001,
        |m| m.rooms.clear(),
        |m| m.rooms.push(m.rooms[0].clone()),
        |m| m.rooms[0].id = RoomId(0),
        |m| m.rooms[0].senders.clear(),
        |m| m.rooms[0].recipients.clear(),
        |m| m.rooms[0].recipients = vec![ClientId(2), ClientId(2)],
        |m| m.rooms[0].senders = vec![ClientId(0)],
        |m| m.rooms[0].recipients = vec![ClientId(4)],
        |m| m.offer_interval_ms = 0,
        |m| m.offer_interval_ms = u64::MAX,
        |m| m.phases.warmup_ms = 1,
        |m| m.phases.measurement_ms = 0,
        |m| m.phases.measurement_ms = u64::MAX,
        |m| m.delivery_timeout_ms = 0,
        |m| m.delivery_timeout_ms = 201,
        |m| m.max_generator_lag_ms = 101,
        |m| m.repetitions = 0,
        |m| m.repetitions = 11,
        |m| m.cooldown_ms = u64::MAX,
        |m| m.payload_bytes = 0,
        |m| m.payload_bytes = 65537,
        |m| {
            m.phases.measurement_ms = 3_600_000;
            m.offer_interval_ms = 1;
        },
        |m| {
            m.phases.measurement_ms = 10_000;
            m.offer_interval_ms = 1;
            m.payload_bytes = 65536;
        },
        |m| {
            m.client_count = 1000;
            m.rooms[0].recipients = (1..=1000).map(ClientId).collect();
            m.phases.measurement_ms = 20_000;
        },
        |m| {
            m.phases.warmup_ms = 3_600_000;
            m.phases.measurement_ms = 3_600_000;
            m.phases.drain_ms = 3_600_000;
            m.offer_interval_ms = 3_600_000;
            m.repetitions = 10;
        },
    ];
    for (index, mutate) in cases.into_iter().enumerate() {
        let mut input = manifest();
        mutate(&mut input);
        assert!(
            matches!(Plan::build(input), Err(ContractError::Manifest(_))),
            "case {index}"
        );
    }
}

#[test]
fn schemas_reject_unknown_fields_credentials_and_retry_modes() {
    let original = serde_json::to_value(manifest()).expect("serialize");
    for field in ["endpoint", "password", "token", "environment"] {
        let mut input = original.clone();
        input[field] = json!("must not enter a plan");
        assert!(serde_json::from_value::<Manifest>(input).is_err());
    }
    let mut input = original.clone();
    input["rooms"][0]["unknown"] = json!(1);
    assert!(serde_json::from_value::<Manifest>(input).is_err());
    let mut input = original;
    input["retries"] = json!("unbounded");
    assert!(serde_json::from_value::<Manifest>(input).is_err());
    let mut input = serde_json::to_value(observations()).expect("serialize");
    input["offers"][0]["outcome"]["unknown"] = json!(1);
    assert!(serde_json::from_value::<ObservationSet>(input).is_err());
}

#[test]
fn missing_pairs_remain_in_denominator_and_late_receipts_are_explicit() {
    let mut input = observations();
    input.receipts = vec![
        receipt(2, 3, 260),
        receipt(0, 3, 101),
        receipt(1, 3, 140),
        receipt(0, 2, 100),
        receipt(2, 3, 250),
    ];
    let report = account(Plan::build(manifest()).expect("plan"), input).expect("account");
    assert_eq!(report.qualification, Qualification::NotAssessed);
    assert_eq!(report.accounting_status, AccountingStatus::Complete);
    assert_eq!(report.measurement.expected_pairs, 6);
    assert_eq!(report.measurement.on_time_pairs, 3);
    assert_eq!(report.measurement.late_pairs, 1);
    assert_eq!(report.measurement.timed_out_pairs, 2);
    assert_eq!(report.measurement.duplicate_receipts, 1);
    assert_eq!(
        report.messages[0].recipients[0].outcome,
        PairOutcome::OnTime {
            protocol_receipt_latency_ms: 100
        }
    );
    assert_eq!(
        report.messages[0].recipients[1].outcome,
        PairOutcome::Late {
            protocol_receipt_latency_ms: 101
        }
    );
    assert_eq!(
        report.messages[1].recipients[0].outcome,
        PairOutcome::TimedOut
    );
    assert_eq!(
        report.messages[2].recipients[1].receipt_times_ms,
        vec![250, 260]
    );
}

#[test]
fn all_timeouts_leave_observation_coverage_unverified_when_accounting_is_complete() {
    let report = account(Plan::build(manifest()).expect("plan"), observations()).expect("account");
    assert_eq!(report.accounting_status, AccountingStatus::Complete);
    assert_eq!(report.measurement.expected_pairs, 6);
    assert_eq!(report.measurement.timed_out_pairs, 6);
    assert_eq!(report.observation_coverage, ObservationCoverage::Unverified);
    let json = serde_json::to_value(report).expect("serialize");
    assert_eq!(json["observation_coverage"], "unverified");
}

#[test]
fn early_close_censors_pairs_and_keeps_future_offers_visible() {
    let mut input = observations();
    input.closed_at_ms = 50;
    input.offers.truncate(1);
    input.receipts.push(receipt(0, 2, 40));
    let report = account(Plan::build(manifest()).expect("plan"), input).expect("account");
    assert_eq!(report.accounting_status, AccountingStatus::Inconclusive);
    assert_eq!(report.measurement.expected_pairs, 6);
    assert_eq!(report.measurement.on_time_pairs, 1);
    assert_eq!(report.measurement.censored_pairs, 1);
    assert_eq!(report.measurement.not_yet_offered_pairs, 4);
    assert_eq!(report.measurement.timed_out_pairs, 0);
    assert_eq!(
        report.messages[0].recipients[1].outcome,
        PairOutcome::Censored
    );
    assert_eq!(
        report.messages[1].generator,
        GeneratorOutcome::ScheduledAfterClose
    );
}

#[test]
fn generator_lag_does_not_shift_offered_times_deadlines_or_latency_origin() {
    let mut input = observations();
    input.offers[0].outcome = OfferOutcome::Sent { at_ms: 50 };
    input.receipts.push(receipt(0, 2, 110));
    let report = account(Plan::build(manifest()).expect("plan"), input).expect("account");
    assert_eq!(report.accounting_status, AccountingStatus::Inconclusive);
    assert_eq!(report.measurement.lagged_messages, 1);
    assert_eq!(
        report.messages[0].generator,
        GeneratorOutcome::Sent {
            at_ms: 50,
            lag_ms: 50
        }
    );
    assert_eq!(
        report.messages[0].recipients[0].outcome,
        PairOutcome::Late {
            protocol_receipt_latency_ms: 110
        }
    );
    assert_eq!(report.plan.messages()[0].deadline_ms, 100);
    assert_eq!(report.plan.messages()[1].due_at_ms, 100);
}

#[test]
fn missing_and_not_sent_generator_outcomes_are_inconclusive_even_after_timeouts() {
    let mut input = observations();
    input.offers.truncate(1);
    input.offers[0].outcome = OfferOutcome::NotSent { at_ms: 1 };
    let report = account(Plan::build(manifest()).expect("plan"), input).expect("account");
    assert_eq!(report.accounting_status, AccountingStatus::Inconclusive);
    assert_eq!(report.measurement.not_sent_messages, 1);
    assert_eq!(report.measurement.missing_offer_observations, 2);
    assert_eq!(report.measurement.expected_pairs, 6);
    assert_eq!(report.measurement.timed_out_pairs, 6);
}

#[test]
fn invalid_observations_cannot_manufacture_receipt_success() {
    let cases: Vec<fn(&mut ObservationSet)> = vec![
        |o| o.schema_version = 2,
        |o| o.manifest.run_id = 99,
        |o| o.manifest.payload_bytes = 1024,
        |o| o.closed_at_ms = 501,
        |o| o.offers[0].message.sequence = 99,
        |o| o.offers[1].outcome = OfferOutcome::Sent { at_ms: 99 },
        |o| o.offers[0].outcome = OfferOutcome::Sent { at_ms: 501 },
        |o| o.offers[1] = o.offers[0].clone(),
        |o| o.receipts.push(receipt(99, 2, 10)),
        |o| o.receipts.push(receipt(0, 1, 10)),
        |o| {
            o.offers.remove(0);
            o.receipts.push(receipt(0, 2, 10));
        },
        |o| {
            o.offers[0].outcome = OfferOutcome::NotSent { at_ms: 0 };
            o.receipts.push(receipt(0, 2, 10));
        },
        |o| o.receipts.push(receipt(1, 2, 99)),
        |o| o.receipts.push(receipt(0, 2, 501)),
        |o| o.receipts = (0..25).map(|_| receipt(0, 2, 1)).collect(),
    ];
    for (index, mutate) in cases.into_iter().enumerate() {
        let mut input = observations();
        mutate(&mut input);
        assert!(
            matches!(
                account(Plan::build(manifest()).expect("plan"), input),
                Err(ContractError::Observations(_))
            ),
            "case {index}"
        );
    }
}

#[test]
fn observation_order_does_not_change_report() {
    let mut input = observations();
    input.receipts = vec![receipt(0, 2, 20), receipt(0, 2, 30), receipt(1, 3, 150)];
    let plan = Plan::build(manifest()).expect("plan");
    let first = account(plan.clone(), input.clone()).expect("account");
    input.offers.reverse();
    input.receipts.reverse();
    let second = account(plan, input).expect("account");
    assert_eq!(first, second);
}

#[test]
fn warmup_pairs_are_preserved_but_excluded_from_measurement_summary() {
    let mut input = manifest();
    input.phases.warmup_ms = 100;
    let plan = Plan::build(input.clone()).expect("plan");
    let mut observed = observations();
    observed.manifest = input;
    observed.closed_at_ms = 600;
    observed.offers.push(OfferObservation {
        message: message(3),
        outcome: OfferOutcome::Sent { at_ms: 300 },
    });
    observed.receipts.push(receipt(0, 2, 10));
    let report = account(plan, observed).expect("account");
    assert_eq!(report.all_phases.expected_pairs, 8);
    assert_eq!(report.all_phases.on_time_pairs, 1);
    assert_eq!(report.measurement.expected_pairs, 6);
    assert_eq!(report.measurement.on_time_pairs, 0);
}
