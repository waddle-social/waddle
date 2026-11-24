//! Voice calls: the header button, the call window, the incoming-call
//! notification and the glue between the call engine (`crate::call_engine`)
//! and the session and media layers.
//!
//! The rules are in `call.rs`, `call_engine.rs` and `call_screen.rs`; this
//! module only carries out the engine's effects, in order, and shows the
//! slot. Nothing here decides what a call does.

use std::collections::VecDeque;
use std::time::Instant;

use adw::prelude::*;
use adw::subclass::prelude::ObjectSubclassIsExt;
use gtk::{gio, glib};
use jid::BareJid;

use super::{avatar, WaddleWindow};
use crate::call::{CallEvent, CallState, JingleReason};
use crate::call_engine::{CallEngine, Effect, Timer, Wire};
use crate::call_media::{self, AudioDevice, MediaEvent, MediaSession};
use crate::call_screen::{self, Button, MediaPhase};
use crate::session::{Kind, Session};

/// Everything about the call in progress that is not the engine's.
#[derive(Default)]
pub struct CallsState {
    engine: Option<CallEngine>,
    media: Option<MediaSession>,
    media_events: Option<async_channel::Sender<MediaEvent>>,
    phase: MediaPhase,
    muted: bool,
    /// When the media came up: the call's clock starts here.
    connected_at: Option<Instant>,
    /// Who the window is about, kept while the ended banner shows.
    peer: Option<BareJid>,
    /// The ringing call already announced by notification.
    announced: Option<String>,
    /// The call whose window was last brought to the front.
    presented: Option<String>,
    screen: Option<CallScreen>,
    /// What this machine offers, once the room is joined.
    devices: Option<(Vec<AudioDevice>, Vec<AudioDevice>)>,
    queue: VecDeque<Effect>,
    draining: bool,
    ticking: bool,
}

/// The call window's widgets.
struct CallScreen {
    window: adw::Window,
    avatar: adw::Avatar,
    name: gtk::Label,
    status: gtk::Label,
    buttons: gtk::Box,
    shown: Vec<Button>,
    controls: gtk::Box,
    microphones: gtk::DropDown,
    speakers: gtk::DropDown,
    /// The dropdowns are being filled, not chosen from.
    filling: bool,
}

fn new_sid() -> String {
    format!("c{}", &uuid::Uuid::new_v4().simple().to_string()[..16])
}

fn notification_id(sid: &str) -> String {
    format!("call-{sid}")
}

impl WaddleWindow {
    /// The header button's `win.start-call`.
    pub(super) fn setup_calls(&self) {
        let action = gio::SimpleAction::new("start-call", None);
        let window = self.downgrade();
        action.connect_activate(move |_, _| {
            if let Some(window) = window.upgrade() {
                window.start_call_here();
            }
        });
        action.set_enabled(false);
        self.add_action(&action);
    }

    /// A signed-in session begins: a fresh slot, and the media stream.
    pub(super) fn begin_calls(&self, session: &Session, generation: u64) {
        let imp = self.imp();
        let Some(own) = session.own_full_jid() else {
            tracing::warn!("calls are off: this device has no valid address");
            return;
        };
        let (sender, events) = async_channel::unbounded();
        imp.calls.replace(CallsState {
            engine: Some(CallEngine::new(own)),
            media_events: Some(sender),
            ..CallsState::default()
        });
        let window = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(event) = events.recv().await {
                if window.imp().generation.get() != generation {
                    break;
                }
                window.on_media_event(event);
            }
        });
        self.refresh_call_ui();
    }

    /// The session ended: drop the call without telling anyone, release
    /// the audio devices and close the window.
    pub(super) fn end_calls(&self) {
        let imp = self.imp();
        let sid = imp
            .calls
            .borrow()
            .engine
            .as_ref()
            .and_then(|engine| engine.state().sid().map(str::to_string));
        let state = imp.calls.take();
        if let Some(screen) = state.screen {
            screen.window.destroy();
        }
        if let (Some(app), Some(sid)) = (self.application(), sid) {
            app.withdraw_notification(&notification_id(&sid));
        }
        self.refresh_call_ui();
    }

    fn session(&self) -> Option<Session> {
        self.imp().session.borrow().clone()
    }

    /// Run `act` on the engine, then carry out what it asks for.
    fn with_engine<T>(&self, act: impl FnOnce(&mut CallEngine) -> T) -> Option<T> {
        let mut calls = self.imp().calls.borrow_mut();
        calls.engine.as_mut().map(act)
    }

    fn call_state(&self) -> CallState {
        self.imp()
            .calls
            .borrow()
            .engine
            .as_ref()
            .map(|engine| engine.state().clone())
            .unwrap_or_default()
    }

    // ── What the person does ────────────────────────────────────────────

    fn start_call_here(&self) {
        let Some(conversation) = self
            .current_conversation()
            .filter(|conversation| conversation.kind == Kind::Direct)
        else {
            return;
        };
        let Ok(peer) = conversation.jid.parse::<BareJid>() else {
            return;
        };
        let Some(effects) = self.with_engine(|engine| engine.start_call(peer.clone(), new_sid()))
        else {
            return;
        };
        let Some(effects) = effects else {
            return self.toast("You are already on a call.");
        };
        {
            let mut calls = self.imp().calls.borrow_mut();
            calls.peer = Some(peer);
            calls.muted = false;
        }
        self.run_effects(effects);
    }

    pub fn answer_call(&self, sid: &str) {
        if self.call_state().sid() != Some(sid) {
            return;
        }
        self.present();
        let effects = self.with_engine(CallEngine::accept).unwrap_or_default();
        self.run_effects(effects);
    }

    pub fn decline_call(&self, sid: &str) {
        if self.call_state().sid() == Some(sid) {
            self.leave_call();
        }
    }

    /// A notification's default action: bring the call up.
    pub fn show_call(&self) {
        if let Some(screen) = self.imp().calls.borrow().screen.as_ref() {
            screen.window.present();
        }
    }

    /// Whatever the slot holds ends: a ringing call is declined, one in
    /// progress is hung up, a banner is dismissed.
    fn leave_call(&self) {
        let effects = self
            .with_engine(|engine| match engine.state() {
                CallState::Incoming(_) => engine.decline(),
                CallState::Outgoing(_) | CallState::Active(_) => engine.hang_up(),
                CallState::Ended { .. } => {
                    engine.dismiss();
                    Vec::new()
                }
                CallState::Idle => Vec::new(),
            })
            .unwrap_or_default();
        self.run_effects(effects);
    }

    fn set_call_muted(&self, muted: bool) {
        let mut calls = self.imp().calls.borrow_mut();
        calls.muted = muted;
        if let Some(media) = calls.media.as_ref() {
            media.set_muted(muted);
        }
    }

    // ── What arrives ────────────────────────────────────────────────────

    pub(super) fn on_call_event(&self, event: &CallEvent) {
        let before = self.call_state();
        let Some(effects) = self.with_engine(|engine| engine.handle(event)) else {
            return;
        };
        let after = self.call_state();
        if let (false, CallState::Incoming(call)) = (before.is_incoming(), &after) {
            let mut calls = self.imp().calls.borrow_mut();
            calls.peer = Some(call.from.to_bare());
            calls.muted = false;
        }
        self.run_effects(effects);
    }

    fn on_media_event(&self, event: MediaEvent) {
        let sid = match &event {
            MediaEvent::Connected { sid }
            | MediaEvent::Failed { sid }
            | MediaEvent::Reconnecting { sid }
            | MediaEvent::Reconnected { sid }
            | MediaEvent::Lost { sid }
            | MediaEvent::Devices { sid, .. } => sid.clone(),
        };
        // A late word from the media of a call that is over.
        if !matches!(self.call_state(), CallState::Active(call) if call.sid == sid) {
            return;
        }
        let imp = self.imp();
        match event {
            MediaEvent::Connected { .. } | MediaEvent::Reconnected { .. } => {
                let mut calls = imp.calls.borrow_mut();
                calls.phase = MediaPhase::Connected;
                calls.connected_at.get_or_insert_with(Instant::now);
            }
            MediaEvent::Reconnecting { .. } => {
                imp.calls.borrow_mut().phase = MediaPhase::Reconnecting
            }
            MediaEvent::Devices {
                inputs, outputs, ..
            } => {
                imp.calls.borrow_mut().devices = Some((inputs, outputs));
                self.fill_devices();
            }
            // Joining failed, or the room ended under us: nobody chose
            // this, so the call is shown as ended.
            MediaEvent::Failed { .. } => {
                let effects = self
                    .with_engine(|engine| engine.media_ended(&sid, JingleReason::Gone))
                    .unwrap_or_default();
                return self.run_effects(effects);
            }
            MediaEvent::Lost { .. } => {
                let effects = self
                    .with_engine(|engine| engine.media_ended(&sid, JingleReason::ConnectivityError))
                    .unwrap_or_default();
                return self.run_effects(effects);
            }
        }
        self.refresh_call_ui();
    }

    fn on_call_timer(&self, timer: Timer, token: u64) {
        let effects = self
            .with_engine(|engine| engine.timer_fired(timer, token))
            .unwrap_or_default();
        self.run_effects(effects);
    }

    // ── Carrying out the engine's effects ───────────────────────────────

    /// Queue effects behind any still running, so one answer's sends never
    /// overtake the previous event's.
    fn run_effects(&self, effects: Vec<Effect>) {
        self.refresh_call_ui();
        let imp = self.imp();
        {
            let mut calls = imp.calls.borrow_mut();
            calls.queue.extend(effects);
            if calls.draining || calls.queue.is_empty() {
                return;
            }
            calls.draining = true;
        }
        let window = self.clone();
        let generation = imp.generation.get();
        glib::spawn_future_local(async move {
            loop {
                let next = {
                    let imp = window.imp();
                    // A newer session owns the slot now, and its own drain.
                    if imp.generation.get() != generation {
                        break;
                    }
                    let mut calls = imp.calls.borrow_mut();
                    let next = calls.queue.pop_front();
                    if next.is_none() {
                        calls.draining = false;
                    }
                    next
                };
                let Some(effect) = next else { break };
                let follow = window.carry_out(effect).await;
                if !follow.is_empty() {
                    window.imp().calls.borrow_mut().queue.extend(follow);
                }
                window.refresh_call_ui();
            }
        });
    }

    /// One effect, and what the engine asks for as a result.
    async fn carry_out(&self, effect: Effect) -> Vec<Effect> {
        let Some(session) = self.session() else {
            return Vec::new();
        };
        match effect {
            Effect::Send(wire) => {
                let ok = session.send_call(wire.clone()).await;
                self.with_engine(|engine| engine.sent(&wire, ok))
                    .unwrap_or_default()
            }
            Effect::RingIfKnown { to, sid } => {
                if !session.peer_receives_our_presence(to.to_bare()).await {
                    return Vec::new();
                }
                // Answered, declined or withdrawn while we looked.
                let still_ringing = matches!(
                    self.call_state(),
                    CallState::Incoming(call) if call.sid == sid && !call.accepting
                );
                if still_ringing {
                    session.send_call(Wire::Ringing { to, sid }).await;
                }
                Vec::new()
            }
            Effect::Terminate {
                peer,
                sid,
                reason,
                finish_reason,
                finish_after_error,
            } => {
                use crate::session_calls::Terminated;
                let outcome = session
                    .terminate_call(peer.clone(), sid.clone(), reason)
                    .await;
                if outcome == Terminated::Error {
                    self.with_engine(CallEngine::terminate_failed);
                    if !finish_after_error {
                        return Vec::new();
                    }
                }
                let wire = Wire::Finish {
                    to: peer,
                    sid,
                    reason: finish_reason,
                };
                let ok = session.send_call(wire.clone()).await;
                self.with_engine(|engine| engine.sent(&wire, ok))
                    .unwrap_or_default()
            }
            Effect::StartTimer {
                timer,
                token,
                after,
            } => {
                let window = self.clone();
                glib::timeout_add_local_once(after, move || window.on_call_timer(timer, token));
                Vec::new()
            }
            Effect::ConnectMedia { sid, join, media } => {
                let window = self.clone();
                let generation = self.imp().generation.get();
                // The lookup must not hold up the signalling behind it.
                glib::spawn_future_local(async move {
                    let ice = call_media::ice_servers(&session.ice_services().await);
                    if window.imp().generation.get() != generation
                        || !matches!(window.call_state(), CallState::Active(call) if call.sid == sid)
                    {
                        return;
                    }
                    let imp = window.imp();
                    let mut calls = imp.calls.borrow_mut();
                    let Some(events) = calls.media_events.clone() else {
                        return;
                    };
                    let muted = calls.muted;
                    calls.phase = MediaPhase::Connecting;
                    calls.media = Some(MediaSession::start(sid, join, media, ice, muted, events));
                });
                Vec::new()
            }
            Effect::DisconnectMedia => {
                let mut calls = self.imp().calls.borrow_mut();
                calls.media = None;
                calls.phase = MediaPhase::Connecting;
                calls.connected_at = None;
                calls.devices = None;
                calls.muted = false;
                Vec::new()
            }
        }
    }

    // ── What is shown ───────────────────────────────────────────────────

    /// The header button: for a one-to-one conversation, when the slot is
    /// free and the session is up.
    pub(super) fn refresh_call_button(&self) {
        let imp = self.imp();
        let direct = self.current_conversation().is_some_and(|conversation| {
            conversation.kind == Kind::Direct && conversation.thread.is_none()
        });
        let has_engine = imp.calls.borrow().engine.is_some();
        imp.call_button.set_visible(direct && has_engine);
        let free = !self.call_state().is_live();
        if let Some(action) = self
            .lookup_action("start-call")
            .and_then(|action| action.downcast::<gio::SimpleAction>().ok())
        {
            action.set_enabled(free && imp.connected.get());
        }
    }

    fn peer_name(&self, peer: &BareJid) -> String {
        let jid = peer.to_string();
        self.imp()
            .conversations
            .borrow()
            .iter()
            .find(|conversation| conversation.kind == Kind::Direct && conversation.jid == jid)
            .map(|conversation| conversation.name.clone())
            .unwrap_or(jid)
    }

    /// Bring the window, the notification and the header button in line
    /// with the slot.
    pub(super) fn refresh_call_ui(&self) {
        self.refresh_call_button();
        let state = self.call_state();
        let imp = self.imp();
        if matches!(state, CallState::Idle) {
            let screen = imp.calls.borrow_mut().screen.take();
            if let Some(screen) = screen {
                screen.window.destroy();
            }
            return;
        }
        if let (CallState::Incoming(call), false) = (&state, self.is_active()) {
            let announced = imp.calls.borrow().announced.as_deref() == Some(call.sid.as_str());
            if !call.accepting && !announced {
                imp.calls.borrow_mut().announced = Some(call.sid.clone());
                self.announce_call(call.sid.clone(), &call.from.to_bare());
            }
        }
        if let Some(app) = self.application() {
            match &state {
                CallState::Incoming(call) if !call.accepting => {}
                other => {
                    if let Some(sid) = other.sid() {
                        app.withdraw_notification(&notification_id(sid));
                    }
                }
            }
        }
        let peer = {
            let calls = imp.calls.borrow();
            calls.peer.clone()
        };
        let Some(peer) = peer.or_else(|| match &state {
            CallState::Incoming(call) => Some(call.from.to_bare()),
            CallState::Outgoing(call) => Some(call.to.clone()),
            CallState::Active(call) => Some(call.peer.to_bare()),
            _ => None,
        }) else {
            return;
        };
        let name = self.peer_name(&peer);
        let fresh = imp.calls.borrow().screen.is_none();
        if fresh {
            let screen = self.build_call_screen(&peer, &name);
            imp.calls.borrow_mut().screen = Some(screen);
            self.fill_devices();
        }
        let (phase, elapsed, error) = {
            let calls = imp.calls.borrow();
            (
                calls.phase,
                calls
                    .connected_at
                    .map_or(0, |since| since.elapsed().as_secs()),
                calls.engine.as_ref().and_then(CallEngine::last_error),
            )
        };
        let shown = call_screen::screen(&state, phase, elapsed, error);
        let rebuild = {
            let mut calls = imp.calls.borrow_mut();
            let Some(screen) = calls.screen.as_mut() else {
                return;
            };
            screen.name.set_label(&name);
            avatar::set_name(&screen.avatar, &name);
            screen.status.set_label(&shown.status);
            screen.controls.set_visible(shown.controls);
            if screen.shown == shown.buttons {
                None
            } else {
                screen.shown.clone_from(&shown.buttons);
                Some(screen.buttons.clone())
            }
        };
        if fresh {
            if let Some(screen) = imp.calls.borrow().screen.as_ref() {
                self.watch_avatar(&screen.avatar, &peer.to_string());
            }
        }
        // A new call comes to the front, even over a banner still showing
        // the last one.
        let sid = state.sid().map(str::to_string);
        let new_call = imp.calls.borrow().presented != sid;
        if fresh || new_call {
            let mut calls = imp.calls.borrow_mut();
            calls.presented = sid;
            if let Some(screen) = calls.screen.as_ref() {
                screen.window.present();
            }
        }
        if let Some(row) = rebuild {
            self.fill_buttons(&row, &shown.buttons);
        }
        self.keep_clock(&state);
    }

    /// Tick once a second while the call is up.
    fn keep_clock(&self, state: &CallState) {
        let imp = self.imp();
        let running = matches!(state, CallState::Active(_));
        {
            let mut calls = imp.calls.borrow_mut();
            if !running || calls.ticking {
                return;
            }
            calls.ticking = true;
        }
        let window = self.downgrade();
        glib::timeout_add_seconds_local(1, move || {
            let Some(window) = window.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if matches!(window.call_state(), CallState::Active(_)) {
                window.refresh_call_ui();
                glib::ControlFlow::Continue
            } else {
                window.imp().calls.borrow_mut().ticking = false;
                glib::ControlFlow::Break
            }
        });
    }

    fn announce_call(&self, sid: String, from: &BareJid) {
        let Some(app) = self.application() else {
            return;
        };
        let name = self.peer_name(from);
        let notification = gio::Notification::new(&format!("{name} is calling"));
        notification.set_body(Some("Waddle voice call"));
        notification.set_priority(gio::NotificationPriority::Urgent);
        notification.set_category(Some("call.incoming"));
        let target = sid.to_variant();
        notification.set_default_action("app.show-call");
        notification.add_button_with_target_value("Answer", "app.answer-call", Some(&target));
        notification.add_button_with_target_value("Decline", "app.decline-call", Some(&target));
        // The same id replaces the previous one, so a re-render does not
        // stack notifications.
        app.send_notification(Some(&notification_id(&sid)), &notification);
    }

    fn fill_buttons(&self, row: &gtk::Box, buttons: &[Button]) {
        while let Some(child) = row.first_child() {
            row.remove(&child);
        }
        for kind in buttons {
            let (label, css) = match kind {
                Button::Answer => ("Answer", "suggested-action"),
                Button::Decline => ("Decline", "destructive-action"),
                Button::Cancel => ("Cancel", "destructive-action"),
                Button::HangUp => ("Hang Up", "destructive-action"),
                Button::Close => ("Close", "suggested-action"),
            };
            let button = gtk::Button::with_label(label);
            button.add_css_class("pill");
            button.add_css_class(css);
            let this = self.downgrade();
            let kind = *kind;
            button.connect_clicked(move |_| {
                let Some(this) = this.upgrade() else {
                    return;
                };
                match kind {
                    Button::Answer => {
                        if let Some(sid) = this.call_state().sid().map(str::to_string) {
                            this.answer_call(&sid);
                        }
                    }
                    Button::Decline | Button::Cancel | Button::HangUp | Button::Close => {
                        this.leave_call();
                    }
                }
            });
            row.append(&button);
        }
    }

    fn build_call_screen(&self, peer: &BareJid, name: &str) -> CallScreen {
        let window = adw::Window::builder()
            .title("Voice Call")
            .default_width(360)
            .default_height(480)
            .build();
        window.set_transient_for(Some(self));
        let picture = avatar::avatar(128);
        avatar::set_name(&picture, name);
        let title = gtk::Label::builder()
            .label(name)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .build();
        title.add_css_class("title-1");
        let status = gtk::Label::builder().label("").build();
        status.add_css_class("title-4");
        status.add_css_class("dim-label");
        status.set_accessible_role(gtk::AccessibleRole::Status);
        let buttons = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();

        let mute = gtk::ToggleButton::builder()
            .icon_name("microphone-sensitivity-muted-symbolic")
            .tooltip_text("Mute")
            .build();
        mute.add_css_class("circular");
        mute.update_property(&[gtk::accessible::Property::Label("Mute microphone")]);
        let this = self.downgrade();
        mute.connect_toggled(move |button| {
            if let Some(this) = this.upgrade() {
                this.set_call_muted(button.is_active());
            }
        });

        let microphones = gtk::DropDown::from_strings(&[]);
        let speakers = gtk::DropDown::from_strings(&[]);
        for (dropdown, choose) in [(&microphones, true), (&speakers, false)] {
            let this = self.downgrade();
            dropdown.connect_selected_notify(move |dropdown| {
                if let Some(this) = this.upgrade() {
                    this.choose_device(choose, dropdown.selected() as usize);
                }
            });
        }
        let devices = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .build();
        for (caption, dropdown) in [("Microphone", &microphones), ("Speakers", &speakers)] {
            let heading = gtk::Label::builder()
                .label(caption)
                .halign(gtk::Align::Start)
                .build();
            super::a11y::labelled_by(dropdown, &heading);
            devices.append(&heading);
            devices.append(dropdown);
        }
        let device_button = gtk::MenuButton::builder()
            .icon_name("audio-card-symbolic")
            .tooltip_text("Audio Devices")
            .popover(&gtk::Popover::builder().child(&devices).build())
            .build();
        super::a11y::label(&device_button, "Audio devices");
        device_button.add_css_class("circular");

        let controls = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        controls.append(&mute);
        controls.append(&device_button);

        let column = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(18)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(24)
            .margin_end(24)
            .build();
        for child in [
            picture.upcast_ref::<gtk::Widget>(),
            title.upcast_ref(),
            status.upcast_ref(),
            controls.upcast_ref(),
            buttons.upcast_ref(),
        ] {
            column.append(child);
        }
        let header = adw::HeaderBar::builder().show_title(false).build();
        header.add_css_class("flat");
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&header);
        toolbar.set_content(Some(&column));
        window.set_content(Some(&toolbar));

        // Closing the window ends the call it shows.
        let this = self.downgrade();
        window.connect_close_request(move |_| {
            if let Some(this) = this.upgrade() {
                // The window is going away; forget it first so the
                // refresh after the engine moves does not rebuild it.
                this.imp().calls.borrow_mut().screen = None;
                this.leave_call();
            }
            glib::Propagation::Proceed
        });
        tracing::debug!(%peer, "call window opened");
        CallScreen {
            window,
            avatar: picture,
            name: title,
            status,
            buttons,
            shown: Vec::new(),
            controls,
            microphones,
            speakers,
            filling: false,
        }
    }

    /// List what the room's audio layer found. The dropdowns announce a
    /// selection while they are filled, so no borrow is held then.
    fn fill_devices(&self) {
        let imp = self.imp();
        let (inputs, outputs, microphones, speakers) = {
            let mut calls = imp.calls.borrow_mut();
            let Some((inputs, outputs)) = calls.devices.clone() else {
                return;
            };
            let Some(screen) = calls.screen.as_mut() else {
                return;
            };
            screen.filling = true;
            (
                inputs,
                outputs,
                screen.microphones.clone(),
                screen.speakers.clone(),
            )
        };
        for (dropdown, devices) in [(&microphones, &inputs), (&speakers, &outputs)] {
            let names: Vec<&str> = devices.iter().map(|device| device.name.as_str()).collect();
            dropdown.set_model(Some(&gtk::StringList::new(&names)));
            dropdown.set_sensitive(!devices.is_empty());
        }
        if let Some(screen) = imp.calls.borrow_mut().screen.as_mut() {
            screen.filling = false;
        }
    }

    /// The person picked a microphone or speakers from the list.
    fn choose_device(&self, microphone: bool, index: usize) {
        let calls = self.imp().calls.borrow();
        if calls.screen.as_ref().is_none_or(|screen| screen.filling) {
            return;
        }
        let Some((inputs, outputs)) = calls.devices.as_ref() else {
            return;
        };
        let Some(media) = calls.media.as_ref() else {
            return;
        };
        if microphone {
            if let Some(device) = inputs.get(index) {
                media.select_input(device.id.clone());
            }
        } else if let Some(device) = outputs.get(index) {
            media.select_output(device.id.clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::call::{CallEventKind, CallMedia};

    fn settle() {
        let context = glib::MainContext::default();
        while context.iteration(false) {}
    }

    #[test]
    #[ignore = "requires a GTK display; run with xvfb-run and --ignored --test-threads=1"]
    fn the_call_window_follows_the_slot() {
        adw::init().expect("GTK display");
        let app = adw::Application::builder()
            .application_id("social.waddle.CallTest")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(None::<&gio::Cancellable>)
            .expect("register application");
        let window = WaddleWindow::new(&app);
        window.imp().calls.replace(CallsState {
            engine: Some(CallEngine::new(
                "alice@example.org/laptop".parse().expect("jid"),
            )),
            ..CallsState::default()
        });
        let status = || {
            window
                .imp()
                .calls
                .borrow()
                .screen
                .as_ref()
                .map(|screen| screen.status.label().to_string())
        };
        assert_eq!(status(), None, "no window while the slot is free");

        window.on_call_event(&CallEvent {
            from: "bob@example.org/phone".parse().expect("jid"),
            sid: "c1".into(),
            kind: CallEventKind::Propose(CallMedia::AUDIO),
        });
        settle();
        assert_eq!(status().as_deref(), Some("Incoming voice call"));
        let shown = window
            .imp()
            .calls
            .borrow()
            .screen
            .as_ref()
            .map(|screen| screen.shown.clone());
        assert_eq!(shown, Some(vec![Button::Decline, Button::Answer]));

        window.decline_call("c1");
        settle();
        assert_eq!(status(), None, "declining closes the window");
        assert_eq!(window.call_state(), CallState::Idle);
    }
}
