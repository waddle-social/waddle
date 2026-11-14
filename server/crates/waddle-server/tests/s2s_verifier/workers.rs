//! Matrix row "Resource/cancellation in the verifier", second half: four
//! occupied verifier workers plus new requests, cancellation during a job,
//! a late completion, no permit reuse before completion and finite worker
//! release.
//!
//! Jobs block on channels the test controls, never on the clock. The
//! timeouts only turn a defect into a failure instead of a hang.

use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;
use std::time::Duration;

use rustls::{ClientConnection, Connection, ServerConnection};
use tokio::time::timeout;
use waddle_server::s2s::verify::limits::{VERIFIER_WAITERS, VERIFIER_WORKERS};
use waddle_server::s2s::verify::{
    initiating_config, receiving_config, Completion, OutboundTransport, VerifierWorkers,
    WorkerError, WorkerLost,
};

use crate::harness::{domain, settings, verifier, Fixture, HOME_A, HOME_B};
use crate::pki::NOW;

const PATIENCE: Duration = Duration::from_secs(60);

/// Start a job that blocks until the returned sender is used or dropped,
/// then returns `value`.
fn blocked_job(workers: &VerifierWorkers, value: usize) -> (Sender<()>, Completion<usize>) {
    let (release, gate) = channel::<()>();
    let completion = workers
        .try_run(move || {
            // An error means the sender was dropped, which also releases
            // the job.
            let _ = gate.recv();
            value
        })
        .expect("a free worker");
    (release, completion)
}

async fn within<T>(future: impl std::future::Future<Output = T>) -> T {
    timeout(PATIENCE, future)
        .await
        .expect("the worker made progress")
}

#[tokio::test]
async fn occupied_workers_refuse_further_jobs_until_one_finishes() {
    let workers = VerifierWorkers::new(VERIFIER_WORKERS, 0).expect("a worker limit");
    assert_eq!(workers.limit(), 4);
    assert_eq!(workers.available(), 4);
    let mut jobs: Vec<_> = (0..4).map(|value| blocked_job(&workers, value)).collect();

    assert_eq!(workers.available(), 0);
    assert_eq!(workers.try_run(|| ()).err(), Some(WorkerError::Saturated));
    // No caller may wait either: the request is refused, not queued.
    assert_eq!(workers.run(|| ()).await.err(), Some(WorkerError::Saturated));
    assert_eq!(workers.waiting(), 0);

    // One job finishes; its worker is free once its result is delivered.
    let (release, completion) = jobs.remove(0);
    release.send(()).expect("the job is waiting");
    assert_eq!(within(completion).await, Ok(0));
    assert_eq!(workers.available(), 1);
    let next = workers.try_run(|| 7).expect("the freed worker");
    assert_eq!(within(next).await, Ok(7));

    for (release, completion) in jobs {
        drop(release);
        within(completion).await.expect("the job's result");
    }
    assert_eq!(workers.available(), 4);
}

#[tokio::test]
async fn a_caller_that_may_not_wait_still_gets_a_free_worker() {
    let workers = VerifierWorkers::new(1, 0).expect("a worker limit");

    // A free worker takes the job although nobody may wait.
    let first = workers.run(|| 1).await.expect("a free worker");
    assert_eq!(within(first).await, Ok(1));

    // An occupied one refuses it.
    let (release, completion) = blocked_job(&workers, 2);
    assert_eq!(workers.run(|| 3).await.err(), Some(WorkerError::Saturated));
    assert_eq!(workers.waiting(), 0);
    release.send(()).expect("the job is waiting");
    assert_eq!(within(completion).await, Ok(2));
}

#[test]
fn a_worker_limit_the_runtime_cannot_count_is_refused() {
    assert_eq!(
        VerifierWorkers::new(VerifierWorkers::MAX_LIMIT + 1, 0).err(),
        Some(WorkerError::LimitTooLarge)
    );
    assert_eq!(
        VerifierWorkers::new(usize::MAX, VERIFIER_WAITERS).err(),
        Some(WorkerError::LimitTooLarge)
    );
    // A limit of zero is raised to one.
    assert_eq!(
        VerifierWorkers::new(0, 0).expect("a worker limit").limit(),
        1
    );
    assert_eq!(VerifierWorkers::default().limit(), VERIFIER_WORKERS);
}

#[tokio::test]
async fn a_cancelled_job_keeps_its_worker_until_it_has_finished() {
    let workers = VerifierWorkers::new(VERIFIER_WORKERS, 1).expect("a worker limit");
    let mut jobs: Vec<_> = (0..4).map(|value| blocked_job(&workers, value)).collect();

    // The connection gives up: it drops the completion and would close its
    // socket. The job is still running and still holds its worker.
    let (release, completion) = jobs.remove(0);
    drop(completion);
    assert_eq!(workers.available(), 0);
    assert_eq!(workers.try_run(|| ()).err(), Some(WorkerError::Saturated));

    // A request that waits gets the worker only after the abandoned job
    // has run to its end.
    let waiting = {
        let workers = workers.clone();
        tokio::spawn(async move { workers.run(|| 9).await })
    };
    within(async {
        while workers.waiting() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert!(!waiting.is_finished());
    assert_eq!(workers.available(), 0);

    release
        .send(())
        .expect("the abandoned job is still running");
    let next = within(waiting)
        .await
        .expect("the waiting task")
        .expect("the released worker");
    // The late result of the abandoned job is discarded; the next job
    // produces its own.
    assert_eq!(within(next).await, Ok(9));
    assert_eq!(workers.waiting(), 0);

    for (release, completion) in jobs {
        drop(release);
        within(completion).await.expect("the job's result");
    }
    assert_eq!(workers.available(), 4);
}

#[tokio::test]
async fn only_a_bounded_number_of_callers_may_wait_for_a_worker() {
    let workers = VerifierWorkers::new(1, 1).expect("a worker limit");
    let (release, completion) = blocked_job(&workers, 1);

    let first_waiter = {
        let workers = workers.clone();
        tokio::spawn(async move { workers.run(|| 2).await })
    };
    within(async {
        while workers.waiting() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;

    // The one waiting place is taken.
    assert_eq!(workers.run(|| 3).await.err(), Some(WorkerError::Saturated));

    release.send(()).expect("the job is waiting");
    assert_eq!(within(completion).await, Ok(1));
    let second = within(first_waiter)
        .await
        .expect("the waiting task")
        .expect("the released worker");
    assert_eq!(within(second).await, Ok(2));
    assert_eq!(workers.waiting(), 0);
    assert_eq!(workers.available(), 1);
}

#[tokio::test]
async fn a_caller_that_stops_waiting_gives_up_its_place() {
    let workers = VerifierWorkers::new(1, 1).expect("a worker limit");
    let (release, completion) = blocked_job(&workers, 1);

    let waiter = {
        let workers = workers.clone();
        tokio::spawn(async move { workers.run(|| 2).await })
    };
    within(async {
        while workers.waiting() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    waiter.abort();
    assert!(within(waiter)
        .await
        .expect_err("an aborted task")
        .is_cancelled());
    assert_eq!(workers.waiting(), 0);

    drop(release);
    assert_eq!(within(completion).await, Ok(1));
    assert_eq!(workers.available(), 1);
}

#[tokio::test]
async fn a_job_that_panics_frees_its_worker() {
    let workers = VerifierWorkers::new(1, 0).expect("a worker limit");

    let completion = workers
        .try_run(|| -> usize { panic!("a failing job") })
        .expect("a free worker");

    assert_eq!(within(completion).await, Err(WorkerLost));
    // The worker's permit is released while the thread unwinds, which may
    // be just after the result channel closes.
    within(async {
        while workers.available() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    let next = workers.try_run(|| 5).expect("the freed worker");
    assert_eq!(within(next).await, Ok(5));
}

/// Process the pending input of `connection` on a verifier worker, and
/// report which thread did it.
async fn process_on_worker(
    workers: &VerifierWorkers,
    mut connection: Connection,
) -> (Connection, Option<rustls::Error>, Option<String>) {
    let job = workers
        .run(move || {
            let error = connection.process_new_packets().err();
            let thread = std::thread::current().name().map(str::to_owned);
            (connection, error, thread)
        })
        .await
        .expect("a verifier worker");
    within(job).await.expect("the handshake step")
}

/// Move the pending TLS records of `from` into `to` without processing
/// them.
fn forward(from: &mut Connection, to: &mut Connection) -> usize {
    let mut moved = 0;
    while from.wants_write() {
        let mut records = Vec::new();
        moved += from
            .write_tls(&mut records)
            .expect("write TLS records to memory");
        let mut unread = &records[..];
        while !unread.is_empty() {
            to.read_tls(&mut unread)
                .expect("read TLS records from memory");
        }
    }
    moved
}

#[tokio::test]
async fn a_handshake_is_processed_on_verifier_workers() {
    let fixture = Fixture::private();
    let home_a = fixture.issuer.issue_for(&[HOME_A]);
    let home_b = fixture.issuer.issue_for(&[HOME_B]);
    let initiating = verifier(&fixture.inventory, HOME_B, NOW);
    let receiving = verifier(&fixture.inventory, HOME_A, NOW);
    let client_config = initiating_config(
        &settings(NOW),
        home_a.certified_key(),
        Arc::clone(&initiating),
        OutboundTransport::StartTls,
    )
    .expect("an initiating configuration");
    let server_config = receiving_config(
        &settings(NOW),
        home_b.certified_key(),
        Arc::clone(&receiving),
    )
    .expect("a receiving configuration");
    let mut client = Connection::Client(
        ClientConnection::new(
            Arc::new(client_config),
            initiating.reference().server_name(),
        )
        .expect("an initiating connection"),
    );
    let mut server = Connection::Server(
        ServerConnection::new(Arc::new(server_config)).expect("a receiving connection"),
    );
    let workers = VerifierWorkers::default();

    let mut threads = Vec::new();
    for _ in 0..8 {
        let to_server = forward(&mut client, &mut server);
        let (processed, error, thread) = process_on_worker(&workers, server).await;
        server = processed;
        assert_eq!(error, None);
        threads.push(thread);

        let to_client = forward(&mut server, &mut client);
        let (processed, error, thread) = process_on_worker(&workers, client).await;
        client = processed;
        assert_eq!(error, None);
        threads.push(thread);

        if to_server == 0 && to_client == 0 {
            break;
        }
    }

    assert!(!client.is_handshaking());
    assert!(!server.is_handshaking());
    assert_eq!(
        initiating
            .established(&client)
            .expect("the initiating connection's evidence")
            .domain(),
        &domain(HOME_B)
    );
    assert_eq!(
        receiving
            .established(&server)
            .expect("the receiving connection's evidence")
            .domain(),
        &domain(HOME_A)
    );
    // Every step, and with it both certificate verifications, ran on a
    // worker thread, not on the test's runtime thread.
    assert!(threads
        .iter()
        .all(|thread| thread.as_deref() == Some("s2s-tls-verify")));
    assert_eq!(workers.available(), workers.limit());
}
