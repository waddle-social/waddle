use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use tokio::runtime::{Builder, Runtime};
use tokio::sync::Mutex as AsyncMutex;
use tokio::task::AbortHandle;
use waddle_flutter_timeline::TimelineStore;
use waddle_xmpp_client_ffi::{WaddleClient, WaddleClientEvent, WaddleEventListener};

use crate::error::RuntimeError;
use crate::events::{Envelope, Outbound};
use crate::generated::{Operation as Command, Output};
use crate::request::Request;

const MAX_PENDING: usize = 64;
const MAX_QUEUED: usize = 4096;
const POLL_BATCH: usize = 128;

static EXECUTOR: OnceLock<Option<Runtime>> = OnceLock::new();
static HANDLES: OnceLock<Mutex<HashMap<u64, Arc<ClientRuntime>>>> = OnceLock::new();
static NEXT_HANDLE: AtomicU64 = AtomicU64::new(1);

struct Pending {
    abort: Option<AbortHandle>,
    connecting: bool,
}

struct State {
    generation: u64,
    sequence: u64,
    retired: bool,
    accepts_events: bool,
    connected: bool,
    client: Option<Arc<WaddleClient>>,
    pending: HashMap<u64, Pending>,
    outstanding: HashSet<u64>,
    queue: VecDeque<Envelope>,
    timeline: TimelineStore,
}

pub(crate) struct ClientRuntime {
    executor: &'static Runtime,
    next_request: AtomicU64,
    state: Mutex<State>,
    lifecycle: AsyncMutex<()>,
    retiring: Mutex<Vec<Arc<WaddleClient>>>,
}

struct Listener {
    owner: Weak<ClientRuntime>,
    generation: u64,
}

impl WaddleEventListener for Listener {
    fn on_event(&self, event: WaddleClientEvent) {
        if let Some(owner) = self.owner.upgrade() {
            let connected = matches!(event, WaddleClientEvent::Connected);
            let displayed = match &event {
                WaddleClientEvent::Message { message } => message.mds_displayed.clone(),
                _ => None,
            };
            owner.event(self.generation, Outbound::Event(Box::new(event)));
            if connected {
                owner.event(self.generation, Outbound::SessionLifecycle);
            }
            if let Some(entries) = displayed {
                owner.event(self.generation, Outbound::MdsDisplayed(entries));
            }
        }
    }
}

fn next_id(counter: &AtomicU64) -> Option<u64> {
    counter
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| {
            value.checked_add(1)
        })
        .ok()
}

pub(crate) fn create() -> u64 {
    let Some(executor) = EXECUTOR
        .get_or_init(|| {
            Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .ok()
        })
        .as_ref()
    else {
        return 0;
    };
    let Some(id) = next_id(&NEXT_HANDLE) else {
        return 0;
    };
    let handle = Arc::new(ClientRuntime {
        executor,
        next_request: AtomicU64::new(1),
        lifecycle: AsyncMutex::new(()),
        retiring: Mutex::new(Vec::new()),
        state: Mutex::new(State {
            generation: 0,
            sequence: 0,
            retired: false,
            accepts_events: false,
            connected: false,
            client: None,
            pending: HashMap::new(),
            outstanding: HashSet::new(),
            queue: VecDeque::new(),
            timeline: TimelineStore::new(),
        }),
    });
    let Ok(mut handles) = HANDLES.get_or_init(|| Mutex::new(HashMap::new())).lock() else {
        return 0;
    };
    handles.insert(id, handle);
    id
}

pub(crate) fn lookup(id: u64) -> Option<Arc<ClientRuntime>> {
    HANDLES.get()?.lock().ok()?.get(&id).cloned()
}

pub(crate) fn destroy(id: u64) {
    let owner = HANDLES
        .get()
        .and_then(|handles| handles.lock().ok()?.remove(&id));
    if let Some(owner) = owner {
        owner.retire();
    }
}

impl ClientRuntime {
    pub(crate) fn submit(self: &Arc<Self>, input: &[u8]) -> u64 {
        let Some(request_id) = next_id(&self.next_request) else {
            return 0;
        };
        let request = serde_json::from_slice::<Request>(input);
        let Ok(mut state) = self.state.lock() else {
            return 0;
        };
        if state.retired || state.outstanding.len() >= MAX_PENDING {
            return 0;
        }
        if state.queue.len() >= MAX_QUEUED {
            let client = state.overflow();
            drop(state);
            self.cleanup(client);
            return 0;
        }
        state.outstanding.insert(request_id);
        match request {
            Ok(Request::Client(Command::Connect(args))) => {
                let config = match args.into_config() {
                    Ok(config) => config,
                    Err(error) => {
                        state.push_result(request_id, Err(error));
                        return request_id;
                    }
                };
                let old_client = state.advance_generation();
                if self.defer_close(old_client.as_ref()).is_err() {
                    state.client = old_client;
                    state.push_result(request_id, Err(RuntimeError::quiescence()));
                    return request_id;
                }
                let generation = state.generation;
                let client = WaddleClient::new(
                    config,
                    Box::new(Listener {
                        owner: Arc::downgrade(self),
                        generation,
                    }),
                );
                state.client = Some(Arc::clone(&client));
                state.accepts_events = true;
                state.pending.insert(
                    request_id,
                    Pending {
                        abort: None,
                        connecting: true,
                    },
                );
                drop(state);
                let owner = Arc::clone(self);
                // Never abort this future: connect installs its transport handle at the end.
                // A superseded connect must finish and explicitly disconnect its late handle.
                self.executor.spawn(async move {
                    let _gate = owner.lifecycle.lock().await;
                    if let Err(error) = owner.close_transports(None).await {
                        owner.complete(generation, request_id, Err(error));
                        return;
                    }
                    if !owner.is_current(generation) {
                        return;
                    }
                    client.connect().await;
                    if owner.is_current(generation) {
                        owner.complete(generation, request_id, Ok(Output::Unit));
                    } else {
                        let _ = owner.close_transports(Some(client)).await;
                    }
                });
            }
            Ok(Request::Client(Command::Disconnect {})) => {
                let old_client = state.advance_generation();
                if self.defer_close(old_client.as_ref()).is_err() {
                    state.client = old_client;
                    state.push_result(request_id, Err(RuntimeError::quiescence()));
                    return request_id;
                }
                let generation = state.generation;
                state.pending.insert(
                    request_id,
                    Pending {
                        abort: None,
                        connecting: true,
                    },
                );
                drop(state);
                let owner = Arc::clone(self);
                self.executor.spawn(async move {
                    let _gate = owner.lifecycle.lock().await;
                    let result = owner.close_transports(None).await.map(|()| Output::Unit);
                    owner.complete(generation, request_id, result);
                });
            }
            Ok(Request::Timeline(command)) => {
                let result = state
                    .timeline
                    .execute(command)
                    .map(Output::Timeline)
                    .map_err(|_| RuntimeError::invalid("Invalid timeline operation"));
                state.push_result(request_id, result);
            }
            Ok(Request::Client(command)) => {
                if let Some(output) = command.local() {
                    state.push_result(request_id, Ok(output));
                    return request_id;
                }
                if !state.connected {
                    state.push_result(request_id, Err(RuntimeError::unavailable()));
                    return request_id;
                }
                let Some(client) = state.client.clone() else {
                    state.push_result(request_id, Err(RuntimeError::unavailable()));
                    return request_id;
                };
                let generation = state.generation;
                state.pending.insert(
                    request_id,
                    Pending {
                        abort: None,
                        connecting: false,
                    },
                );
                drop(state);
                let owner = Arc::clone(self);
                let task = self.executor.spawn(async move {
                    let result = command.dispatch(&client).await;
                    owner.complete(generation, request_id, result);
                });
                if let Ok(mut state) = self.state.lock() {
                    if let Some(pending) = state.pending.get_mut(&request_id) {
                        pending.abort = Some(task.abort_handle());
                    }
                }
            }
            Err(_) => state.push_result(
                request_id,
                Err(RuntimeError::invalid("Unknown method or invalid arguments")),
            ),
        }
        request_id
    }

    fn is_current(&self, generation: u64) -> bool {
        self.state
            .lock()
            .map(|state| !state.retired && state.generation == generation && state.accepts_events)
            .unwrap_or(false)
    }

    fn complete(
        self: &Arc<Self>,
        generation: u64,
        request_id: u64,
        result: Result<Output, RuntimeError>,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.retired
            || state.generation != generation
            || !state.pending.contains_key(&request_id)
        {
            return;
        }
        if state.queue.len() >= MAX_QUEUED {
            let client = state.overflow();
            drop(state);
            self.cleanup(client);
        } else {
            state.pending.remove(&request_id);
            state.push_result(request_id, result);
        }
    }

    fn event(self: &Arc<Self>, generation: u64, event: Outbound) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        if state.retired || !state.accepts_events || state.generation != generation {
            return;
        }
        if state.queue.len() >= MAX_QUEUED {
            let client = state.overflow();
            drop(state);
            self.cleanup(client);
        } else {
            if let Outbound::Event(event) = &event {
                match event.as_ref() {
                    WaddleClientEvent::Connected => state.connected = true,
                    WaddleClientEvent::Disconnected
                    | WaddleClientEvent::AuthenticationFailed { .. } => state.connected = false,
                    _ => {}
                }
            }
            state.push(event);
        }
    }

    pub(crate) fn poll(&self) -> Vec<Envelope> {
        let Ok(mut state) = self.state.lock() else {
            return Vec::new();
        };
        let count = POLL_BATCH.min(state.queue.len());
        let frames: Vec<Envelope> = state.queue.drain(..count).collect();
        for frame in &frames {
            if let Outbound::Result { request_id, .. } = &frame.outbound {
                state.outstanding.remove(request_id);
            }
        }
        frames
    }

    // Publish retired clients before scheduling their cleanup. Otherwise a later
    // disconnect could acquire the lifecycle gate first and miss an old socket
    // owned only by a cleanup future that has not started running yet.
    fn defer_close(&self, client: Option<&Arc<WaddleClient>>) -> Result<(), RuntimeError> {
        if let Some(client) = client {
            let mut held = self
                .retiring
                .lock()
                .map_err(|_| RuntimeError::quiescence())?;
            if !held.iter().any(|existing| Arc::ptr_eq(existing, client)) {
                held.push(Arc::clone(client));
            }
        }
        Ok(())
    }

    fn cleanup(self: &Arc<Self>, client: Option<Arc<WaddleClient>>) {
        if self.defer_close(client.as_ref()).is_err() {
            return;
        }
        let owner = Arc::clone(self);
        self.executor.spawn(async move {
            let _gate = owner.lifecycle.lock().await;
            let _ = owner.close_transports(None).await;
        });
    }

    async fn close_transports(
        &self,
        client: Option<Arc<WaddleClient>>,
    ) -> Result<(), RuntimeError> {
        let mut clients = self
            .retiring
            .lock()
            .map(|mut held| std::mem::take(&mut *held))
            .map_err(|_| RuntimeError::quiescence())?;
        if let Some(client) = client {
            if !clients.iter().any(|held| Arc::ptr_eq(held, &client)) {
                clients.push(client);
            }
        }
        let mut clients = clients.into_iter();
        while let Some(client) = clients.next() {
            if client.flutter_disconnect_and_wait().await.is_err() {
                let mut held = self
                    .retiring
                    .lock()
                    .map_err(|_| RuntimeError::quiescence())?;
                held.push(client);
                held.extend(clients);
                return Err(RuntimeError::quiescence());
            }
        }
        Ok(())
    }

    fn retire(self: &Arc<Self>) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        state.retired = true;
        let client = state.advance_generation();
        state.queue.clear();
        state.outstanding.clear();
        drop(state);
        self.cleanup(client);
    }
}

impl State {
    fn push(&mut self, outbound: Outbound) {
        self.sequence = self.sequence.saturating_add(1);
        self.queue.push_back(Envelope {
            generation: self.generation,
            sequence: self.sequence,
            outbound,
        });
    }

    fn push_result(&mut self, request_id: u64, result: Result<Output, RuntimeError>) {
        self.push(Outbound::Result {
            request_id,
            result: result.map(Box::new),
        });
    }

    fn advance_generation(&mut self) -> Option<Arc<WaddleClient>> {
        self.queue
            .retain(|frame| matches!(&frame.outbound, Outbound::Result { .. }));
        for frame in &mut self.queue {
            if let Outbound::Result { result, .. } = &mut frame.outbound {
                *result = Err(RuntimeError::retired());
            }
        }
        self.accepts_events = false;
        self.connected = false;
        let pending = std::mem::take(&mut self.pending);
        for (request_id, pending) in pending {
            if !pending.connecting {
                if let Some(abort) = pending.abort {
                    abort.abort();
                }
            }
            self.push_result(request_id, Err(RuntimeError::retired()));
        }
        self.generation = self.generation.saturating_add(1);
        self.timeline = TimelineStore::new();
        self.client.take()
    }

    fn overflow(&mut self) -> Option<Arc<WaddleClient>> {
        self.accepts_events = false;
        self.connected = false;
        self.queue.clear();
        let pending = std::mem::take(&mut self.pending);
        for (_, pending) in pending {
            if !pending.connecting {
                if let Some(abort) = pending.abort {
                    abort.abort();
                }
            }
        }
        // Completed-but-unpolled replies still own Dart futures. Settle those too.
        let outstanding: Vec<u64> = self.outstanding.iter().copied().collect();
        for request_id in outstanding {
            self.push_result(request_id, Err(RuntimeError::overflow()));
        }
        self.push(Outbound::Overflow);
        // Keep the client discoverable until cleanup has been registered. A
        // concurrent disconnect must still see it before cleanup is scheduled.
        self.client.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::{create, destroy, lookup, ClientRuntime, Pending, MAX_QUEUED};
    use crate::error::RuntimeError;
    use crate::events::Envelope;
    use crate::events::Outbound;
    use crate::generated::Output;
    use std::time::{Duration, Instant};
    use waddle_xmpp_client_ffi::WaddleClientEvent;

    fn poll_count(owner: &ClientRuntime, count: usize) -> Vec<Envelope> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut frames = Vec::new();
        while frames.len() < count {
            frames.extend(owner.poll());
            assert!(
                Instant::now() < deadline,
                "Expected native results before deadline"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        frames
    }

    #[test]
    fn typed_requests_fail_explicitly_without_a_session_or_a_supported_method() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        let first = owner.submit(br#"{"method":"ping_server","args":{}}"#);
        let second = owner.submit(br#"{"method":"send_raw_xml","args":{"xml":"secret"}}"#);
        assert!(first > 0 && second > first);
        let values = serde_json::to_value(owner.poll()).expect("serialize results");
        assert_eq!(values[0]["request_id"], first);
        assert_eq!(values[0]["error"]["code"], "not_connected");
        assert_eq!(values[1]["error"]["code"], "invalid_request");
        assert!(!values.to_string().contains("secret"));
        destroy(id);
    }

    #[test]
    fn retired_clients_remain_discoverable_when_lifecycle_tasks_have_not_run() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        // Freeze lifecycle workers to exercise submission order independently
        // of their eventual scheduling order. No connection may open here.
        let gate = owner.executor.block_on(owner.lifecycle.lock());
        let request = br#"{"method":"connect","args":{"server_url":"ws://127.0.0.1:1/xmpp","jid":"alice@example.test","access_token":"local-test-token","resource":"flutter"}}"#;
        owner.submit(request);
        let original = owner
            .state
            .lock()
            .expect("state")
            .client
            .clone()
            .expect("client");
        owner.submit(request);
        owner.submit(br#"{"method":"disconnect","args":{}}"#);
        let clients = owner.retiring.lock().expect("retired clients");
        assert_eq!(clients.len(), 2);
        assert!(clients
            .iter()
            .any(|client| std::sync::Arc::ptr_eq(client, &original)));
        drop(clients);
        assert!(owner.state.lock().expect("state").client.is_none());
        drop(gate);
        let frames = serde_json::to_value(poll_count(&owner, 3)).expect("results");
        assert_eq!(frames[2]["ok"], true);
        destroy(id);
    }

    #[test]
    fn unsupported_resumption_rejects_before_a_transport_or_credentials_escape() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        let request = br#"{"method":"connect","args":{"server_url":"wss://xmpp.example.test/xmpp","jid":"alice@example.test","access_token":"do-not-log-this-token","resource":"flutter","resumable":true}}"#;
        assert!(owner.submit(request) > 0);
        let values = serde_json::to_value(owner.poll()).expect("serialize failure");
        assert_eq!(values[0]["error"]["code"], "invalid_request");
        assert!(!values.to_string().contains("do-not-log-this-token"));
        assert!(owner.state.lock().expect("lock").client.is_none());
        destroy(id);
    }

    #[test]
    fn retired_generation_cannot_publish_events_or_finish_another_accounts_request() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        {
            let mut state = owner.state.lock().expect("lock");
            state.generation = 2;
            state.accepts_events = true;
            state.outstanding.insert(7);
            state.pending.insert(
                7,
                Pending {
                    abort: None,
                    connecting: false,
                },
            );
        }
        owner.event(1, Outbound::Event(Box::new(WaddleClientEvent::Connected)));
        owner.complete(1, 7, Ok(Output::Unit));
        assert!(owner.poll().is_empty());
        assert!(owner.state.lock().expect("lock").pending.contains_key(&7));
        owner.complete(2, 7, Err(RuntimeError::retired()));
        let values = serde_json::to_value(owner.poll()).expect("serialize");
        assert_eq!(values[0]["generation"], 2);
        assert_eq!(values[0]["error"]["code"], "session_changed");
        destroy(id);
        owner.event(2, Outbound::Event(Box::new(WaddleClientEvent::Connected)));
        assert!(owner.poll().is_empty());
        assert!(lookup(id).is_none());
    }

    #[test]
    fn replacing_a_generation_settles_pending_requests_and_discards_late_completions() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        {
            let mut state = owner.state.lock().expect("lock");
            state.outstanding.insert(5);
            state.pending.insert(
                5,
                Pending {
                    abort: None,
                    connecting: true,
                },
            );
        }
        let disconnect = owner.submit(br#"{"method":"disconnect","args":{}}"#);
        owner.complete(0, 5, Ok(Output::Unit));
        let values = serde_json::to_value(poll_count(&owner, 2)).expect("serialize");
        assert_eq!(values.as_array().expect("array").len(), 2);
        assert_eq!(values[0]["request_id"], 5);
        assert_eq!(values[0]["error"]["code"], "session_changed");
        assert_eq!(values[1]["request_id"], disconnect);
        assert_eq!(values[1]["generation"], 1);
        destroy(id);
    }

    #[test]
    fn overflow_explicitly_requests_resync_and_settles_pending_futures() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        {
            let mut state = owner.state.lock().expect("lock");
            state.accepts_events = true;
            state.outstanding.insert(9);
            state.pending.insert(
                9,
                Pending {
                    abort: None,
                    connecting: false,
                },
            );
        }
        for _ in 0..=MAX_QUEUED {
            owner.event(0, Outbound::Event(Box::new(WaddleClientEvent::Connected)));
        }
        // The overflowing generation can emit nothing further until a fresh connect.
        owner.event(0, Outbound::Event(Box::new(WaddleClientEvent::Connected)));
        let values = serde_json::to_value(owner.poll()).expect("serialize");
        let values = values.as_array().expect("array");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["request_id"], 9);
        assert_eq!(values[0]["error"]["code"], "resync_required");
        assert_eq!(values[1]["event"]["data"]["code"], "resync_required");
        destroy(id);
    }

    #[test]
    fn generation_change_discards_queued_protocol_events_and_retires_queued_success() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        owner.state.lock().expect("lock").accepts_events = true;
        owner.event(0, Outbound::Event(Box::new(WaddleClientEvent::Connected)));
        let query =
            owner.submit(br#"{"method":"parse_jid","args":{"input":"alice@example.test"}}"#);
        let disconnect = owner.submit(br#"{"method":"disconnect","args":{}}"#);
        let values = serde_json::to_value(poll_count(&owner, 2)).expect("serialize");
        let values = values.as_array().expect("array");
        assert_eq!(values.len(), 2);
        assert_eq!(values[0]["request_id"], query);
        assert_eq!(values[0]["error"]["code"], "session_changed");
        assert_eq!(values[1]["request_id"], disconnect);
        assert_eq!(values[1]["ok"], true);
        destroy(id);
    }

    #[test]
    fn overflow_settles_already_completed_but_unpolled_futures_too() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        let query =
            owner.submit(br#"{"method":"parse_jid","args":{"input":"alice@example.test"}}"#);
        owner.state.lock().expect("lock").accepts_events = true;
        for _ in 0..=MAX_QUEUED {
            owner.event(0, Outbound::Event(Box::new(WaddleClientEvent::Connected)));
        }
        let values = serde_json::to_value(owner.poll()).expect("serialize");
        assert_eq!(values[0]["request_id"], query);
        assert_eq!(values[0]["error"]["code"], "resync_required");
        assert_eq!(values[1]["event"]["data"]["code"], "resync_required");
        destroy(id);
    }

    #[test]
    fn the_request_window_counts_unpolled_completions_until_the_consumer_drains_them() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        for _ in 0..super::MAX_PENDING {
            assert!(
                owner.submit(br#"{"method":"parse_jid","args":{"input":"alice@example.test"}}"#)
                    > 0
            );
        }
        assert_eq!(
            owner.submit(br#"{"method":"parse_jid","args":{"input":"alice@example.test"}}"#),
            0
        );
        assert_eq!(owner.poll().len(), super::MAX_PENDING);
        assert!(
            owner.submit(br#"{"method":"parse_jid","args":{"input":"alice@example.test"}}"#) > 0
        );
        destroy(id);
    }

    #[test]
    fn invalid_requests_are_bounded_even_if_the_consumer_never_polls() {
        let id = create();
        let owner = lookup(id).expect("runtime created");
        for _ in 0..MAX_QUEUED * 3 {
            owner.submit(br#"{"method":"unknown","args":{}}"#);
        }
        assert!(owner.state.lock().expect("lock").queue.len() <= MAX_QUEUED);
        destroy(id);
    }
}
