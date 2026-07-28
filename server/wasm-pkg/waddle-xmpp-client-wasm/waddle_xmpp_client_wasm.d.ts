/* tslint:disable */
/* eslint-disable */

/**
 * Closed synchronous pagehide result exposed to the browser binding. This is
 * deliberately a wasm enum rather than a stringly-typed transport status.
 */
export enum PagehideSmAckEnqueueOutcome {
    Sent = 0,
    AlreadyPending = 1,
    Full = 2,
    Closed = 3,
    Busy = 4,
    WriteFailed = 5,
}

export class WaddleClient {
    free(): void;
    [Symbol.dispose](): void;
    admin_channels_affiliations(args: any): Promise<any>;
    admin_channels_create(args: any): Promise<any>;
    admin_channels_delete(args: any): Promise<any>;
    admin_channels_kick(args: any): Promise<any>;
    admin_channels_list(args: any): Promise<any>;
    admin_channels_occupants(args: any): Promise<any>;
    admin_channels_set_affiliation(args: any): Promise<any>;
    admin_channels_update(args: any): Promise<any>;
    admin_spaces_create(args: any): Promise<any>;
    admin_spaces_delete(args: any): Promise<any>;
    admin_spaces_list(args: any): Promise<any>;
    admin_spaces_members(args: any): Promise<any>;
    admin_spaces_set_role(args: any): Promise<any>;
    admin_spaces_update(args: any): Promise<any>;
    /**
     * Call the `urn:waddle:admin:users:list:0` ad-hoc command
     * against the user-bearing server domain and return a typed
     * page of matching users. Errors out (rejecting the returned
     * Promise) if the server replies with a stanza error — the
     * chat client interprets `<forbidden/>` as "not the community
     * owner" and falls back to the empty-state screen.
     */
    admin_users_list(prefix?: string | null, page_size?: number | null, after_cursor?: string | null): Promise<any>;
    cancel_raw_iq(id: string): Promise<any>;
    connect(): Promise<any>;
    /**
     * XEP-0050 `disable-device` ad-hoc command on `push.<domain>`.
     * Single-step `action='execute'` carrying the `node` + `device-id`
     * fields. The Push Service marks the row inactive — no payload
     * shape returned, the caller only cares about success vs. error.
     *
     * Verifies the response's `from=` matches `service_jid` before
     * returning (RFC 6120 §8.1.2.1 / §10.5 defense-in-depth — same
     * pattern as `fetch_vapid_public_key` above).
     */
    disable_push_device(service_jid: string, node: string, device_id: string): Promise<any>;
    /**
     * XEP-0357 §6.1 `<disable/>` IQ. A `None`/missing `node` disables
     * ALL push nodes at the service for this user.
     */
    disable_push_notifications(service_jid: string, node?: string | null): Promise<any>;
    disconnect(): Promise<any>;
    discover_extension_routes(user_jid?: string | null): Promise<any>;
    discover_upload_service(): Promise<any>;
    /**
     * XEP-0357 §5 `<enable/>` IQ. No provider credentials — those
     * flow through `register_push_device` (XEP-0050) against the
     * Push Service component.
     */
    enable_push_notifications(service_jid: string, node: string): Promise<any>;
    /**
     * Fetch the latest items from the community Social Feed node on
     * `spaces_jid` — the community service (`community.<domain>`).
     * Returns an array of JsFeedEntry objects ordered as the server
     * delivered them (newest first by `last_published`).
     */
    feed_items(spaces_jid: string, max_items?: number | null): Promise<any>;
    /**
     * Publish a new entry to the community Social Feed. The server
     * enforces publish authorisation via XEP-0060 affiliations;
     * callers without Publisher access receive a Forbidden stanza
     * error which surfaces as a rejected Promise.
     */
    feed_publish(spaces_jid: string, entry: any): Promise<any>;
    /**
     * Waddle-specific MAM stanza-id filter for 1:1 history. Targets the
     * account's personal archive and constrains the query with `with=peer`.
     */
    fetch_direct_messages_by_stanza_ids(peer_jid: string, stanza_ids: string[]): Promise<any>;
    /**
     * Fetch the user's Waddle DM-bookmark items (issue #720) from PEP,
     * surfaced as a typed array carrying the XEP-0492 fallback
     * notification mode + rich-payload opt-in (#719) per direct-chat
     * contact. The DM counterpart to [`Self::fetch_user_bookmarks`].
     *
     * The carrier node `urn:waddle:dm-bookmarks:0` is sparse /
     * override-only: an item exists ONLY when the DM has an override
     * beyond the XEP-0492 §3 direct-chat default. A `None` `notify_mode`
     * means the hosted `<notify/>` carries only identity-scoped
     * siblings; the chat resolves it against the §3 default (`always`).
     *
     * Resolves to an empty array when the PEP node is absent (no DM has
     * an override yet) — XEP-0163 returns `item-not-found`, caught here
     * and treated as the empty list rather than rejecting the Promise.
     *
     * **Deferred** (same as the MUC path): no XEP-0163 §4.4 `+notify`
     * self-subscription on the DM node yet, so a change in another
     * client reaches this one on the next session-ready re-fetch.
     */
    fetch_dm_bookmarks(): Promise<any>;
    fetch_dm_history(peer_jid: string, max: number, before_id?: string | null): Promise<any>;
    /**
     * Fetch a DM thread's archived replies from the account archive,
     * filtered by `with=peer` and the Waddle MAM thread field. Mirrors
     * `fetch_room_history_by_thread`, but targets the personal archive
     * (`to=account` + `with=peer`) instead of a room. A `None` / empty
     * `before_id` requests the most-recent page; a cursor pages older
     * replies via RSM (XEP-0059).
     */
    fetch_dm_history_by_thread(peer_jid: string, thread_id: string, max: number, before_id?: string | null): Promise<any>;
    fetch_dm_history_page(peer_jid: string, max: number, page_param: any): Promise<any>;
    fetch_extension_route_items(route: any, room_jid: string): Promise<any>;
    /**
     * XEP-0215 §3.2: fetch the external services (TURN/STUN) the user's own
     * server advertises, resolving to a typed array the chat maps to
     * `RTCIceServer[]` for LiveKit's `rtcConfig` at connect time. The query is
     * addressed to the authenticated user's server domain; an empty
     * `<services/>` requests every advertised service type.
     */
    fetch_external_services(): Promise<any>;
    /**
     * Fetch the user's inbox via XEP-0430 (`urn:xmpp:inbox:1`).
     *
     * Wire-shape: IQ-get with `<inbox/>`, server streams
     * `<message><entry/></message>` per conversation, terminating
     * with `<iq type='result'><fin/></iq>`. The streaming reducer
     * lives in the wasm driver; this method registers the pending
     * inbox query, drives the IQ send, and resolves the JS promise
     * once the closing fin arrives.
     */
    fetch_inbox(opts: any): Promise<any>;
    /**
     * XEP-0490 §3.1 catch-up: retrieve every item from the user's
     * own `urn:xmpp:mds:displayed:0` PEP node. Returns an array of
     * `WaddleMdsDisplayedEntry` records. An empty array on first
     * call (no node yet) is normal and not an error.
     */
    fetch_mds_displayed(): Promise<any>;
    fetch_personal_history_page(max: number, page_param: any): Promise<any>;
    fetch_room_history(room_jid: string, max: number, before_id?: string | null): Promise<any>;
    fetch_room_history_by_thread(room_jid: string, thread_id: string, max: number, before_id?: string | null): Promise<any>;
    fetch_room_history_page(room_jid: string, max: number, page_param: any): Promise<any>;
    /**
     * Waddle-specific MAM stanza-id filter — fetch a batch of messages from
     * a room MAM archive by XEP-0359 stanza-id. Uses the custom data-form
     * var `{urn:waddle:mam-stanza-id:0}stanza-id` per XEP-0313 §4.2 +
     * XEP-0068 (not the `urn:xmpp:sid:0` namespace, which is XEP-0359
     * wire protocol only). Used by the pinned-panel rich-preview render
     * path to materialize `TimelineMessage`s for pinned entries that
     * are not in the loaded timeline window.
     */
    fetch_room_messages_by_stanza_ids(room_jid: string, stanza_ids: string[]): Promise<any>;
    /**
     * Fetch the current pinned-messages list for a MUC room (#414).
     * Resolves to a JS array of `WaddlePinEntry`. Empty array if the
     * room has no pins. Server gates on room occupancy: a non-occupant
     * caller will get a `<forbidden type='auth'/>` error which surfaces
     * here as a rejected Promise.
     */
    fetch_room_pins(room_jid: string): Promise<any>;
    /**
     * Fetch the global threads view (`urn:waddle:threads:0`).
     * Returns a `WaddleThreadsPage` (empty page on transport failure).
     */
    fetch_threads(opts: any): Promise<any>;
    /**
     * Fetch the user's XEP-0402 bookmark items from PEP, surfaced as
     * a typed array carrying the XEP-0492 fallback notification mode
     * (when present) for each room. The chat UI uses this to
     * hydrate per-chat notification controls on connect.
     *
     * Resolves to an empty array when the user's PEP `urn:xmpp:bookmarks:1`
     * node is absent (first publish hasn't happened) or empty —
     * XEP-0163 PEP returns `item-not-found` in that case, which is
     * caught here and treated as the empty list rather than
     * rejecting the Promise. Per XEP-0492 §3, the chat caller
     * resolves an empty `notify_mode` against the conversation-kind
     * default.
     *
     * **Deferred:** the conformant XEP-0163 §4.4 `+notify` self-
     * subscription on `urn:xmpp:bookmarks:1` would push every other
     * client's bookmark publish to this client as a `<message>`
     * headline. Without it, the chat re-fetches on every fresh
     * session-ready (see `notifySettingsStore.hydrate` wiring at
     * `chat/src/shell/chat-app-controller.ts`); a setting changed
     * in another tab reaches this tab only on the next reconnect.
     * Wiring the headline route is a meaningful slice of new WASM
     * plumbing and lands in a separate PR.
     */
    fetch_user_bookmarks(): Promise<any>;
    fetch_user_pep_profile(jid: string): Promise<any>;
    /**
     * Fetch the Push Service's currently-active VAPID public key + kid
     * via the XEP-0128 disco extension form
     * (`FORM_TYPE='urn:waddle:push:vapid:0'`). The chat passes
     * `publicKey` (base64url-no-pad uncompressed SEC1 P-256, 65 bytes
     * with leading 0x04) to `pushManager.subscribe({ applicationServerKey })`
     * and tracks `kid` so silent rotations on a server-side key change
     * re-subscribe transparently.
     *
     * Resolves to `null` when the server is reachable but does not
     * advertise the form (Web Push not configured on this deployment)
     * so the chat caller can branch into the "foreground-only"
     * fallback. Rejects on transport / stanza errors and on a
     * malformed advertisement — the chat MUST NOT degrade silently on
     * a malformed key (round-trip with the browser would fail anyway,
     * later and less diagnosable).
     */
    fetch_vapid_public_key(service_jid: string): Promise<any>;
    fetch_vcard4(jid: string): Promise<any>;
    get_resume_state(): any;
    get_resume_state_handle(): WaddleResumeState | undefined;
    get_server_version(): Promise<any>;
    /**
     * `true` iff the authenticated user is the community owner — i.e.
     * the server accepts a probe of the admin Users command. Any
     * stanza error (including `<forbidden/>`) resolves to `false`;
     * the wasm boundary doesn't try to distinguish "not owner" from
     * "server error" because the admin panel's empty state is the
     * right fallback in either case.
     */
    is_community_owner(): Promise<any>;
    /**
     * Join a MUC room. Always requests zero discussion history
     * (`<history maxstanzas='0'/>`, XEP-0045 §7.2.15): MAM catch-up is
     * the authoritative history source, so accepting the service's
     * default join history would double-deliver recent messages (#1255).
     * The canonical presence shape lives in
     * [`waddle_xmpp_client::messaging::build_muc_join_presence`].
     */
    join_room(room_jid: string, nick: string): Promise<any>;
    leave_room(room_jid: string, nick: string): Promise<any>;
    list_room_members(room_jid: string, affiliation: string): Promise<any>;
    list_roster_contacts(): Promise<any>;
    mark_inbox_read(partner_jid: string, thread_id?: string | null): Promise<any>;
    constructor(config: WaddleConfig);
    /**
     * Publish a 1:1 DM pin request.
     */
    pin_direct_message(peer_jid: string, target_stanza_id: string): Promise<any>;
    /**
     * Publish a pin request (#414). `room_jid` is the bare MUC JID;
     * `target_stanza_id` is the XEP-0359 `by=room` stanza-id of the
     * message to pin. Server gates on Owner/Admin affiliation; a
     * non-admin sender will receive a `<forbidden type='auth'/>`
     * reply via the inbound message stream.
     */
    pin_message(room_jid: string, target_stanza_id: string): Promise<any>;
    publish_activity(activity_json: any): Promise<any>;
    /**
     * XEP-0490 §3 publish to `urn:xmpp:mds:displayed:0`. `chat_id` is
     * the JID of the chat (bare DM contact, bare MUC room, or full MUC
     * occupant for a private message) which becomes the PEP item id;
     * `stanza_id` is the XEP-0359 id of the latest
     * displayed message; `stanza_id_by` is the resource-less room or
     * account-server authority required by XEP-0490. The publish carries the spec-mandated
     * publish-options as preconditions.
     */
    publish_mds_displayed(chat_id: string, stanza_id: string, stanza_id_by: string): Promise<any>;
    publish_mood(mood_json: any): Promise<any>;
    publish_tune(tune_json: any): Promise<any>;
    publish_vcard4(vcard_json: any): Promise<any>;
    /**
     * XEP-0050 `register-device` ad-hoc command on `push.<domain>`.
     * Drives the multi-step dance and resolves to the assigned
     * XEP-0357 node id. Polymorphic over Web Push / APNs / FCM via
     * the `platform`-discriminated [`RegisterPushDeviceOptions`].
     *
     * Replaces the pre-cutover `ensure_push_node` +
     * `register_web_push_device` pair: the XEP-0050 result form
     * carries the assigned node id directly.
     */
    register_push_device(options: any): Promise<any>;
    request_avatar(jid: string): Promise<any>;
    /**
     * Best-effort XEP-0198 acknowledgement request for synchronous browser
     * pagehide. The Rust runtime produces the typed `<r/>` control element.
     */
    request_stream_management_ack(): Promise<any>;
    request_upload_slot(service_jid: string, filename: string, size: bigint, content_type: string): Promise<any>;
    retract_activity(): Promise<any>;
    retract_mood(): Promise<any>;
    retract_tune(): Promise<any>;
    search_dm_history(peer_jid: string, query: string, max: number): Promise<any>;
    search_room_history(room_jid: string, query: string, max: number): Promise<any>;
    search_users(query: string): Promise<any>;
    send_call_finish(peer_full_jid: string, sid_str: string): Promise<any>;
    send_call_finish_migrated(peer_full_jid: string, old_sid_str: string, new_sid_str: string): Promise<any>;
    send_call_proceed(peer_full_jid: string, sid_str: string): Promise<any>;
    /**
     * Send a JMI `<propose/>` to the peer's bare JID (XEP-0353
     * §5.1.1). The bare JID lets the responder's server ring every
     * connected resource until one of them proceeds/rejects.
     */
    send_call_propose(peer_bare_jid: string, sid_str: string, audio: boolean, video: boolean): Promise<any>;
    send_call_reject(peer_full_jid: string, sid_str: string): Promise<any>;
    send_call_reject_tie_break(peer_full_jid: string, sid_str: string): Promise<any>;
    /**
     * Send a JMI `<retract/>` to the peer's bare JID (XEP-0353 §3).
     * Like `<propose/>`, retract is addressed to the BARE JID so the
     * responder's server can fan the disavowal out to every resource
     * that may have seen the original ring. Typing the parameter as a
     * bare JID (and routing through `message_with_jmi_to_bare`, which
     * rejects a resource-bearing JID) enforces that on the wire — the
     * resource-targeted variant is `send_call_retract_tie_break`.
     */
    send_call_retract(peer_bare_jid: string, sid_str: string): Promise<any>;
    send_call_retract_tie_break(peer_full_jid: string, sid_str: string): Promise<any>;
    /**
     * Send a JMI `<ringing/>` to the caller's bare JID (XEP-0353
     * §3.2). The bare JID lets the initiator's server fan out the
     * responder's device-ring state to every caller resource.
     */
    send_call_ringing(peer_bare_jid: string, sid_str: string): Promise<any>;
    /**
     * Send a Jingle `session-accept` IQ in response to a received
     * session-initiate. `responder` is validated as a full JID at
     * the wasm boundary so a malformed JID surfaces
     * as a clear `JsError` rather than a wire-rejected stanza.
     */
    send_call_session_accept(peer_full_jid: string, responder_full_jid: string, sid_str: string, audio: boolean, video: boolean): Promise<any>;
    /**
     * Send a Jingle `session-initiate` IQ to the peer's full JID
     * (XEP-0166 §6.4). The `initiator` attribute names the call
     * originator; the server's Jingle handler additionally
     * validates the authenticated session matches it.
     */
    send_call_session_initiate(peer_full_jid: string, initiator_full_jid: string, sid_str: string, audio: boolean, video: boolean): Promise<any>;
    send_call_session_terminate(peer_full_jid: string, sid_str: string, reason?: string | null): Promise<any>;
    send_call_session_terminate_with_outcome(peer_full_jid: string, sid_str: string, reason?: string | null): Promise<any>;
    send_chat_message(peer_jid: string, body: string, options: any): Promise<any>;
    send_chat_state(to: string, msg_type: string, state: string, thread_id?: string | null, thread_parent?: string | null): Promise<any>;
    send_correction(to: string, msg_type: string, body: string, replaces_id: string, options: any): Promise<any>;
    send_displayed(to: string, msg_type: string, message_id: string, thread_id?: string | null, thread_parent?: string | null): Promise<any>;
    send_groupchat_message(room_jid: string, body: string, options: any): Promise<any>;
    send_in_call_reaction(to: string, msg_type: string, sid: string, emoji: string): Promise<any>;
    send_moderation(to: string, msg_type: string, target_id: string, reason?: string | null): Promise<any>;
    /**
     * Send a Jingle `session-terminate` IQ to hang up. `reason` is
     * one of the XEP-0166 §7.4 condition names ("success",
     * "decline", "cancel", "busy", "gone", …) parsed against
     * `xmpp_parsers::jingle::Reason`; unknown values are rejected
     * at the wasm boundary so a typo can't ship a malformed
     * condition over the wire.
     * Send a XEP-0272 Muji-bearing Jingle `session-initiate` IQ to
     * the SFU mixer (`calls.<server-domain>`) to join the room's
     * group call, and return the issued LiveKit credentials as a
     * typed `{ url, room, identity, token }` object.
     *
     * Wire shape (XEP-0166 + XEP-0272 §Joining):
     *
     * ```xml
     * <iq type='set' to='calls.<domain>' id='…'>
     *   <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate'
     *           sid='ATTEMPT_ID'>
     *     <muji xmlns='urn:xmpp:jingle:muji:0' room='ROOM_JID'/>
     *     <content creator='initiator' name='audio' senders='both'>
     *       <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'/>
     *       <transport xmlns='urn:waddle:transports:livekit:0'/>
     *     </content>
     *     [<content creator='initiator' name='video'>…</content>]
     *   </jingle>
     * </iq>
     * ```
     *
     * Convention: `sid` is a per-attempt correlation id while
     * `<muji room='…'/>` remains the stable SFU room identity.
     * This lets the chat UI ignore a stale accept from a cancelled
     * same-room retry without changing XEP-0272 room semantics.
     *
     * `video` opt-in mirrors the call store's `media.video`
     * flag — audio is always advertised (the call wouldn't be
     * useful otherwise); video is included only when the user
     * asked for it. LiveKit handles the actual codec selection
     * once connected, so the descriptions are minimal.
     */
    send_muji_session_initiate(room_jid: string, sid_str: string, video: boolean): Promise<any>;
    /**
     * Send a XEP-0272 Muji-bearing Jingle `session-terminate` IQ
     * to the SFU mixer (`calls.<server-domain>`). Server
     * unregisters the participant + revokes every jti it minted
     * for `(room, identity)`. Resolves with no payload.
     *
     * Wire shape (XEP-0166 §6.7):
     *
     * ```xml
     * <iq type='set' to='calls.<domain>' id='…'>
     *   <jingle xmlns='urn:xmpp:jingle:1' action='session-terminate'
     *           sid='PER_ATTEMPT_SID'>
     *     <muji xmlns='urn:xmpp:jingle:muji:0' room='ROOM_JID'/>
     *     <reason><success/></reason>
     *   </jingle>
     * </iq>
     * ```
     * `iq_id` is caller-supplied (and validated against the
     * [`waddle_xmpp_client::request::StanzaId`] invariant) so the JS
     * side can cancel the still-pending or deferred IQ via
     * [`WaddleClient::cancel_raw_iq`] when its own teardown deadline
     * expires — a stale room-scoped terminate must not linger in the
     * driver after the room has been re-occupied (#1606 review).
     */
    send_muji_session_terminate(room_jid: string, sid_str: string, iq_id: string): Promise<any>;
    /**
     * Publish the user's own presence (RFC 6121 §4.7). `show` is an RFC
     * 6121 `<show>` value (`away` / `xa` / `dnd` / `chat`) or `None` for
     * plain Available (the absence of a `<show>`); `status` is the optional
     * free-text line; `idle_since` is the XEP-0319 last-interaction instant as
     * an xs:dateTime string (auto-away only), or `None` on a return-from-idle.
     * Shares `build_presence_stanza` with the native + FFI `send_presence`, so
     * every Waddle client emits byte-identical presence including the XEP-0115
     * caps advertisement.
     */
    send_presence(status?: string | null, show?: string | null, idle_since?: string | null): Promise<any>;
    send_raw_iq(xml: string): Promise<any>;
    send_reaction(to: string, msg_type: string, target_id: string, emojis: string[], thread_id?: string | null, thread_parent?: string | null): Promise<any>;
    send_retraction(to: string, msg_type: string, retracts_id: string, thread_id?: string | null, thread_parent?: string | null): Promise<any>;
    /**
     * Set the per-DM XEP-0492 notification mode for one direct-chat
     * contact by merging into the Waddle DM-bookmark carrier (issue
     * #720). The DM counterpart to [`Self::set_room_notification_mode`].
     *
     * Semantics:
     * * Parse [`SetDmNotificationModeOptions`]; `dmJid` MUST parse as a
     *   `BareJid` with a localpart, else a typed JS error (the PEP item
     *   id is the contact bare JID).
     * * Fetch existing DM items (first-publish `item-not-found` →
     *   empty); find the one whose id == `dmJid` and read its hosted
     *   `<notify/>` via [`read_dm_bookmark_notify`].
     * * Merge the new mode via [`merge_notify`] (the single §3-conformant
     *   core shared with the MUC carrier — foreign `<advanced/>` and
     *   identity-scoped siblings preserved verbatim; rich opt-in (#719)
     *   toggled).
     * * The node is sparse / override-only: if the merged `<notify/>`
     *   collapses to the §3 direct-chat default (`always`, no opt-in, no
     *   foreign `<advanced/>`) per [`dm_notify_is_default`], RETRACT the
     *   item and resolve to `Removed` instead of publishing. Otherwise
     *   publish and resolve to `Ok` with the surfaced item — the chat
     *   reconciles without a refetch.
     * * `precondition-not-met` maps to the same `NodeConfigMismatch`
     *   outcome the room path uses; other stanza errors map to `Error`
     *   (the condition stays on the Rust side as a `tracing::warn`).
     */
    set_dm_notification_mode(options: any): Promise<any>;
    /**
     * Register a callback for inbound XMPP-native call events
     * (XEP-0353 JMI envelopes + XEP-0166 Jingle session control
     * carrying a `urn:waddle:transports:livekit:0` transport).
     * The callback receives a [`WaddleCallEvent`]-shaped object
     * with a `kind` discriminator and optional `media` / `join` /
     * `reason` fields. See `messaging::call::parse_call_event` on
     * the Rust side for the typed input.
     */
    set_on_call(cb: Function): void;
    set_on_connected(cb: Function): void;
    set_on_disconnected(cb: Function): void;
    set_on_error(cb: Function): void;
    /**
     * XEP-0490 §3.2 PEP event handler. Invoked once per displayed
     * item carried in an inbound `urn:xmpp:mds:displayed:0` PEP
     * event, with a `WaddleMdsDisplayedEntry`-shaped JS value.
     */
    set_on_mds_displayed(cb: Function): void;
    set_on_message(cb: Function): void;
    set_on_message_delivery_acked(cb: Function): void;
    set_on_message_delivery_failed(cb: Function): void;
    set_on_presence(cb: Function): void;
    /**
     * Generic XEP-0060 pubsub event handler. Invoked once per
     * inbound `<items/>` event with a `WaddlePubsubEvent`-shaped JS
     * value.
     */
    set_on_pubsub_event(cb: Function): void;
    set_on_session_lifecycle(cb: Function): void;
    /**
     * Register closed, bounded XEP-0198 lifecycle outcomes. The callback
     * deliberately receives no stanza XML, stream identifiers, or counters.
     */
    set_on_stream_management(cb: Function): void;
    set_room_affiliation(room_jid: string, jid: string, affiliation: string): Promise<any>;
    /**
     * Set the per-chat XEP-0492 notification mode for one room by
     * merging into the user's XEP-0402 bookmark for that room. If no
     * bookmark exists yet for `room_jid`, one is created with
     * `autojoin=false` so this call doesn't change join behavior.
     *
     * Semantics:
     * * Fetch existing PEP bookmarks (XEP-0402 §2). A missing PEP
     *   node (`item-not-found`) is treated as empty rather than a
     *   hard error — the user's first XEP-0492 publish creates the
     *   node via XEP-0060 publish-options.
     * * Find the item whose id matches `room_jid`; if missing,
     *   construct a fresh item with the given `name` (or `None`).
     * * Replace the fallback `<notify/>` child via
     *   [`merge_notify_into_extensions`] — foreign `<advanced/>`
     *   children and identity-scoped siblings written by other
     *   clients are preserved verbatim (XEP-0492 §3). The same call
     *   toggles the Waddle rich XEP-0357 push-summary opt-in
     *   (`options.richPayloadOptIn`, #719) inside the fallback's
     *   `<advanced/>`.
     * * Publish the merged item back.
     *
     * Resolves to the new [`WaddleBookmarkItem`] so the chat UI can
     * reconcile its store without a follow-up fetch.
     */
    set_room_notification_mode(options: any): Promise<any>;
    /**
     * Fetch the user's stored preference from their own PEP node.
     * Resolves to `null` when nothing is stored.
     *
     * Per XEP-0223 §Security Considerations (CVE-2023-28686), the
     * result IQ's `from` MUST be absent or the account's bare JID;
     * anything else is treated as spoofed and yields `null`.
     */
    status_preference_fetch(): Promise<any>;
    /**
     * Publish the user's picked presence mode. Overwrites the single
     * `current` item on every call (reset publishes `mode='automatic'`
     * rather than retracting, so the change fans out to other devices).
     */
    status_preference_publish(input: any): Promise<any>;
    /**
     * Fetch the latest stories from the community stories node on
     * `community_jid`. Returns ALL items including expired ones —
     * the chat filters active vs expired locally so a story fades
     * out as the countdown hits zero without a server roundtrip.
     */
    stories_items(community_jid: string, max_items?: number | null): Promise<any>;
    /**
     * Publish a new story. `media_url` is required by XEP-0501; `body`
     * is optional text content attached to that media. `expiry_hours`
     * defaults to 24.
     */
    stories_publish(community_jid: string, input: any): Promise<any>;
    /**
     * Fetch the latest read-state from the user's own PEP node.
     *
     * Per XEP-0223 §Security Considerations (CVE-2023-28686), the
     * result IQ's `from` attribute MUST be either absent or equal to
     * the account's bare JID. Anything else is treated as spoofed
     * and produces an empty result.
     */
    story_reads_fetch(): Promise<any>;
    /**
     * Publish the user's read-state. Overwrites the single `current`
     * item on every call.
     */
    story_reads_publish(input: any): Promise<any>;
    /**
     * XEP-0060 explicit subscribe to the MDS node, used as a
     * fallback path for receiving `+notify` events when the chat
     * client's presence does not yet carry XEP-0115 caps. The
     * subscriber JID is derived from the bound session JID (bare).
     */
    subscribe_mds_displayed(): Promise<any>;
    subscribe_to_presence(peer_jid: string): Promise<any>;
    supports_mds_publish_options(): Promise<any>;
    /**
     * Pagehide-only XEP-0198 acknowledgement handoff. This synchronously
     * drains the admitted FIFO and writes the typed `<r/>` through the
     * driver's exact socket before returning; it never awaits capacity or a
     * driver turn. Callers must persist immediately after every outcome.
     */
    try_request_stream_management_ack_for_pagehide(): PagehideSmAckEnqueueOutcome;
    /**
     * Publish a 1:1 DM unpin request.
     */
    unpin_direct_message(peer_jid: string, target_stanza_id: string): Promise<any>;
    /**
     * Publish an unpin request (#414). Same authorization rules as
     * [`Self::pin_message`].
     */
    unpin_message(room_jid: string, target_stanza_id: string): Promise<any>;
    /**
     * Update the occupant's MUC Muji presence (XEP-0272).
     *
     * - `active=false` (and no preparing): emits a bare `<presence/>`
     *   without any `<muji/>` child — XEP-0272 §Leaving says the
     *   absence of the element IS the leave marker.
     * - `active=true`: emits a Muji presence advertising `<content>`
     *   children for audio (always) and video (when `video=true`).
     * - `preparing=true`: emits a `<preparing/>` sentinel per XEP-0272
     *   §Joining two-phase flow. Typically the client sends this
     *   first, awaits the room's echo, then re-emits with contents
     *   declared.
     * - `in_call`: a plain JS object `{ handRaised, muted }`
     *   ([`InCallPresenceFlags`]) appended as an `<in-call
     *   xmlns='urn:waddle:in-call:0'>` presence child *alongside*
     *   `<muji/>` (#1029 raised hand / #1030 mute), one marker child per
     *   set flag. This is the FFI in-call "set method": the caller
     *   re-emits its current call presence with the flags toggled, and
     *   the absence of a marker clears that sub-state for everyone (the
     *   server drops the stored state). Ignored unless the occupant is in
     *   the call (`active` or `preparing`), since in-call state is
     *   meaningless without call participation.
     */
    update_muji_presence(room_jid: string, nick: string, active: boolean, preparing: boolean, video: boolean, in_call: { handRaised?: boolean; muted?: boolean }): Promise<any>;
    /**
     * Fetch the latest calendar events from the community events
     * node. Returns ALL items including past events; chat-side
     * composables filter by DTSTART for upcoming-only views.
     */
    xcal_items(community_jid: string, max_items?: number | null): Promise<any>;
    /**
     * Publish a new calendar event, optionally with an RRULE for
     * recurrence. SUMMARY is required (per RFC 5545); DTSTART is
     * required for the event to be useful on a timeline.
     */
    xcal_publish(community_jid: string, input: any): Promise<any>;
    /**
     * Publish (or replace) a full CalendarItem at the given item
     * id — master event plus optional per-instance overrides and
     * EXDATE cancellations. Use this for the read-modify-write
     * flows ("edit this occurrence", "edit all occurrences",
     * "cancel this occurrence") after fetching current state via
     * `xcal_items`. Passing an existing item id overwrites that
     * item atomically; passing a new id creates a new item.
     */
    xcal_publish_item(community_jid: string, item_id: string, input: any): Promise<any>;
    /**
     * Retract a calendar item from the events node. Used for
     * "cancel entire series" — removes the master plus any
     * overrides in one shot.
     */
    xcal_retract(community_jid: string, item_id: string): Promise<any>;
    /**
     * Publish (or update) this session's RSVP for a calendar event.
     * `partstat` must be one of "ACCEPTED" | "DECLINED" | "TENTATIVE"
     * | "NEEDS-ACTION". The chat groups sibling `-rsvp-*` items back
     * into the master event on the next items fetch.
     */
    xcal_rsvp(community_jid: string, master_uid: string, self_localpart: string, self_jid: string, partstat: string): Promise<any>;
}

export class WaddleConfig {
    free(): void;
    [Symbol.dispose](): void;
    constructor(server_url: string, jid: string, access_token: string, resource: string);
    with_resume_state(previd: string, inbound_h: number, outbound_h: number): void;
    with_resume_state_entries(previd: string, inbound_h: number, outbound_h: number, entries: any): void;
    with_resume_state_entries_with_max(previd: string, inbound_h: number, outbound_h: number, entries: any, max_resume_seconds: number): void;
    with_resume_state_handle(state: WaddleResumeState): void;
    with_resume_state_with_max(previd: string, inbound_h: number, outbound_h: number, max_resume_seconds: number): void;
}

export class WaddleResumeState {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
}

/**
 * Compute a CSS `hsl(...)` string for `input` using XEP-0392 with
 * custom saturation/lightness (percentages, 0.0–100.0). The CVD
 * correction is "none" — pass the hue back through `xep0392_consistent_hue`
 * and `apply_cvd_correction` in a future iteration if CVD modes
 * become a user preference.
 */
export function xep0392_consistent_color(input: string, saturation: number, lightness: number): string;

/**
 * Compute the XEP-0392 consistent-color hue (0.0–360.0) for `input`.
 *
 * Stateless free function — does not require a WaddleClient instance.
 * JS callers receive a Number.
 */
export function xep0392_consistent_hue(input: string): number;

export default function init(): Promise<void>;
