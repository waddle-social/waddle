//! Browser ownership proof for callers replacing an account/resource binding.

use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};

use futures::future::{select, Either};
use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

use crate::commands::disconnect_client;
use crate::helpers::js_error;
use crate::WaddleClientInner;

#[derive(Default)]
pub(crate) struct StartupGate {
    next: u64,
    pending: Option<u64>,
    retiring: bool,
}

impl StartupGate {
    fn admit(&mut self) -> Result<u64, &'static str> {
        if self.retiring {
            return Err("client transport retirement is in progress");
        }
        if self.pending.is_some() {
            return Err("client connection startup is in progress");
        }
        self.next = self.next.wrapping_add(1);
        if self.next == 0 {
            self.next = 1;
        }
        self.pending = Some(self.next);
        Ok(self.next)
    }

    fn accepts(&self, ticket: u64) -> bool {
        !self.retiring && self.pending == Some(ticket)
    }

    fn complete(&mut self, ticket: u64) {
        if self.pending == Some(ticket) {
            self.pending = None;
        }
    }
}

struct Witness {
    socket: web_sys::WebSocket,
    driver_retired: Rc<Cell<bool>>,
    events_retired: Rc<Cell<bool>>,
}

impl Witness {
    fn closed_and_retired(&self) -> bool {
        self.socket.ready_state() == web_sys::WebSocket::CLOSED
            && self.driver_retired.get()
            && self.events_retired.get()
    }
}

#[derive(Default)]
pub(crate) struct BrowserOwnership {
    gate: StartupGate,
    witnesses: Vec<Witness>,
}

pub(crate) struct ConnectionRetirements {
    pub(crate) driver: Rc<Cell<bool>>,
    pub(crate) events: Rc<Cell<bool>>,
}

/// Set only after the owning async task and its local transport values drop.
pub(crate) struct RetiredOnDrop(Rc<Cell<bool>>);

impl RetiredOnDrop {
    pub(crate) fn new(retired: Rc<Cell<bool>>) -> Self {
        Self(retired)
    }
}

impl Drop for RetiredOnDrop {
    fn drop(&mut self) {
        self.0.set(true);
    }
}

pub(crate) fn admit_connect(inner: &Rc<RefCell<WaddleClientInner>>) -> Result<u64, &'static str> {
    let mut borrowed = inner.borrow_mut();
    if borrowed.command_lane.is_some() {
        return Err("client is already connected");
    }
    borrowed
        .ownership
        .witnesses
        .retain(|witness| !witness.closed_and_retired());
    borrowed.ownership.gate.admit()
}

pub(crate) fn startup_allowed(inner: &Rc<RefCell<WaddleClientInner>>, ticket: u64) -> bool {
    inner.borrow().ownership.gate.accepts(ticket)
}

pub(crate) fn complete_startup(inner: &Rc<RefCell<WaddleClientInner>>, ticket: u64) {
    inner.borrow_mut().ownership.gate.complete(ticket);
}

pub(crate) fn track_connection(
    inner: &Rc<RefCell<WaddleClientInner>>,
    socket: web_sys::WebSocket,
) -> ConnectionRetirements {
    let driver = Rc::new(Cell::new(false));
    let events = Rc::new(Cell::new(false));
    inner.borrow_mut().ownership.witnesses.push(Witness {
        socket,
        driver_retired: driver.clone(),
        events_retired: events.clone(),
    });
    ConnectionRetirements { driver, events }
}

pub(crate) fn begin_retirement(inner: &Rc<RefCell<WaddleClientInner>>) {
    inner.borrow_mut().ownership.gate.retiring = true;
}

/// Resolve only when every socket is physically CLOSED and both Rust tasks
/// have retired. Timeout leaves ownership fenced and retained for a later retry.
pub(crate) async fn retire_and_wait(inner: Rc<RefCell<WaddleClientInner>>) -> Result<(), JsValue> {
    let deadline = BrowserTimer::new(12_000)?;
    let result = match select(Box::pin(close_owned(inner.clone())), deadline).await {
        Either::Left((result, _)) => result,
        Either::Right((Err(error), _)) => Err(error),
        Either::Right((Ok(()), _)) => Err(js_error(
            "browser transport closure is not yet proven; retry closing",
        )),
    };
    if result.is_err() {
        request_physical_close(&inner);
    }
    result
}

async fn close_owned(inner: Rc<RefCell<WaddleClientInner>>) -> Result<(), JsValue> {
    // A connect Promise admitted before retirement must either finish startup
    // (and register its socket witness) or observe the fence and cancel startup.
    while inner.borrow().ownership.gate.pending.is_some() {
        BrowserTimer::new(20)?.await?;
    }
    let grace = BrowserTimer::new(1_000)?;
    let _ = select(Box::pin(disconnect_client(inner.clone())), grace).await;
    request_physical_close(&inner);
    loop {
        let proven = {
            let borrowed = inner.borrow();
            borrowed.ownership.gate.pending.is_none()
                && borrowed
                    .ownership
                    .witnesses
                    .iter()
                    .all(Witness::closed_and_retired)
        };
        if proven {
            let mut borrowed = inner.borrow_mut();
            borrowed.ownership.witnesses.clear();
            borrowed.ownership.gate.retiring = false;
            borrowed.command_lane = None;
            borrowed.driver_core = None;
            borrowed.resume_state = None;
            return Ok(());
        }
        BrowserTimer::new(20)?.await?;
    }
}

fn request_physical_close(inner: &Rc<RefCell<WaddleClientInner>>) {
    for witness in &inner.borrow().ownership.witnesses {
        if witness.socket.ready_state() != web_sys::WebSocket::CLOSED {
            let _ = witness.socket.close();
        }
    }
}

/// Cancellable timers avoid leaked deadline callbacks and reject immediately
/// when the host cannot schedule one, rather than silently waiting forever.
struct BrowserTimer {
    future: JsFuture,
    clear: Function,
    id: Option<JsValue>,
}

impl BrowserTimer {
    fn new(milliseconds: u32) -> Result<Self, JsValue> {
        let global = js_sys::global();
        let set = Reflect::get(&global, &JsValue::from_str("setTimeout"))?
            .dyn_into::<Function>()
            .map_err(|_| js_error("browser timer is unavailable"))?;
        let clear = Reflect::get(&global, &JsValue::from_str("clearTimeout"))?
            .dyn_into::<Function>()
            .map_err(|_| js_error("browser timer cancellation is unavailable"))?;
        let mut id = None;
        let promise = Promise::new(&mut |resolve, reject| match set.call2(
            &global,
            &resolve,
            &JsValue::from_f64(f64::from(milliseconds)),
        ) {
            Ok(scheduled) => id = Some(scheduled),
            Err(error) => {
                let _ = reject.call1(&JsValue::NULL, &error);
            }
        });
        Ok(Self {
            future: JsFuture::from(promise),
            clear,
            id,
        })
    }
}

impl Future for BrowserTimer {
    type Output = Result<(), JsValue>;
    fn poll(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.future)
            .poll(context)
            .map(|result| result.map(|_| ()))
    }
}

impl Drop for BrowserTimer {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            let _ = self.clear.call1(&js_sys::global(), id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retirement_fences_an_admitted_but_not_yet_polled_connect() {
        let mut gate = StartupGate::default();
        let ticket = gate.admit().expect("startup");
        gate.retiring = true;
        assert!(!gate.accepts(ticket));
        assert!(gate.admit().is_err());
        gate.complete(ticket);
        assert!(gate.pending.is_none());
        assert!(
            gate.admit().is_err(),
            "unproven retirement remains fenced for retry"
        );
        gate.retiring = false;
        let next = gate.admit().expect("new startup after proof");
        assert_ne!(next, ticket);
        assert!(!gate.accepts(ticket));
        assert!(gate.accepts(next));
    }

    #[test]
    fn startup_admission_and_task_retirement_have_single_owners() {
        let mut gate = StartupGate::default();
        let ticket = gate.admit().expect("startup");
        assert!(gate.admit().is_err());
        gate.complete(ticket.wrapping_add(1));
        assert!(
            gate.pending.is_some(),
            "stale completion cannot clear a successor"
        );
        gate.complete(ticket);
        let retired = Rc::new(Cell::new(false));
        {
            let _guard = RetiredOnDrop::new(retired.clone());
            assert!(!retired.get());
        }
        assert!(retired.get());
    }
}
