use super::permissions::write_tuple_if_absent;
use super::*;
use crate::admin::channels::{acquire_room_config_lock, explicit_channel_affiliations_for_jids};

fn data_form_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .filter(|child| child.name() == "field" && child.ns() == DATA_FORMS_NS)
        .find(|field| field.attr("var") == Some(var))
        .and_then(|field| field.get_child("value", DATA_FORMS_NS))
        .map(|value| value.texts().collect())
}

fn data_form_bool(form: &Element, var: &str) -> Option<bool> {
    data_form_value(form, var).and_then(|value| match value.as_str() {
        "1" | "true" | "True" | "TRUE" => Some(true),
        "0" | "false" | "False" | "FALSE" => Some(false),
        _ => None,
    })
}

fn channel_type_from_config(config: &waddle_xmpp::muc::RoomConfig) -> waddle_xmpp::ChannelType {
    if config.group_dm {
        waddle_xmpp::ChannelType::GroupDm
    } else if config.forum {
        waddle_xmpp::ChannelType::Forum
    } else if config.moderated {
        waddle_xmpp::ChannelType::Announcement
    } else {
        waddle_xmpp::ChannelType::Text
    }
}

pub(super) fn project_channel_type_to_config(
    config: &mut waddle_xmpp::muc::RoomConfig,
    channel_type: waddle_xmpp::ChannelType,
) {
    match channel_type {
        waddle_xmpp::ChannelType::Text => {
            config.group_dm = false;
            config.forum = false;
        }
        waddle_xmpp::ChannelType::Announcement => {
            config.group_dm = false;
            config.forum = false;
            config.moderated = true;
        }
        waddle_xmpp::ChannelType::Forum => {
            config.group_dm = false;
            config.forum = true;
        }
        waddle_xmpp::ChannelType::GroupDm => {
            config.group_dm = true;
            config.forum = false;
            config.members_only = true;
        }
    }
}

pub(super) async fn managed_channel_type_for_room(
    state: &WebSocketState,
    room_jid: &BareJid,
) -> Result<Option<waddle_xmpp::ChannelType>, String> {
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(room_jid) else {
        return Ok(None);
    };
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    get_xmpp_channel(actor, &channel_id)
        .await
        .map_err(|error| format!("channel lookup failed: {error}"))
        .map(|channel| {
            channel.and_then(|channel| waddle_xmpp::ChannelType::parse(&channel.channel_type))
        })
}

async fn recover_exact_room_after_ambiguous_config_commit(
    state: &WebSocketState,
    room_jid: &BareJid,
    stale_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    recovery_snapshot: &waddle_xmpp::muc::room_actor::RoomSnapshot,
) -> Result<
    (
        kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        waddle_xmpp::muc::room_actor::RoomSnapshot,
    ),
    String,
> {
    // Demote the exact stale actor, then acquire the successor WITH the live
    // roster in one registry step: transplanting after publication would let
    // a join/leave that landed on an already-live successor be overwritten by
    // the stale snapshot (the registry documents why post-publication merging
    // is unsafe). If the stale actor was already replaced, the live successor
    // is authoritative and gets no transplant.
    let demoted = state
        .deps
        .protocol
        .room_registry
        .ask(
            waddle_xmpp::muc::room_registry_actor::DemoteRoomIfExactActor {
                room_jid: room_jid.clone(),
                actor_ref: stale_actor.clone(),
            },
        )
        .await
        .map_err(|error| format!("config outcome recovery demotion failed: {error:?}"))?;
    let recovered_actor = if demoted {
        state
            .deps
            .protocol
            .room_registry
            .ask(
                waddle_xmpp::muc::room_registry_actor::GetOrCreateRoomWithLiveRoster {
                    room_jid: room_jid.clone(),
                    waddle_id: waddle_xmpp::muc::durable::WaddleId::new(
                        recovery_snapshot.room.waddle_id.clone(),
                    ),
                    channel_id: waddle_xmpp::muc::durable::ChannelId::new(
                        recovery_snapshot.room.channel_id.clone(),
                    ),
                    config: recovery_snapshot.room.config.clone(),
                    live_room_restore: recovery_snapshot.room.clone(),
                    occupancy_revision: recovery_snapshot.occupancy_revision,
                    departures: recovery_snapshot.departures.clone(),
                },
            )
            .await
            .map_err(|error| format!("config outcome recovery failed: {error:?}"))?
            .actor_ref
    } else {
        waddle_xmpp::muc::RoomRegistry::wrap(state.deps.protocol.room_registry.clone())
            .get_or_create_room(
                room_jid.clone(),
                recovery_snapshot.room.waddle_id.clone(),
                recovery_snapshot.room.channel_id.clone(),
                recovery_snapshot.room.config.clone(),
            )
            .await
            .map_err(|error| format!("config outcome recovery failed: {error:?}"))?
            .actor_ref
    };
    let recovered_snapshot = recovered_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("recovered config snapshot failed: {error:?}"))?;
    Ok((recovered_actor, recovered_snapshot))
}

fn request_config_reservation_arm(
    state: &WebSocketState,
    reservation: Option<&waddle_xmpp::muc::RoomEffectReservation>,
) {
    let Some(reservation) = reservation else {
        return;
    };
    state
        .deps
        .protocol
        .room_effect_arm_supervisor
        .arm(reservation.clone());
}

const OWNER_CONFIG_RECOVERY_ASK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone)]
enum CancelledOwnerConfigAskRecoveryAction {
    ArmReservation {
        arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
    },
    DeferMembersOnly(PendingMembersOnlyEnforcementSeed),
}

#[derive(Clone)]
struct PendingMembersOnlyEnforcementSeed {
    request: PendingMembersOnlyEnforcementRequest,
    room_jid: BareJid,
    connections: std::sync::Arc<waddle_xmpp::registry::ConnectionRegistry>,
    sfu: Option<std::sync::Arc<dyn waddle_sfu::SfuService>>,
    arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
}

impl PendingMembersOnlyEnforcementSeed {
    async fn run(
        self,
        actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        fallback_reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) {
        PendingMembersOnlyEnforcement {
            actor,
            request: self.request,
            fallback_reservation,
            room_jid: self.room_jid,
            connections: self.connections,
            sfu: self.sfu,
            arm_supervisor: self.arm_supervisor,
        }
        .run()
        .await;
    }
}

struct CancelledOwnerConfigAskRecoveryGuard {
    actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    room_jid: BareJid,
    intended_config: waddle_xmpp::muc::RoomConfig,
    expected_revision: u64,
    outbox: std::sync::Arc<crate::room_effect_outbox::RoomEffectOutboxStore>,
    action: CancelledOwnerConfigAskRecoveryAction,
    disarmed: bool,
}

impl CancelledOwnerConfigAskRecoveryGuard {
    /// Reservations to arm once the cancelled ask's outcome is known. When
    /// the intended config is still the latest, its exact row; when a later
    /// config commit superseded it, EVERY still-inert row of the lifecycle up
    /// to the latest config commit (each describes a durably committed
    /// config — arm-by-default — and an unarmed row would head-of-line-block
    /// the lifecycle FIFO).
    async fn recovered_reservations(
        &self,
        snapshot: &waddle_xmpp::muc::room_actor::RoomSnapshot,
    ) -> Vec<waddle_xmpp::muc::RoomEffectReservation> {
        let Some(coordinates) = snapshot.config_durable_coordinates else {
            return Vec::new();
        };
        let exact = snapshot.config_revision == self.expected_revision
            && snapshot.room.config == self.intended_config;
        // A transient outbox lookup failure must not strand committed inert rows
        // at the lifecycle FIFO head (nothing else arms live-origin rows): retry
        // with backoff before giving up.
        let mut lookup_attempt = 0_i64;
        loop {
            match if exact {
                self.outbox
                    .staged_reservation_for(coordinates.lifecycle, coordinates.revision)
                    .await
                    .map(|reservation| reservation.into_iter().collect())
            } else {
                self.outbox
                    .staged_reservations_up_to(coordinates.lifecycle, coordinates.revision)
                    .await
            } {
                Ok(reservations) => break reservations,
                // Retry until the lookup succeeds: the producing process is still
                // alive, so no other supervisor will ever arm these rows; the backoff
                // is capped at MAX_RETRY_DELAY_MS and this task is detached.
                Err(error) => {
                    lookup_attempt += 1;
                    let backoff_ms = crate::room_effect_outbox::retry_delay_ms(lookup_attempt);
                    tracing::warn!(
                        room = %self.room_jid,
                        %error,
                        lookup_attempt,
                        backoff_ms,
                        "cancelled owner config ask recovery: staged reservation lookup failed; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms.max(0) as u64))
                        .await;
                }
            }
        }
    }

    fn arm_only(
        state: &WebSocketState,
        actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        room_jid: &BareJid,
        intended_config: &waddle_xmpp::muc::RoomConfig,
        expected_revision: u64,
    ) -> Self {
        Self {
            actor: actor.clone(),
            room_jid: room_jid.clone(),
            intended_config: intended_config.clone(),
            expected_revision,
            outbox: std::sync::Arc::clone(&state.deps.protocol.room_effect_outbox),
            action: CancelledOwnerConfigAskRecoveryAction::ArmReservation {
                arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
            },
            disarmed: false,
        }
    }

    fn defer_members_only(
        state: &WebSocketState,
        actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
        room_jid: &BareJid,
        intended_config: &waddle_xmpp::muc::RoomConfig,
        expected_revision: u64,
        seed: PendingMembersOnlyEnforcementSeed,
    ) -> Self {
        Self {
            actor: actor.clone(),
            room_jid: room_jid.clone(),
            intended_config: intended_config.clone(),
            expected_revision,
            outbox: std::sync::Arc::clone(&state.deps.protocol.room_effect_outbox),
            action: CancelledOwnerConfigAskRecoveryAction::DeferMembersOnly(seed),
            disarmed: false,
        }
    }

    fn disarm(&mut self) {
        self.disarmed = true;
    }

    async fn recover(self) {
        let mut timeout_attempt = 0_i64;
        let snapshot = loop {
            match self
                .actor
                .ask(GetSnapshot)
                .reply_timeout(OWNER_CONFIG_RECOVERY_ASK_TIMEOUT)
                .await
            {
                Ok(snapshot) => break snapshot,
                // Both Timeout variants mean the same thing here: the actor
                // did not answer in time (kameo reports reply timeouts with
                // and without the returned message). Retry either way — the
                // give-up arm below is for closed/dead actors only.
                Err(kameo::error::SendError::Timeout(_)) => {
                    timeout_attempt = timeout_attempt.saturating_add(1);
                    let backoff_ms = crate::room_effect_outbox::retry_delay_ms(timeout_attempt);
                    tracing::warn!(
                        room = %self.room_jid,
                        timeout_attempt,
                        backoff_ms,
                        "cancelled owner config ask recovery snapshot timed out; retrying"
                    );
                    tokio::time::sleep(std::time::Duration::from_millis(backoff_ms.max(0) as u64))
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        room = %self.room_jid,
                        ?error,
                        "cancelled owner config ask recovery could not snapshot the room"
                    );
                    return;
                }
            }
        };
        let exact_intended_config = snapshot.config_revision == self.expected_revision
            && snapshot.room.config == self.intended_config;
        let recovered_reservations = self.recovered_reservations(&snapshot).await;
        match &self.action {
            CancelledOwnerConfigAskRecoveryAction::ArmReservation { arm_supervisor } => {
                for reservation in recovered_reservations {
                    arm_supervisor.clone().arm(reservation);
                }
            }
            CancelledOwnerConfigAskRecoveryAction::DeferMembersOnly(seed) => {
                if exact_intended_config {
                    seed.clone()
                        .run(
                            self.actor.clone(),
                            recovered_reservations.into_iter().next(),
                        )
                        .await;
                } else {
                    for reservation in recovered_reservations {
                        seed.arm_supervisor.clone().arm(reservation);
                    }
                }
            }
        }
    }
}

impl Drop for CancelledOwnerConfigAskRecoveryGuard {
    fn drop(&mut self) {
        if self.disarmed {
            return;
        }
        let recovery = Self {
            actor: self.actor.clone(),
            room_jid: self.room_jid.clone(),
            intended_config: self.intended_config.clone(),
            expected_revision: self.expected_revision,
            outbox: std::sync::Arc::clone(&self.outbox),
            action: self.action.clone(),
            disarmed: true,
        };
        tokio::spawn(recovery.recover());
    }
}

struct CommittedConfigReservationGuard<'a> {
    state: &'a WebSocketState,
    reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    pending_members_only_enforcement: Option<PendingMembersOnlyEnforcement>,
}

struct PendingMembersOnlyEnforcement {
    actor: kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    request: PendingMembersOnlyEnforcementRequest,
    fallback_reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    room_jid: BareJid,
    connections: std::sync::Arc<waddle_xmpp::registry::ConnectionRegistry>,
    sfu: Option<std::sync::Arc<dyn waddle_sfu::SfuService>>,
    arm_supervisor: crate::room_effect_outbox::RoomEffectArmSupervisor,
}

#[derive(Clone)]
enum PendingMembersOnlyEnforcementRequest {
    Managed {
        affiliations: Vec<(BareJid, waddle_xmpp::Affiliation)>,
        config_status_codes: Vec<waddle_xmpp::muc::MucConfigStatusCode>,
    },
    Unmanaged,
}

impl PendingMembersOnlyEnforcement {
    async fn run(self) {
        let Self {
            actor,
            request,
            fallback_reservation,
            room_jid,
            connections,
            sfu,
            arm_supervisor,
        } = self;
        let fallback_on_failure = fallback_reservation.clone();
        match request {
            PendingMembersOnlyEnforcementRequest::Managed {
                affiliations,
                config_status_codes,
            } => match actor
                .ask(EnforceMembersOnlyAffiliations {
                    affiliations,
                    fallback_reservation,
                    config_status_codes,
                })
                .await
            {
                Ok(applied) => {
                    super::super::super::muc_call_sfu::converge_members_only_sweep_via_sfu(
                        sfu.as_ref(),
                        &room_jid,
                        &applied,
                    );
                    if let Some(reservation) = applied.outbox_reservation.as_ref() {
                        arm_supervisor.arm(reservation.clone());
                    } else {
                        for (recipient, presence) in applied.presence_updates {
                            let _ = connections.try_send_to(&recipient, Stanza::Presence(presence));
                        }
                    }
                }
                Err(error) => {
                    if let Some(reservation) = fallback_on_failure {
                        arm_supervisor.arm(reservation);
                    }
                    tracing::warn!(
                        room = %room_jid,
                        ?error,
                        "cancelled owner config recovery could not enforce members-only"
                    );
                }
            },
            PendingMembersOnlyEnforcementRequest::Unmanaged => {
                match actor.ask(EnforceMembersOnly).await {
                    Ok(applied) => {
                        super::super::super::muc_call_sfu::converge_members_only_sweep_via_sfu(
                            sfu.as_ref(),
                            &room_jid,
                            &applied,
                        );
                        for (recipient, presence) in applied.presence_updates {
                            let _ = connections.try_send_to(&recipient, Stanza::Presence(presence));
                        }
                        if let Some(reservation) = fallback_reservation {
                            arm_supervisor.arm(reservation);
                        }
                    }
                    Err(error) => {
                        if let Some(reservation) = fallback_on_failure {
                            arm_supervisor.arm(reservation);
                        }
                        tracing::warn!(
                            room = %room_jid,
                            ?error,
                            "cancelled owner config recovery could not enforce members-only"
                        );
                    }
                }
            }
        }
    }
}

impl<'a> CommittedConfigReservationGuard<'a> {
    fn new(
        state: &'a WebSocketState,
        reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) -> Self {
        Self {
            state,
            reservation,
            pending_members_only_enforcement: None,
        }
    }

    fn reservation(&self) -> Option<&waddle_xmpp::muc::RoomEffectReservation> {
        self.reservation.as_ref()
    }

    fn replace(
        &mut self,
        reservation: Option<waddle_xmpp::muc::RoomEffectReservation>,
    ) -> Option<waddle_xmpp::muc::RoomEffectReservation> {
        std::mem::replace(&mut self.reservation, reservation)
    }

    fn clear(&mut self) {
        self.reservation = None;
        self.pending_members_only_enforcement = None;
    }

    fn defer_to_members_only_enforcement(
        &mut self,
        pending_members_only_enforcement: PendingMembersOnlyEnforcement,
    ) {
        self.pending_members_only_enforcement = Some(pending_members_only_enforcement);
    }

    fn clear_members_only_enforcement(&mut self) {
        self.pending_members_only_enforcement = None;
    }
}

impl Drop for CommittedConfigReservationGuard<'_> {
    fn drop(&mut self) {
        if let Some(pending) = self.pending_members_only_enforcement.take() {
            tokio::spawn(pending.run());
            return;
        }
        request_config_reservation_arm(self.state, self.reservation());
    }
}

async fn rollback_config_or_arm_reservation(
    state: &WebSocketState,
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    expected_revision: u64,
    previous_config: waddle_xmpp::muc::RoomConfig,
    reservation: Option<&waddle_xmpp::muc::RoomEffectReservation>,
) {
    let rollback = room_actor
        .ask(waddle_xmpp::muc::room_actor::RollbackConfigIfRevision {
            expected_revision,
            config: previous_config,
            reservation: reservation.cloned(),
        })
        .await;
    if !matches!(rollback, Ok(true)) {
        request_config_reservation_arm(state, reservation);
    }
}

pub(super) async fn apply_muc_owner_config(
    state: &WebSocketState,
    room_jid: &BareJid,
    iq: &xmpp_parsers::iq::Iq,
    session: Option<&Session>,
    initiator: Option<&FullJid>,
) -> Result<ResponseBatch, String> {
    let mut room_actor = get_room_actor(state, room_jid)
        .await
        .ok_or_else(|| "room actor not found".to_string())?;
    let _config_guard = acquire_room_config_lock(room_jid).await;
    let snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("snapshot failed: {error:?}"))?;
    let previous_members_only = snapshot.room.config.members_only;
    let previous_config = snapshot.room.config.clone();
    let occupant_jids: Vec<BareJid> = snapshot
        .room
        .occupants
        .values()
        .map(|occupant| occupant.real_jid.to_bare())
        .collect();
    let mut config = previous_config.clone();

    if let xmpp_parsers::iq::Iq::Set { payload: query, .. } = iq {
        if let Some(form) = query.get_child("x", DATA_FORMS_NS) {
            if let Some(name) =
                data_form_value(form, "muc#roomconfig_roomname").filter(|value| !value.is_empty())
            {
                config.name = name;
            }
            // Treat presence of the roomdesc field as authoritative
            // even when empty, so an owner can clear the description.
            // Field absent => keep existing; field present + empty
            // => clear; field present + non-empty => set.
            if let Some(value) = data_form_value(form, "muc#roomconfig_roomdesc") {
                config.description = if value.is_empty() { None } else { Some(value) };
            }
            if let Some(members_only) = data_form_bool(form, "muc#roomconfig_membersonly") {
                config.members_only = members_only;
            }
            if let Some(public_room) = data_form_bool(form, "muc#roomconfig_publicroom") {
                config.public_room = public_room;
            }
            if let Some(moderated) = data_form_bool(form, "muc#roomconfig_moderatedroom") {
                config.moderated = moderated;
            }
            if let Some(enable_logging) = data_form_bool(form, "muc#roomconfig_enablelogging") {
                config.enable_logging = enable_logging;
            }
            // XEP-0045 §8.1 (#1265 item 8): who may change the subject.
            if let Some(changesubject) = data_form_bool(form, "muc#roomconfig_changesubject") {
                config.occupants_may_change_subject = changesubject;
            }
            if let Some(forum) = data_form_bool(form, "muc#roomconfig_forum") {
                config.forum = forum;
            }
            // XEP-0045 muc#roomconfig_allowpm (#1257): who may send
            // §7.5 private messages. Unknown values keep the previous
            // policy (mirrors the pin-permission fallback rule).
            if let Some(value) = data_form_value(form, "muc#roomconfig_allowpm") {
                if let Some(allow_pm) = waddle_xmpp::muc::AllowPm::from_form_value(&value) {
                    config.allow_pm = allow_pm;
                }
            }
            // #415: per-room pin permission policy.
            if let Some(value) =
                data_form_value(form, waddle_xmpp::muc::owner::FIELD_PIN_PERMISSION)
            {
                if let Some(pin_permission) =
                    waddle_xmpp::muc::PinPermission::from_form_value(&value)
                {
                    config.pin_permission = pin_permission;
                }
            }
            // #1265 item 7: honor muc#roomconfig_maxusers on submit.
            // The registrar value "none" and 0 both mean unlimited.
            if let Some(value) = data_form_value(form, "muc#roomconfig_maxusers") {
                let trimmed = value.trim();
                if trimmed.eq_ignore_ascii_case("none") {
                    config.max_occupants = 0;
                } else if let Ok(max_occupants) = trimmed.parse::<u32>() {
                    config.max_occupants = max_occupants;
                }
            }
        }
    }

    // #1265 item 7: `muc#roomconfig_persistentroom` is not offered in
    // the config form — submitting this very form persists the room
    // into the channel catalog below, so every configured room is
    // persistent by construction. A submitted value for the unoffered
    // field is ignored.
    config.persistent = true;

    let channel_id = waddle_xmpp::parse_managed_room_jid(room_jid);
    let existing_channel_type = managed_channel_type_for_room(state, room_jid).await?;
    if let Some(channel_type) = existing_channel_type {
        project_channel_type_to_config(&mut config, channel_type);
    }
    if let (Some(channel_id), Some(session)) = (channel_id.as_deref(), session) {
        write_tuple_if_absent(
            state,
            Tuple::new(
                Object::new(ObjectType::Channel, channel_id),
                Relation::new("owner"),
                Subject::user(&session.user_jid),
            ),
        )
        .await
        .map_err(|error| format!("channel owner tuple failed: {error}"))?;
    }
    let managed_enforcement_affiliations = if !previous_members_only && config.members_only {
        if let Some(channel_id) = channel_id.as_deref() {
            Some(
                explicit_channel_affiliations_for_jids(
                    &state.deps.app_state,
                    channel_id,
                    occupant_jids,
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };

    config = config.normalized();
    let mut recovered_broadcast_room = None;
    let mut recovered_voice_roster = None;
    let effect_plan = if !previous_members_only && config.members_only {
        if channel_id.is_some() {
            waddle_xmpp::muc::room_actor::ConfigEffectPlan::ManagedMembersOnlyFallback
        } else {
            waddle_xmpp::muc::room_actor::ConfigEffectPlan::UnmanagedMembersOnlyPostEnforcement
        }
    } else {
        waddle_xmpp::muc::room_actor::ConfigEffectPlan::DirectAudience
    };
    let mut config_reservation = None;
    let mut cancelled_commit_recovery = if !previous_members_only && config.members_only {
        let request = if let Some(affiliations) = managed_enforcement_affiliations.as_ref() {
            PendingMembersOnlyEnforcementRequest::Managed {
                affiliations: affiliations.clone(),
                config_status_codes: waddle_xmpp::muc::config_change_status_codes(
                    &previous_config,
                    &config,
                ),
            }
        } else {
            PendingMembersOnlyEnforcementRequest::Unmanaged
        };
        CancelledOwnerConfigAskRecoveryGuard::defer_members_only(
            state,
            &room_actor,
            room_jid,
            &config,
            snapshot.config_revision.saturating_add(1),
            PendingMembersOnlyEnforcementSeed {
                request,
                room_jid: room_jid.clone(),
                connections: state.deps.protocol.connection_registry.clone(),
                sfu: state.deps.protocol.sfu.clone(),
                arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
            },
        )
    } else {
        CancelledOwnerConfigAskRecoveryGuard::arm_only(
            state,
            &room_actor,
            room_jid,
            &config,
            snapshot.config_revision.saturating_add(1),
        )
    };
    let expected_revision = match room_actor
        .ask(UpdateConfig {
            config: config.clone(),
            effect_plan,
        })
        .await
    {
        Ok(applied) => {
            cancelled_commit_recovery.disarm();
            config_reservation = applied.reservation;
            applied.revision
        }
        Err(kameo::error::SendError::HandlerError(
            waddle_xmpp::muc::room_actor::RoomMutationError::CommitOutcomeUnknown,
        )) => {
            cancelled_commit_recovery.disarm();
            let (recovered_actor, recovered_snapshot) =
                recover_exact_room_after_ambiguous_config_commit(
                    state,
                    room_jid,
                    &room_actor,
                    &snapshot,
                )
                .await?;
            if recovered_snapshot.room.config != config {
                return Err("config update outcome is being reconciled; please retry".to_string());
            }
            let mut room_with_reconciled_config = snapshot.room.clone();
            room_with_reconciled_config.config = config.clone();
            if let Some(affiliations) = &managed_enforcement_affiliations {
                for (jid, affiliation) in affiliations {
                    room_with_reconciled_config.set_affiliation(jid.clone(), *affiliation);
                }
            } else {
                for affiliation in recovered_snapshot.room.get_all_affiliations() {
                    room_with_reconciled_config
                        .set_affiliation(affiliation.jid, affiliation.affiliation);
                }
            }
            if !previous_members_only && config.members_only {
                // Drop evicted non-members from the reconciled broadcast
                // roster; the applied removal effects come from the live
                // actor's EnforceMembersOnlyAffiliations ask below, so the
                // in-memory effects are discarded here.
                let _ = waddle_xmpp::muc::room_actor::enforce_members_only_from_room(
                    &mut room_with_reconciled_config,
                    &state.deps.occupant_id_secret,
                );
            }
            recovered_voice_roster = Some(
                super::super::super::muc_call_sfu::derive_room_voice_from_snapshot(
                    &room_with_reconciled_config,
                    &config,
                ),
            );
            recovered_broadcast_room = Some(room_with_reconciled_config);
            room_actor = recovered_actor;
            if let Some(coordinates) = recovered_snapshot.config_durable_coordinates {
                config_reservation = state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .staged_reservation_for(coordinates.lifecycle, coordinates.revision)
                    .await
                    .map_err(|error| format!("config reservation recovery failed: {error}"))?;
            }
            recovered_snapshot.config_revision
        }
        Err(error) => {
            cancelled_commit_recovery.disarm();
            return Err(format!("config update failed: {error:?}"));
        }
    };
    let config_status_codes =
        waddle_xmpp::muc::config_change_status_codes(&previous_config, &config);
    let mut config_reservation = CommittedConfigReservationGuard::new(state, config_reservation);
    if let Some(affiliations) = managed_enforcement_affiliations.as_ref() {
        config_reservation.defer_to_members_only_enforcement(PendingMembersOnlyEnforcement {
            actor: room_actor.clone(),
            request: PendingMembersOnlyEnforcementRequest::Managed {
                affiliations: affiliations.clone(),
                config_status_codes: config_status_codes.clone(),
            },
            fallback_reservation: config_reservation.reservation().cloned(),
            room_jid: room_jid.clone(),
            connections: state.deps.protocol.connection_registry.clone(),
            sfu: state.deps.protocol.sfu.clone(),
            arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
        });
    } else if !previous_members_only && config.members_only && channel_id.is_none() {
        config_reservation.defer_to_members_only_enforcement(PendingMembersOnlyEnforcement {
            actor: room_actor.clone(),
            request: PendingMembersOnlyEnforcementRequest::Unmanaged,
            fallback_reservation: config_reservation.reservation().cloned(),
            room_jid: room_jid.clone(),
            connections: state.deps.protocol.connection_registry.clone(),
            sfu: state.deps.protocol.sfu.clone(),
            arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
        });
    }

    let Some(channel_id) = channel_id else {
        if !previous_members_only && config.members_only {
            let enforcement = room_actor.ask(EnforceMembersOnly).await;
            config_reservation.clear_members_only_enforcement();
            let applied = enforcement
                .map_err(|error| format!("members-only enforcement failed: {error:?}"))?;
            super::super::super::muc_call_sfu::converge_members_only_sweep_via_sfu(
                state.deps.protocol.sfu.as_ref(),
                room_jid,
                &applied,
            );
            for (recipient, presence) in applied.presence_updates {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&recipient, Stanza::Presence(presence));
            }
        }
        converge_flip(
            state,
            room_jid,
            &room_actor,
            &previous_config,
            &config,
            recovered_voice_roster.as_deref(),
        )
        .await;
        let post_update_snapshot = room_actor
            .ask(GetSnapshot)
            .await
            .map_err(|error| format!("post-config snapshot failed: {error:?}"))?;
        let response = drain_or_broadcast_config_change(
            state,
            room_jid,
            recovered_broadcast_room
                .as_ref()
                .unwrap_or(&post_update_snapshot.room),
            &config_status_codes,
            config_reservation.reservation(),
            initiator,
        )
        .await;
        config_reservation.clear();
        return Ok(response);
    };

    let now = chrono::Utc::now().to_rfc3339();
    let actor = state.deps.app_state.db_pool.global_actor().clone();
    let channel_type = existing_channel_type.unwrap_or_else(|| channel_type_from_config(&config));
    if let Err(error) = actor
        .ask(DbExecute {
            sql: r#"
                INSERT INTO channels (id, name, description, channel_type, position, is_default, pin_permission, members_only, public_room, created_at, updated_at)
                VALUES (?, ?, ?, ?, 0, 0, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    description = excluded.description,
                    channel_type = excluded.channel_type,
                    pin_permission = excluded.pin_permission,
                    members_only = excluded.members_only,
                    public_room = excluded.public_room,
                    updated_at = excluded.updated_at
            "#
            .to_string(),
            params: vec![
                channel_id.clone().into(),
                config.name.clone().into(),
                config.description.clone().into(),
                channel_type.as_str().into(),
                config.pin_permission.as_form_value().into(),
                config.members_only.into(),
                config.public_room.into(),
                now.clone().into(),
                now.into(),
            ],
        })
        .await
    {
        // If we have to compensate the committed config write, invalidate the
        // deferred members-only sweep before awaiting rollback. Otherwise the
        // guard's Drop would still spawn the stale 322 enforcement after the
        // actor restored the previous config.
        config_reservation.clear_members_only_enforcement();
        rollback_config_or_arm_reservation(
            state,
            &room_actor,
            expected_revision,
            previous_config,
            config_reservation.reservation(),
        )
        .await;
        config_reservation.clear();
        return Err(format!("channel upsert failed: {error}"));
    }

    // The channel#owner tuple was written before the config update/upsert so the
    // creator can always rejoin after restart, before a Space bookmark is
    // published. XEP-0045 §10 requires the room creator to be an owner.
    if session.is_none() {
        warn!(
            channel_id = %channel_id,
            "apply_muc_owner_config called without a session; \
             channel owner tuple not written — room may be inaccessible after server restart"
        );
    }

    if !previous_members_only && config.members_only {
        let enforcement = if let Some(affiliations) = managed_enforcement_affiliations {
            room_actor
                .ask(EnforceMembersOnlyAffiliations {
                    affiliations,
                    fallback_reservation: config_reservation.reservation().cloned(),
                    config_status_codes: config_status_codes.clone(),
                })
                .await
                .map_err(|error| format!("members-only enforcement failed: {error:?}"))
        } else {
            room_actor
                .ask(EnforceMembersOnly)
                .await
                .map_err(|error| format!("members-only enforcement failed: {error:?}"))
        };
        config_reservation.clear_members_only_enforcement();
        let applied = enforcement?;
        if applied.outbox_reservation.is_some() {
            let _ = config_reservation.replace(applied.outbox_reservation.clone());
        }
        super::super::super::muc_call_sfu::converge_members_only_sweep_via_sfu(
            state.deps.protocol.sfu.as_ref(),
            room_jid,
            &applied,
        );
        if applied.outbox_reservation.is_none() {
            for (recipient, presence) in applied.presence_updates {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&recipient, Stanza::Presence(presence));
            }
        }
    }

    converge_flip(
        state,
        room_jid,
        &room_actor,
        &previous_config,
        &config,
        recovered_voice_roster.as_deref(),
    )
    .await;

    let post_update_snapshot = room_actor
        .ask(GetSnapshot)
        .await
        .map_err(|error| format!("post-config snapshot failed: {error:?}"))?;
    let response = drain_or_broadcast_config_change(
        state,
        room_jid,
        recovered_broadcast_room
            .as_ref()
            .unwrap_or(&post_update_snapshot.room),
        &config_status_codes,
        config_reservation.reservation(),
        initiator,
    )
    .await;
    config_reservation.clear();
    Ok(response)
}

async fn drain_or_broadcast_config_change(
    state: &WebSocketState,
    room_jid: &BareJid,
    room: &waddle_xmpp::muc::MucRoom,
    status_codes: &[waddle_xmpp::muc::MucConfigStatusCode],
    reservation: Option<&waddle_xmpp::muc::RoomEffectReservation>,
    initiator: Option<&FullJid>,
) -> ResponseBatch {
    if status_codes.is_empty() {
        return ResponseBatch::default();
    }
    if let Some(reservation) = reservation {
        if let Err(error) = state
            .deps
            .protocol
            .room_effect_outbox
            .arm_reservation(reservation, crate::time::now_ms())
            .await
        {
            tracing::warn!(
                room = %room_jid,
                %error,
                "owner config direct arm failed after committed mutation; supervisor will retry"
            );
            request_config_reservation_arm(state, Some(reservation));
            return ResponseBatch::default();
        }
        match crate::room_effect_outbox::drain::drain_reservation_inline(
            state,
            reservation,
            initiator,
        )
        .await
        {
            Ok(frames) => return response_batch_from_inline_room_effect_frames(frames),
            Err(error) => {
                tracing::warn!(
                    room = %room_jid,
                    %error,
                    "owner config inline drain failed after committed mutation"
                );
                return ResponseBatch::default();
            }
        }
    }

    for occupant in room.occupants.values() {
        for recipient_jid in room.get_occupant_sessions(&occupant.nick) {
            let message = waddle_xmpp::muc::build_config_change_message(
                room_jid,
                &recipient_jid,
                status_codes,
            );
            let _ = state
                .deps
                .protocol
                .connection_registry
                .try_send_to(&recipient_jid, Stanza::Message(message));
        }
    }
    ResponseBatch::default()
}

/// Thin adapter: run the shared moderation-flip convergence only when
/// this config change actually flipped `moderated`.
async fn converge_flip(
    state: &WebSocketState,
    room_jid: &BareJid,
    room_actor: &kameo::actor::ActorRef<waddle_xmpp::muc::room_actor::RoomActor>,
    previous: &waddle_xmpp::muc::RoomConfig,
    updated: &waddle_xmpp::muc::RoomConfig,
    voices_from_recovered_snapshot: Option<&[(FullJid, waddle_xmpp_core::types::Voice)]>,
) {
    if previous.moderated == updated.moderated {
        return;
    }
    super::super::super::muc_call_sfu::converge_room_voice_after_moderation_flip(
        state.deps.protocol.sfu.as_ref(),
        room_actor,
        room_jid,
        voices_from_recovered_snapshot,
    )
    .await;
}

#[cfg(test)]
mod channel_type_projection_tests {
    use super::*;

    #[test]
    fn text_projection_preserves_xep_moderatedroom() {
        let mut config = waddle_xmpp::muc::RoomConfig {
            group_dm: true,
            forum: true,
            moderated: true,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        project_channel_type_to_config(&mut config, waddle_xmpp::ChannelType::Text);

        assert!(!config.group_dm);
        assert!(!config.forum);
        assert!(config.moderated);
    }

    #[test]
    fn announcement_projection_forces_moderatedroom() {
        let mut config = waddle_xmpp::muc::RoomConfig {
            forum: true,
            moderated: false,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        project_channel_type_to_config(&mut config, waddle_xmpp::ChannelType::Announcement);

        assert!(!config.group_dm);
        assert!(!config.forum);
        assert!(config.moderated);
    }

    #[test]
    fn group_dm_projection_forces_members_only() {
        let mut config = waddle_xmpp::muc::RoomConfig {
            members_only: false,
            ..waddle_xmpp::muc::RoomConfig::default()
        };

        project_channel_type_to_config(&mut config, waddle_xmpp::ChannelType::GroupDm);

        assert!(config.group_dm);
        assert!(config.members_only);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::str::FromStr;
    use std::sync::Mutex;
    use std::time::Duration;

    use jid::{BareJid, FullJid};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::sync::Notify;
    use tokio_util::sync::CancellationToken;
    use waddle_xmpp::muc::{
        durable::{
            DurableRoomState, MucDurableFuture, MucDurableStore, RoomCommitDatabaseError,
            RoomCommitError, RoomCommitFuture, RoomCommittedCoordinates, RoomDurableMutation,
        },
        room_actor::{
            ChangeAffiliation, ConfigEffectPlan, GetSnapshot, JoinAffiliationGrant,
            JoinWithAffiliation, UpdateConfig,
        },
        room_registry_actor::WireClusteringClaims,
        MucConfigStatusCode, RoomEffectReservation, RoomLifecycleId, RoomMutationEffects,
        RoomRevision,
    };
    use waddle_xmpp::ownership::{
        ClaimStore, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    use super::{
        apply_muc_owner_config, rollback_config_or_arm_reservation,
        CommittedConfigReservationGuard, PendingMembersOnlyEnforcement,
        PendingMembersOnlyEnforcementRequest, OWNER_CONFIG_RECOVERY_ASK_TIMEOUT,
    };
    use crate::room_effect_outbox::{
        RoomEffectEnqueue, RoomEffectKey, RoomEffectOriginInstanceId, RoomEffectProducingNode,
    };
    use crate::server::routes::websocket::{
        cleanup::get_or_create_room_actor,
        stanza_to_xml,
        tests::{
            create_test_websocket_state, create_test_websocket_state_with_sfu,
            register_test_connection, snapshot_room, RecordingSfu,
        },
        WebSocketState,
    };

    fn room_jid() -> BareJid {
        BareJid::from_str("cancellation-room@conference.example.test").expect("room JID")
    }

    fn full_jid(value: &str) -> FullJid {
        FullJid::from_str(value).expect("full JID")
    }

    fn config_effects() -> RoomMutationEffects {
        RoomMutationEffects::config(
            room_jid(),
            vec![MucConfigStatusCode::NonPrivacyConfigurationChange],
            vec![full_jid("alice@example.test/device")],
        )
    }

    fn origin() -> RoomEffectOriginInstanceId {
        RoomEffectOriginInstanceId::new("config-guard-origin".to_owned()).expect("origin")
    }

    fn producing_node() -> RoomEffectProducingNode {
        RoomEffectProducingNode::from_node_identity(NodeIdentity::new("node-a", "epoch-a"))
    }

    async fn enqueue_config_reservation(
        state: &WebSocketState,
        lifecycle: RoomLifecycleId,
        revision: RoomRevision,
    ) -> RoomEffectReservation {
        let store = state.deps.protocol.room_effect_outbox.as_ref();
        let mut tx = store.database().begin().await.expect("transaction");
        let reservation = store
            .enqueue_in_tx(
                &mut tx,
                RoomEffectEnqueue {
                    lifecycle,
                    revision,
                    effects: &config_effects(),
                    origin: &origin(),
                    producing_node: &producing_node(),
                    now_ms: 100,
                },
            )
            .await
            .expect("enqueue");
        tx.commit().await.expect("commit");
        reservation
    }

    #[derive(Clone)]
    struct CommitPause {
        reached: Arc<Notify>,
        release: CancellationToken,
    }

    impl CommitPause {
        const TIMEOUT: Duration = Duration::from_secs(30);

        fn new() -> Self {
            Self {
                reached: Arc::new(Notify::new()),
                release: CancellationToken::new(),
            }
        }

        async fn wait_until_reached(&self) {
            tokio::time::timeout(Self::TIMEOUT, self.reached.notified())
                .await
                .expect("timed out waiting for the paused owner-config commit reply; the commit was never attempted");
        }

        async fn wait_until_released(&self) {
            self.release.cancelled().await;
        }

        fn release(&self) {
            self.release.cancel();
        }
    }

    struct PausableConfigDurableStore {
        outbox: Arc<crate::room_effect_outbox::RoomEffectOutboxStore>,
        states: Mutex<HashMap<BareJid, DurableRoomState>>,
        fences: Mutex<HashMap<BareJid, waddle_xmpp::muc::RoomClaimFenceContext>>,
        coordinates: Mutex<HashMap<BareJid, (RoomLifecycleId, i64)>>,
        commit_pause: Mutex<Option<CommitPause>>,
    }

    impl PausableConfigDurableStore {
        fn new(outbox: Arc<crate::room_effect_outbox::RoomEffectOutboxStore>) -> Arc<Self> {
            Arc::new(Self {
                outbox,
                states: Mutex::new(HashMap::new()),
                fences: Mutex::new(HashMap::new()),
                coordinates: Mutex::new(HashMap::new()),
                commit_pause: Mutex::new(None),
            })
        }

        fn exact_fence_matches(
            &self,
            room_jid: &BareJid,
            fence: &waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> bool {
            self.fences.lock().expect("fences lock").get(room_jid) == Some(fence)
        }

        fn next_coordinates(&self, room_jid: &BareJid) -> RoomCommittedCoordinates {
            let mut coordinates = self.coordinates.lock().expect("coordinates lock");
            let entry = coordinates
                .entry(room_jid.clone())
                .or_insert_with(|| (RoomLifecycleId::generate(), 0));
            entry.1 += 1;
            RoomCommittedCoordinates {
                lifecycle: entry.0,
                revision: RoomRevision::from_stored(entry.1).expect("positive revision"),
            }
        }

        fn apply_mutation(&self, room_jid: &BareJid, intent: RoomDurableMutation) {
            match intent {
                RoomDurableMutation::Create {
                    waddle_id,
                    channel_id,
                    config,
                    initial_affiliations,
                } => {
                    self.states.lock().expect("states lock").insert(
                        room_jid.clone(),
                        DurableRoomState {
                            coordinates: None,
                            config_coordinates: None,
                            waddle_id: waddle_id.into_string(),
                            channel_id: channel_id.into_string(),
                            config,
                            subject: None,
                            affiliations: initial_affiliations
                                .into_iter()
                                .filter_map(|entry| {
                                    entry.affiliation.map(|affiliation| {
                                        waddle_xmpp::muc::affiliation::AffiliationEntry::new(
                                            entry.jid,
                                            affiliation,
                                        )
                                    })
                                })
                                .collect(),
                        },
                    );
                }
                RoomDurableMutation::Config { config, .. }
                | RoomDurableMutation::MembersOnlyEnforcement { config, .. } => {
                    if let Some(state) = self.states.lock().expect("states lock").get_mut(room_jid)
                    {
                        state.config = config;
                    }
                }
                _ => {}
            }
        }

        fn record_coordinates(
            &self,
            room_jid: &BareJid,
            coordinates: RoomCommittedCoordinates,
            is_config_commit: bool,
        ) {
            if let Some(state) = self.states.lock().expect("states lock").get_mut(room_jid) {
                state.coordinates = Some(coordinates);
                if is_config_commit {
                    state.config_coordinates = Some(coordinates);
                }
            }
        }

        fn pause_next_commit_reply(&self) -> CommitPause {
            let pause = CommitPause::new();
            *self.commit_pause.lock().expect("commit pause lock") = Some(pause.clone());
            pause
        }
    }

    impl MucDurableStore for PausableConfigDurableStore {
        fn load_room_state_fenced<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, Option<DurableRoomState>> {
            let exact = self.exact_fence_matches(room_jid, fence);
            let state = self
                .states
                .lock()
                .expect("states lock")
                .get(room_jid)
                .cloned();
            Box::pin(async move {
                if exact {
                    Ok(state)
                } else {
                    Err(waddle_xmpp::XmppError::OwnershipLost {
                        entity: fence.entity.clone(),
                    })
                }
            })
        }

        fn commit_room_mutation<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
            intent: RoomDurableMutation,
            effects: RoomMutationEffects,
        ) -> RoomCommitFuture<'a> {
            let exact = self.exact_fence_matches(room_jid, fence);
            let coordinates = self.next_coordinates(room_jid);
            Box::pin(async move {
                if !exact {
                    return Err(RoomCommitError::NotOwner);
                }
                let mut tx =
                    self.outbox.database().begin().await.map_err(|_| {
                        RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
                    })?;
                let reservation = if effects.effects().is_empty() {
                    None
                } else {
                    let origin = origin();
                    let producing_node = producing_node();
                    self.outbox
                        .enqueue_in_tx(
                            &mut tx,
                            RoomEffectEnqueue {
                                lifecycle: coordinates.lifecycle,
                                revision: coordinates.revision,
                                effects: &effects,
                                origin: &origin,
                                producing_node: &producing_node,
                                now_ms: 0,
                            },
                        )
                        .await
                        .map(Some)
                        .map_err(|_| {
                            RoomCommitError::Database(RoomCommitDatabaseError::sanitized())
                        })?
                };
                tx.commit()
                    .await
                    .map_err(|_| RoomCommitError::Database(RoomCommitDatabaseError::sanitized()))?;
                let is_config_commit = matches!(
                    &intent,
                    RoomDurableMutation::Create { .. }
                        | RoomDurableMutation::Config { .. }
                        | RoomDurableMutation::MembersOnlyEnforcement { .. }
                );
                self.apply_mutation(room_jid, intent);
                self.record_coordinates(room_jid, coordinates, is_config_commit);
                let pause = { self.commit_pause.lock().expect("commit pause lock").take() };
                if let Some(pause) = pause {
                    // Each pause has exactly one producer and one test waiter. `notify_one`
                    // retains a permit if the waiter has not reached `.notified()` yet.
                    pause.reached.notify_one();
                    pause.wait_until_released().await;
                }
                Ok(waddle_xmpp::muc::RoomCommitOutcome {
                    coordinates,
                    reservation,
                })
            })
        }

        fn establish_claim_fence(
            &self,
            room_jid: &BareJid,
            fence: waddle_xmpp::muc::RoomClaimFenceContext,
        ) {
            self.fences
                .lock()
                .expect("fences lock")
                .insert(room_jid.clone(), fence);
        }

        fn check_exact_claim_fence<'a>(
            &'a self,
            room_jid: &'a BareJid,
            fence: &'a waddle_xmpp::muc::RoomClaimFenceContext,
        ) -> MucDurableFuture<'a, bool> {
            let exact = self.exact_fence_matches(room_jid, fence);
            Box::pin(async move { Ok(exact) })
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_owner_config_snapshot_timeout_then_arms_committed_reservation() {
        let state = create_test_websocket_state().await;
        let durable_store =
            PausableConfigDurableStore::new(Arc::clone(&state.deps.protocol.room_effect_outbox));
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "owner-config-cancel-node",
                    "owner-config-cancel-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable owner-config store");

        let room = room_jid();
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room,
            waddle_xmpp::muc::RoomConfig::default(),
            "waddle".to_owned(),
            "channel".to_owned(),
        )
        .await
        .expect("create room")
        .actor_ref;
        let owner = full_jid("alice@example.test/desktop");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: owner.clone(),
                nick: "alice".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Owner),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("owner join");

        let submit_form =
            xmpp_parsers::minidom::Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("type").to_owned(),
                    "submit".to_owned(),
                )
                .append(
                    xmpp_parsers::minidom::Element::builder(
                        "field",
                        waddle_xmpp::muc::DATA_FORMS_NS,
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("var").to_owned(),
                        "muc#roomconfig_roomname",
                    )
                    .append(
                        xmpp_parsers::minidom::Element::builder(
                            "value",
                            waddle_xmpp::muc::DATA_FORMS_NS,
                        )
                        .append("After")
                        .build(),
                    )
                    .build(),
                )
                .build();
        let iq = xmpp_parsers::iq::Iq::Set {
            from: Some(jid::Jid::from(owner.clone())),
            to: Some(jid::Jid::from(room.clone())),
            id: "owner-config-cancel".into(),
            payload: xmpp_parsers::minidom::Element::builder(
                "query",
                waddle_xmpp::muc::NS_MUC_OWNER,
            )
            .append(submit_form)
            .build(),
        };
        let owner_session = crate::auth::Session::new("alice@example.test", "alice", "alice");
        let pause = durable_store.pause_next_commit_reply();
        let task_state = Arc::clone(&state);
        let task_room = room.clone();
        let task_owner = owner.clone();
        let task_session = owner_session.clone();
        let task = tokio::spawn(async move {
            let _ = apply_muc_owner_config(
                task_state.as_ref(),
                &task_room,
                &iq,
                Some(&task_session),
                Some(&task_owner),
            )
            .await;
        });

        pause.wait_until_reached().await;
        let (lifecycle, revision) = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room)
            .expect("committed coordinates");
        let key = RoomEffectKey {
            lifecycle,
            revision: RoomRevision::from_stored(revision).expect("revision"),
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };

        task.abort();
        let _ = task.await;

        // Real time on purpose: choreographing the spawned recovery task,
        // the actor mailbox, and the sqlite pool under a paused clock is
        // race-prone. Hold the pause across the recovery's first snapshot
        // ask so it must time out and retry; total test budget stays far
        // below the CI slow threshold.
        tokio::time::sleep(OWNER_CONFIG_RECOVERY_ASK_TIMEOUT + Duration::from_millis(500)).await;
        assert_eq!(
            state
                .deps
                .protocol
                .room_effect_outbox
                .find(&key)
                .await
                .expect("still-inert row lookup")
                .expect("still-inert row")
                .available_at_ms,
            i64::MAX,
            "the timed-out recovery snapshot must not arm the row before its retry"
        );

        pause.release();

        let recovered_snapshot = room_actor
            .ask(GetSnapshot)
            .await
            .expect("snapshot after releasing paused config commit");
        assert_eq!(recovered_snapshot.config_revision, 1);
        assert_eq!(
            recovered_snapshot.config_durable_coordinates,
            Some(RoomCommittedCoordinates {
                lifecycle,
                revision: RoomRevision::from_stored(revision).expect("config revision"),
            })
        );

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .find(&key)
                    .await
                    .expect("armed row lookup")
                    .expect("armed row")
                    .available_at_ms
                    != i64::MAX
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("cancelled owner-config ask must still arm the committed row");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_owner_config_recovery_keeps_retrying_lookup_failures_until_success() {
        async fn recover_and_wait(
            guard: super::CancelledOwnerConfigAskRecoveryGuard,
            store: &crate::room_effect_outbox::RoomEffectOutboxStore,
            keys: &[RoomEffectKey],
            context: &'static str,
        ) {
            let recovery = tokio::spawn(async move {
                guard.recover().await;
            });
            // Two injected failures back off for 5s then 10s, so a 25s poll
            // budget leaves room for scheduler jitter while bounding both the
            // recovery task and the staged-row poll.
            let armed = tokio::time::timeout(Duration::from_secs(25), async {
                loop {
                    let mut armed = true;
                    for key in keys {
                        armed &= store
                            .find(key)
                            .await
                            .expect("armed row lookup")
                            .expect("armed row")
                            .available_at_ms
                            != i64::MAX;
                    }
                    if armed {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            })
            .await;
            if armed.is_err() {
                recovery.abort();
            }
            armed.expect(context);
            tokio::time::timeout(Duration::from_secs(1), recovery)
                .await
                .expect(
                    "cancelled owner-config recovery task must finish after arming the staged rows",
                )
                .expect("cancelled owner-config recovery task must not panic");
        }

        {
            let state = create_test_websocket_state().await;
            let durable_store = PausableConfigDurableStore::new(Arc::clone(
                &state.deps.protocol.room_effect_outbox,
            ));
            let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
            state
                .deps
                .protocol
                .room_registry
                .ask(WireClusteringClaims {
                    claim_store,
                    node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                        "owner-config-lookup-retry-exact-node",
                        "owner-config-lookup-retry-exact-epoch",
                    )),
                    durable_store: Some(durable_store.clone()),
                    rollout_backoff: None,
                })
                .await
                .expect("wire durable owner-config store");

            let room =
                BareJid::new("owner-config-lookup-retry-exact@muc.example.test").expect("room jid");
            let room_actor = get_or_create_room_actor(
                state.as_ref(),
                &room,
                waddle_xmpp::muc::RoomConfig::default(),
                "waddle".to_owned(),
                "channel".to_owned(),
            )
            .await
            .expect("create room")
            .actor_ref;
            room_actor
                .ask(JoinWithAffiliation {
                    sender_jid: full_jid("alice@example.test/desktop"),
                    nick: "alice".to_owned(),
                    affiliation_grant: JoinAffiliationGrant::Resolver(
                        waddle_xmpp::Affiliation::Owner,
                    ),
                    local_domain: "example.test".to_owned(),
                    admission_revision: 0,
                })
                .await
                .expect("owner join");

            let intended_config = waddle_xmpp::muc::RoomConfig {
                name: "After".to_owned(),
                ..waddle_xmpp::muc::RoomConfig::default()
            };
            let intended_applied = room_actor
                .ask(UpdateConfig {
                    config: intended_config.clone(),
                    effect_plan: ConfigEffectPlan::DirectAudience,
                })
                .await
                .expect("commit intended config");
            let intended_snapshot = room_actor
                .ask(GetSnapshot)
                .await
                .expect("intended snapshot");
            let intended_reservation = intended_applied
                .reservation
                .expect("intended staged reservation");
            let intended_key = RoomEffectKey {
                lifecycle: intended_reservation.lifecycle,
                revision: intended_reservation.revision,
                ordinal: intended_reservation.ordinals[0],
            };
            let store = state.deps.protocol.room_effect_outbox.as_ref();
            assert_eq!(
                store
                    .find(&intended_key)
                    .await
                    .expect("intended staged row lookup")
                    .expect("intended staged row")
                    .available_at_ms,
                i64::MAX,
                "the intended config row starts inert"
            );
            store.fail_staged_reservation_lookup_times_for_test(
                intended_reservation.lifecycle,
                intended_reservation.revision,
                2,
            );

            let recovery = super::CancelledOwnerConfigAskRecoveryGuard::arm_only(
                state.as_ref(),
                &room_actor,
                &room,
                &intended_config,
                intended_snapshot.config_revision,
            );

            recover_and_wait(
                recovery,
                store,
                &[intended_key],
                "cancelled owner-config recovery must retry exact staged-reservation lookups until the row arms",
            )
            .await;
        }

        {
            let state = create_test_websocket_state().await;
            let durable_store = PausableConfigDurableStore::new(Arc::clone(
                &state.deps.protocol.room_effect_outbox,
            ));
            let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
            state
                .deps
                .protocol
                .room_registry
                .ask(WireClusteringClaims {
                    claim_store,
                    node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                        "owner-config-lookup-retry-superseded-node",
                        "owner-config-lookup-retry-superseded-epoch",
                    )),
                    durable_store: Some(durable_store.clone()),
                    rollout_backoff: None,
                })
                .await
                .expect("wire durable owner-config store");

            let room = BareJid::new("owner-config-lookup-retry-superseded@muc.example.test")
                .expect("room jid");
            let room_actor = get_or_create_room_actor(
                state.as_ref(),
                &room,
                waddle_xmpp::muc::RoomConfig::default(),
                "waddle".to_owned(),
                "channel".to_owned(),
            )
            .await
            .expect("create room")
            .actor_ref;
            room_actor
                .ask(JoinWithAffiliation {
                    sender_jid: full_jid("alice@example.test/desktop"),
                    nick: "alice".to_owned(),
                    affiliation_grant: JoinAffiliationGrant::Resolver(
                        waddle_xmpp::Affiliation::Owner,
                    ),
                    local_domain: "example.test".to_owned(),
                    admission_revision: 0,
                })
                .await
                .expect("owner join");

            let intended_config = waddle_xmpp::muc::RoomConfig {
                name: "After".to_owned(),
                ..waddle_xmpp::muc::RoomConfig::default()
            };
            let intended_applied = room_actor
                .ask(UpdateConfig {
                    config: intended_config.clone(),
                    effect_plan: ConfigEffectPlan::DirectAudience,
                })
                .await
                .expect("commit intended config");
            let intended_snapshot = room_actor
                .ask(GetSnapshot)
                .await
                .expect("intended snapshot");
            let intended_reservation = intended_applied
                .reservation
                .expect("intended staged reservation");
            let intended_key = RoomEffectKey {
                lifecycle: intended_reservation.lifecycle,
                revision: intended_reservation.revision,
                ordinal: intended_reservation.ordinals[0],
            };

            let latest_applied = room_actor
                .ask(UpdateConfig {
                    config: waddle_xmpp::muc::RoomConfig {
                        name: "Latest".to_owned(),
                        ..waddle_xmpp::muc::RoomConfig::default()
                    },
                    effect_plan: ConfigEffectPlan::DirectAudience,
                })
                .await
                .expect("commit latest config");
            let latest_reservation = latest_applied
                .reservation
                .expect("latest staged reservation");
            let latest_key = RoomEffectKey {
                lifecycle: latest_reservation.lifecycle,
                revision: latest_reservation.revision,
                ordinal: latest_reservation.ordinals[0],
            };
            let store = state.deps.protocol.room_effect_outbox.as_ref();
            assert_eq!(
                store
                    .find(&intended_key)
                    .await
                    .expect("intended staged row lookup")
                    .expect("intended staged row")
                    .available_at_ms,
                i64::MAX,
                "the intended config row starts inert"
            );
            assert_eq!(
                store
                    .find(&latest_key)
                    .await
                    .expect("latest staged row lookup")
                    .expect("latest staged row")
                    .available_at_ms,
                i64::MAX,
                "the superseding config row starts inert"
            );
            store.fail_staged_reservations_up_to_lookup_times_for_test(
                latest_reservation.lifecycle,
                latest_reservation.revision,
                2,
            );

            let recovery = super::CancelledOwnerConfigAskRecoveryGuard::arm_only(
                state.as_ref(),
                &room_actor,
                &room,
                &intended_config,
                intended_snapshot.config_revision,
            );

            recover_and_wait(
                recovery,
                store,
                &[intended_key, latest_key],
                "cancelled owner-config recovery must retry superseded staged-reservation lookups until every committed row arms",
            )
            .await;
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_owner_config_recovery_superseded_config_arms_every_committed_inert_row() {
        let state = create_test_websocket_state().await;
        let durable_store =
            PausableConfigDurableStore::new(Arc::clone(&state.deps.protocol.room_effect_outbox));
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "owner-config-superseded-node",
                    "owner-config-superseded-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable owner-config store");

        let room = room_jid();
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room,
            waddle_xmpp::muc::RoomConfig::default(),
            "waddle".to_owned(),
            "channel".to_owned(),
        )
        .await
        .expect("create room")
        .actor_ref;
        let owner = full_jid("alice@example.test/desktop");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: owner,
                nick: "alice".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Owner),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("owner join");

        let intended_config = waddle_xmpp::muc::RoomConfig {
            name: "After".to_owned(),
            ..waddle_xmpp::muc::RoomConfig::default()
        };
        let intended_applied = room_actor
            .ask(UpdateConfig {
                config: intended_config.clone(),
                effect_plan: ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit intended config");
        let intended_snapshot = room_actor
            .ask(GetSnapshot)
            .await
            .expect("intended snapshot");
        let intended_reservation = intended_applied
            .reservation
            .expect("intended staged reservation");
        let intended_key = RoomEffectKey {
            lifecycle: intended_reservation.lifecycle,
            revision: intended_reservation.revision,
            ordinal: intended_reservation.ordinals[0],
        };

        let latest_config = waddle_xmpp::muc::RoomConfig {
            name: "Latest".to_owned(),
            ..waddle_xmpp::muc::RoomConfig::default()
        };
        let latest_applied = room_actor
            .ask(UpdateConfig {
                config: latest_config,
                effect_plan: ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("commit latest config");
        let latest_reservation = latest_applied
            .reservation
            .expect("latest staged reservation");
        let latest_key = RoomEffectKey {
            lifecycle: latest_reservation.lifecycle,
            revision: latest_reservation.revision,
            ordinal: latest_reservation.ordinals[0],
        };
        let store = state.deps.protocol.room_effect_outbox.as_ref();
        assert_eq!(
            store
                .find(&intended_key)
                .await
                .expect("intended staged row lookup")
                .expect("intended staged row")
                .available_at_ms,
            i64::MAX,
            "the intended config row starts inert"
        );
        assert_eq!(
            store
                .find(&latest_key)
                .await
                .expect("latest staged row lookup")
                .expect("latest staged row")
                .available_at_ms,
            i64::MAX,
            "the superseding config row starts inert"
        );

        super::CancelledOwnerConfigAskRecoveryGuard::arm_only(
            state.as_ref(),
            &room_actor,
            &room,
            &intended_config,
            intended_snapshot.config_revision,
        )
        .recover()
        .await;

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let intended_armed = store
                    .find(&intended_key)
                    .await
                    .expect("intended armed row lookup")
                    .expect("intended armed row")
                    .available_at_ms
                    != i64::MAX;
                let latest_armed = store
                    .find(&latest_key)
                    .await
                    .expect("latest armed row lookup")
                    .expect("latest armed row")
                    .available_at_ms
                    != i64::MAX;
                if intended_armed && latest_armed {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("cancelled owner-config recovery must arm every committed inert row through the latest config revision");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_owner_config_interleaved_affiliation_mutation_still_arms_config_row() {
        let state = create_test_websocket_state().await;
        let durable_store =
            PausableConfigDurableStore::new(Arc::clone(&state.deps.protocol.room_effect_outbox));
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        state
            .deps
            .protocol
            .room_registry
            .ask(WireClusteringClaims {
                claim_store,
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "owner-config-interleaved-node",
                    "owner-config-interleaved-epoch",
                )),
                durable_store: Some(durable_store.clone()),
                rollout_backoff: None,
            })
            .await
            .expect("wire durable owner-config store");

        let room = room_jid();
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room,
            waddle_xmpp::muc::RoomConfig::default(),
            "waddle".to_owned(),
            "channel".to_owned(),
        )
        .await
        .expect("create room")
        .actor_ref;
        let owner = full_jid("alice@example.test/desktop");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: owner.clone(),
                nick: "alice".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Owner),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("owner join");

        let submit_form =
            xmpp_parsers::minidom::Element::builder("x", waddle_xmpp::muc::DATA_FORMS_NS)
                .attr(
                    minidom::rxml::xml_ncname!("type").to_owned(),
                    "submit".to_owned(),
                )
                .append(
                    xmpp_parsers::minidom::Element::builder(
                        "field",
                        waddle_xmpp::muc::DATA_FORMS_NS,
                    )
                    .attr(
                        minidom::rxml::xml_ncname!("var").to_owned(),
                        "muc#roomconfig_roomname",
                    )
                    .append(
                        xmpp_parsers::minidom::Element::builder(
                            "value",
                            waddle_xmpp::muc::DATA_FORMS_NS,
                        )
                        .append("After")
                        .build(),
                    )
                    .build(),
                )
                .build();
        let iq = xmpp_parsers::iq::Iq::Set {
            from: Some(jid::Jid::from(owner.clone())),
            to: Some(jid::Jid::from(room.clone())),
            id: "owner-config-interleaved".into(),
            payload: xmpp_parsers::minidom::Element::builder(
                "query",
                waddle_xmpp::muc::NS_MUC_OWNER,
            )
            .append(submit_form)
            .build(),
        };
        let owner_session = crate::auth::Session::new("alice@example.test", "alice", "alice");
        let pause = durable_store.pause_next_commit_reply();
        let task_state = Arc::clone(&state);
        let task_room = room.clone();
        let task_owner = owner.clone();
        let task_session = owner_session.clone();
        let task = tokio::spawn(async move {
            let _ = apply_muc_owner_config(
                task_state.as_ref(),
                &task_room,
                &iq,
                Some(&task_session),
                Some(&task_owner),
            )
            .await;
        });

        pause.wait_until_reached().await;
        let (lifecycle, revision) = *durable_store
            .coordinates
            .lock()
            .expect("coordinates lock")
            .get(&room)
            .expect("committed coordinates");
        let key = RoomEffectKey {
            lifecycle,
            revision: RoomRevision::from_stored(revision).expect("revision"),
            ordinal: waddle_xmpp::muc::RoomEffectOrdinal::first(),
        };

        task.abort();
        let _ = task.await;

        // Hold the pause past the recovery's first snapshot ask so the retry
        // observes the post-interleave state deterministically (the
        // affiliation ask below queues behind the paused commit and processes
        // before the retry snapshot). Real time on purpose — see the sibling
        // snapshot-timeout test.
        tokio::time::sleep(OWNER_CONFIG_RECOVERY_ASK_TIMEOUT + Duration::from_millis(500)).await;

        pause.release();
        room_actor
            .ask(ChangeAffiliation {
                jid: BareJid::from_str("bob@example.test").expect("member bare JID"),
                affiliation: waddle_xmpp::Affiliation::Member,
            })
            .await
            .expect("interleaved affiliation mutation");
        assert_eq!(
            room_actor
                .ask(GetSnapshot)
                .await
                .expect("snapshot after interleaved affiliation")
                .durable_coordinates
                .expect("latest durable coordinates")
                .revision,
            RoomRevision::from_stored(revision + 1).expect("later revision"),
            "the interleaved affiliation mutation must advance the actor's latest durable coordinates"
        );

        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                if state
                    .deps
                    .protocol
                    .room_effect_outbox
                    .find(&key)
                    .await
                    .expect("armed row lookup")
                    .expect("armed row")
                    .available_at_ms
                    != i64::MAX
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the cancelled config recovery must arm the original config row after an interleaved affiliation revision");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_committed_config_reservation_registers_supervisor_fallback() {
        let state = create_test_websocket_state().await;
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        let reservation = enqueue_config_reservation(state.as_ref(), lifecycle, revision).await;
        let key = RoomEffectKey {
            lifecycle,
            revision,
            ordinal: reservation.ordinals[0],
        };
        let store = state.deps.protocol.room_effect_outbox.as_ref();

        assert_eq!(
            store
                .find(&key)
                .await
                .expect("staged row lookup")
                .expect("staged row present")
                .available_at_ms,
            i64::MAX,
            "committed config reservation starts inert"
        );

        {
            let _guard =
                CommittedConfigReservationGuard::new(state.as_ref(), Some(reservation.clone()));
        }

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if store
                    .find(&key)
                    .await
                    .expect("armed row lookup")
                    .expect("armed row present")
                    .available_at_ms
                    != i64::MAX
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drop-time supervisor registration must arm the staged row");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dropped_committed_config_reservation_detaches_unmanaged_members_only_enforcement() {
        let sfu = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(sfu.clone()).await;
        let room = room_jid();
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room,
            waddle_xmpp::muc::RoomConfig {
                members_only: false,
                ..waddle_xmpp::muc::RoomConfig::default()
            },
            "waddle".to_owned(),
            "channel".to_owned(),
        )
        .await
        .expect("create room")
        .actor_ref;
        let admin = full_jid("admin@example.test/desktop");
        let alice = full_jid("alice@example.test/phone");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: admin.clone(),
                nick: "admin".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Owner),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("admin join");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::None),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("alice join");
        room_actor
            .ask(UpdateConfig {
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: true,
                    ..waddle_xmpp::muc::RoomConfig::default()
                },
                effect_plan: ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("members-only config commit");
        let (_admin_tx, mut admin_rx) = {
            let (tx, rx) = mpsc::channel(8);
            let _ = register_test_connection(state.as_ref(), &admin, tx).await;
            ((), rx)
        };
        let (_alice_tx, mut alice_rx) = {
            let (tx, rx) = mpsc::channel(8);
            let _ = register_test_connection(state.as_ref(), &alice, tx).await;
            ((), rx)
        };
        let lifecycle = RoomLifecycleId::generate();
        let revision = RoomRevision::initial();
        let reservation = enqueue_config_reservation(state.as_ref(), lifecycle, revision).await;
        let key = RoomEffectKey {
            lifecycle,
            revision,
            ordinal: reservation.ordinals[0],
        };
        let store = state.deps.protocol.room_effect_outbox.as_ref();

        {
            let mut guard =
                CommittedConfigReservationGuard::new(state.as_ref(), Some(reservation.clone()));
            guard.defer_to_members_only_enforcement(PendingMembersOnlyEnforcement {
                actor: room_actor.clone(),
                request: PendingMembersOnlyEnforcementRequest::Unmanaged,
                fallback_reservation: Some(reservation.clone()),
                room_jid: room.clone(),
                connections: state.deps.protocol.connection_registry.clone(),
                sfu: state.deps.protocol.sfu.clone(),
                arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
            });
        }

        let alice_presence = tokio::time::timeout(Duration::from_secs(1), alice_rx.recv())
            .await
            .expect("alice detached removal timeout")
            .expect("alice detached removal");
        let admin_presence = tokio::time::timeout(Duration::from_secs(1), admin_rx.recv())
            .await
            .expect("admin detached removal timeout")
            .expect("admin detached removal");
        let alice_xml = stanza_to_xml(&alice_presence.stanza);
        let admin_xml = stanza_to_xml(&admin_presence.stanza);
        assert!(
            alice_xml.contains("code='322'") && alice_xml.contains("code='110'"),
            "detached unmanaged enforcement must notify the removed occupant first-class: {alice_xml}"
        );
        assert!(
            admin_xml.contains("code='322'"),
            "detached unmanaged enforcement must notify remaining occupants: {admin_xml}"
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if store
                    .find(&key)
                    .await
                    .expect("armed row lookup")
                    .expect("armed row present")
                    .available_at_ms
                    != i64::MAX
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect(
            "detached unmanaged enforcement must arm the config reservation after the 322 sweep",
        );

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if snapshot_room(state.as_ref(), &room)
                    .await
                    .room
                    .get_occupant("alice")
                    .is_none()
                {
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("detached unmanaged enforcement must evict the non-member from the room");

        assert_eq!(
            sfu.snapshot(),
            vec![(
                waddle_sfu::CallId::new(room.to_string()).expect("valid call id"),
                waddle_sfu::Identity::from_jid(alice),
            )],
            "detached unmanaged enforcement must still revoke the removed occupant from the SFU"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rollback_clears_deferred_members_only_enforcement_before_guard_drop() {
        let sfu = Arc::new(RecordingSfu::default());
        let state = create_test_websocket_state_with_sfu(sfu.clone()).await;
        let room = room_jid();
        let room_actor = get_or_create_room_actor(
            state.as_ref(),
            &room,
            waddle_xmpp::muc::RoomConfig {
                members_only: false,
                ..waddle_xmpp::muc::RoomConfig::default()
            },
            "waddle".to_owned(),
            "channel".to_owned(),
        )
        .await
        .expect("create room")
        .actor_ref;
        let admin = full_jid("admin@example.test/desktop");
        let alice = full_jid("alice@example.test/phone");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: admin.clone(),
                nick: "admin".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::Owner),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("admin join");
        room_actor
            .ask(JoinWithAffiliation {
                sender_jid: alice.clone(),
                nick: "alice".to_owned(),
                affiliation_grant: JoinAffiliationGrant::Resolver(waddle_xmpp::Affiliation::None),
                local_domain: "example.test".to_owned(),
                admission_revision: 0,
            })
            .await
            .expect("alice join");
        let previous_config = snapshot_room(state.as_ref(), &room).await.room.config;
        let applied = room_actor
            .ask(UpdateConfig {
                config: waddle_xmpp::muc::RoomConfig {
                    members_only: true,
                    ..waddle_xmpp::muc::RoomConfig::default()
                },
                effect_plan: ConfigEffectPlan::DirectAudience,
            })
            .await
            .expect("members-only config commit");
        let (_admin_tx, mut admin_rx) = {
            let (tx, rx) = mpsc::channel(8);
            let _ = register_test_connection(state.as_ref(), &admin, tx).await;
            ((), rx)
        };
        let (_alice_tx, mut alice_rx) = {
            let (tx, rx) = mpsc::channel(8);
            let _ = register_test_connection(state.as_ref(), &alice, tx).await;
            ((), rx)
        };

        {
            let mut guard =
                CommittedConfigReservationGuard::new(state.as_ref(), applied.reservation);
            guard.defer_to_members_only_enforcement(PendingMembersOnlyEnforcement {
                actor: room_actor.clone(),
                request: PendingMembersOnlyEnforcementRequest::Unmanaged,
                fallback_reservation: guard.reservation().cloned(),
                room_jid: room.clone(),
                connections: state.deps.protocol.connection_registry.clone(),
                sfu: state.deps.protocol.sfu.clone(),
                arm_supervisor: state.deps.protocol.room_effect_arm_supervisor.clone(),
            });
            guard.clear_members_only_enforcement();
            rollback_config_or_arm_reservation(
                state.as_ref(),
                &room_actor,
                applied.revision,
                previous_config.clone(),
                guard.reservation(),
            )
            .await;
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            admin_rx.try_recv().is_err(),
            "rollback must not emit stale 322/config frames to remaining occupants"
        );
        assert!(
            alice_rx.try_recv().is_err(),
            "rollback must not emit stale 322/config frames to restored occupants"
        );

        let snapshot = snapshot_room(state.as_ref(), &room).await;
        assert_eq!(
            snapshot.room.config.members_only, previous_config.members_only,
            "rollback must restore the original room config"
        );
        assert!(
            snapshot.room.get_occupant("alice").is_some(),
            "rollback must leave the restored occupant seated"
        );
        assert!(
            sfu.snapshot().is_empty(),
            "rollback must not revoke SFU access when the stale 322 sweep is cancelled"
        );
    }
}
