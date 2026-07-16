export class WaddleClient {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WaddleClientFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_waddleclient_free(ptr, 0);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_affiliations(args) {
        const ret = wasm.waddleclient_admin_channels_affiliations(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_create(args) {
        const ret = wasm.waddleclient_admin_channels_create(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_delete(args) {
        const ret = wasm.waddleclient_admin_channels_delete(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_kick(args) {
        const ret = wasm.waddleclient_admin_channels_kick(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_list(args) {
        const ret = wasm.waddleclient_admin_channels_list(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_occupants(args) {
        const ret = wasm.waddleclient_admin_channels_occupants(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_set_affiliation(args) {
        const ret = wasm.waddleclient_admin_channels_set_affiliation(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_channels_update(args) {
        const ret = wasm.waddleclient_admin_channels_update(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_spaces_create(args) {
        const ret = wasm.waddleclient_admin_spaces_create(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_spaces_delete(args) {
        const ret = wasm.waddleclient_admin_spaces_delete(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_spaces_list(args) {
        const ret = wasm.waddleclient_admin_spaces_list(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_spaces_members(args) {
        const ret = wasm.waddleclient_admin_spaces_members(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_spaces_set_role(args) {
        const ret = wasm.waddleclient_admin_spaces_set_role(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * @param {any} args
     * @returns {Promise<any>}
     */
    admin_spaces_update(args) {
        const ret = wasm.waddleclient_admin_spaces_update(this.__wbg_ptr, addHeapObject(args));
        return takeObject(ret);
    }
    /**
     * Call the `urn:waddle:admin:users:list:0` ad-hoc command
     * against the user-bearing server domain and return a typed
     * page of matching users. Errors out (rejecting the returned
     * Promise) if the server replies with a stanza error — the
     * chat client interprets `<forbidden/>` as "not the community
     * owner" and falls back to the empty-state screen.
     * @param {string | null} [prefix]
     * @param {number | null} [page_size]
     * @param {string | null} [after_cursor]
     * @returns {Promise<any>}
     */
    admin_users_list(prefix, page_size, after_cursor) {
        var ptr0 = isLikeNone(prefix) ? 0 : passStringToWasm0(prefix, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(after_cursor) ? 0 : passStringToWasm0(after_cursor, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_admin_users_list(this.__wbg_ptr, ptr0, len0, isLikeNone(page_size) ? Number.MAX_SAFE_INTEGER : (page_size) >>> 0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} id
     * @returns {Promise<any>}
     */
    cancel_raw_iq(id) {
        const ptr0 = passStringToWasm0(id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_cancel_raw_iq(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    connect() {
        const ret = wasm.waddleclient_connect(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * XEP-0050 `disable-device` ad-hoc command on `push.<domain>`.
     * Single-step `action='execute'` carrying the `node` + `device-id`
     * fields. The Push Service marks the row inactive — no payload
     * shape returned, the caller only cares about success vs. error.
     *
     * Verifies the response's `from=` matches `service_jid` before
     * returning (RFC 6120 §8.1.2.1 / §10.5 defense-in-depth — same
     * pattern as `fetch_vapid_public_key` above).
     * @param {string} service_jid
     * @param {string} node
     * @param {string} device_id
     * @returns {Promise<any>}
     */
    disable_push_device(service_jid, node, device_id) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(device_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_disable_push_device(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * XEP-0357 §6.1 `<disable/>` IQ. A `None`/missing `node` disables
     * ALL push nodes at the service for this user.
     * @param {string} service_jid
     * @param {string | null} [node]
     * @returns {Promise<any>}
     */
    disable_push_notifications(service_jid, node) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(node) ? 0 : passStringToWasm0(node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_disable_push_notifications(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    disconnect() {
        const ret = wasm.waddleclient_disconnect(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string | null} [user_jid]
     * @returns {Promise<any>}
     */
    discover_extension_routes(user_jid) {
        var ptr0 = isLikeNone(user_jid) ? 0 : passStringToWasm0(user_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_discover_extension_routes(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    discover_upload_service() {
        const ret = wasm.waddleclient_discover_upload_service(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Permanently retire this wrapper. Disposal is idempotent and clears the
     * command sender and every JS callback before any queued driver event can
     * re-enter application code.
     */
    dispose() {
        wasm.waddleclient_dispose(this.__wbg_ptr);
    }
    /**
     * XEP-0357 §5 `<enable/>` IQ. No provider credentials — those
     * flow through `register_push_device` (XEP-0050) against the
     * Push Service component.
     * @param {string} service_jid
     * @param {string} node
     * @returns {Promise<any>}
     */
    enable_push_notifications(service_jid, node) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_enable_push_notifications(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Fetch the latest items from the community Social Feed node on
     * `spaces_jid` (typically `spaces.<domain>`). Returns an array of
     * JsFeedEntry objects ordered as the server delivered them
     * (newest first by `last_published`).
     * @param {string} spaces_jid
     * @param {number | null} [max_items]
     * @returns {Promise<any>}
     */
    feed_items(spaces_jid, max_items) {
        const ptr0 = passStringToWasm0(spaces_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_feed_items(this.__wbg_ptr, ptr0, len0, isLikeNone(max_items) ? Number.MAX_SAFE_INTEGER : (max_items) >>> 0);
        return takeObject(ret);
    }
    /**
     * Publish a new entry to the community Social Feed. The server
     * enforces publish authorisation via XEP-0060 affiliations;
     * callers without Publisher access receive a Forbidden stanza
     * error which surfaces as a rejected Promise.
     * @param {string} spaces_jid
     * @param {any} entry
     * @returns {Promise<any>}
     */
    feed_publish(spaces_jid, entry) {
        const ptr0 = passStringToWasm0(spaces_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_feed_publish(this.__wbg_ptr, ptr0, len0, addHeapObject(entry));
        return takeObject(ret);
    }
    /**
     * Waddle-specific MAM stanza-id filter for 1:1 history. Targets the
     * account's personal archive and constrains the query with `with=peer`.
     * @param {string} peer_jid
     * @param {string[]} stanza_ids
     * @returns {Promise<any>}
     */
    fetch_direct_messages_by_stanza_ids(peer_jid, stanza_ids) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(stanza_ids, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_direct_messages_by_stanza_ids(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
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
     * @returns {Promise<any>}
     */
    fetch_dm_bookmarks() {
        const ret = wasm.waddleclient_fetch_dm_bookmarks(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_jid
     * @param {number} max
     * @param {string | null} [before_id]
     * @returns {Promise<any>}
     */
    fetch_dm_history(peer_jid, max, before_id) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(before_id) ? 0 : passStringToWasm0(before_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_dm_history(this.__wbg_ptr, ptr0, len0, max, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Fetch a DM thread's archived replies from the account archive,
     * filtered by `with=peer` and the Waddle MAM thread field. Mirrors
     * `fetch_room_history_by_thread`, but targets the personal archive
     * (`to=account` + `with=peer`) instead of a room. A `None` / empty
     * `before_id` requests the most-recent page; a cursor pages older
     * replies via RSM (XEP-0059).
     * @param {string} peer_jid
     * @param {string} thread_id
     * @param {number} max
     * @param {string | null} [before_id]
     * @returns {Promise<any>}
     */
    fetch_dm_history_by_thread(peer_jid, thread_id, max, before_id) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(before_id) ? 0 : passStringToWasm0(before_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_dm_history_by_thread(this.__wbg_ptr, ptr0, len0, ptr1, len1, max, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_jid
     * @param {number} max
     * @param {any} page_param
     * @returns {Promise<any>}
     */
    fetch_dm_history_page(peer_jid, max, page_param) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_dm_history_page(this.__wbg_ptr, ptr0, len0, max, addHeapObject(page_param));
        return takeObject(ret);
    }
    /**
     * @param {any} route
     * @param {string} room_jid
     * @returns {Promise<any>}
     */
    fetch_extension_route_items(route, room_jid) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_extension_route_items(this.__wbg_ptr, addHeapObject(route), ptr0, len0);
        return takeObject(ret);
    }
    /**
     * XEP-0215 §3.2: fetch the external services (TURN/STUN) the user's own
     * server advertises, resolving to a typed array the chat maps to
     * `RTCIceServer[]` for LiveKit's `rtcConfig` at connect time. The query is
     * addressed to the authenticated user's server domain; an empty
     * `<services/>` requests every advertised service type.
     * @returns {Promise<any>}
     */
    fetch_external_services() {
        const ret = wasm.waddleclient_fetch_external_services(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Fetch the user's inbox via XEP-0430 (`urn:xmpp:inbox:1`).
     *
     * Wire-shape: IQ-get with `<inbox/>`, server streams
     * `<message><entry/></message>` per conversation, terminating
     * with `<iq type='result'><fin/></iq>`. The streaming reducer
     * lives in the wasm driver; this method registers the pending
     * inbox query, drives the IQ send, and resolves the JS promise
     * once the closing fin arrives.
     * @param {any} opts
     * @returns {Promise<any>}
     */
    fetch_inbox(opts) {
        const ret = wasm.waddleclient_fetch_inbox(this.__wbg_ptr, addHeapObject(opts));
        return takeObject(ret);
    }
    /**
     * XEP-0490 §3.1 catch-up: retrieve every item from the user's
     * own `urn:xmpp:mds:displayed:0` PEP node. Returns an array of
     * `WaddleMdsDisplayedEntry` records. An empty array on first
     * call (no node yet) is normal and not an error.
     * @returns {Promise<any>}
     */
    fetch_mds_displayed() {
        const ret = wasm.waddleclient_fetch_mds_displayed(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {number} max
     * @param {any} page_param
     * @returns {Promise<any>}
     */
    fetch_personal_history_page(max, page_param) {
        const ret = wasm.waddleclient_fetch_personal_history_page(this.__wbg_ptr, max, addHeapObject(page_param));
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {number} max
     * @param {string | null} [before_id]
     * @returns {Promise<any>}
     */
    fetch_room_history(room_jid, max, before_id) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(before_id) ? 0 : passStringToWasm0(before_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_room_history(this.__wbg_ptr, ptr0, len0, max, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {string} thread_id
     * @param {number} max
     * @param {string | null} [before_id]
     * @returns {Promise<any>}
     */
    fetch_room_history_by_thread(room_jid, thread_id, max, before_id) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(before_id) ? 0 : passStringToWasm0(before_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_room_history_by_thread(this.__wbg_ptr, ptr0, len0, ptr1, len1, max, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {number} max
     * @param {any} page_param
     * @returns {Promise<any>}
     */
    fetch_room_history_page(room_jid, max, page_param) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_room_history_page(this.__wbg_ptr, ptr0, len0, max, addHeapObject(page_param));
        return takeObject(ret);
    }
    /**
     * Waddle-specific MAM stanza-id filter — fetch a batch of messages from
     * a room MAM archive by XEP-0359 stanza-id. Uses the custom data-form
     * var `{urn:waddle:mam-stanza-id:0}stanza-id` per XEP-0313 §4.2 +
     * XEP-0068 (not the `urn:xmpp:sid:0` namespace, which is XEP-0359
     * wire protocol only). Used by the pinned-panel rich-preview render
     * path to materialize `TimelineMessage`s for pinned entries that
     * are not in the loaded timeline window.
     * @param {string} room_jid
     * @param {string[]} stanza_ids
     * @returns {Promise<any>}
     */
    fetch_room_messages_by_stanza_ids(room_jid, stanza_ids) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passArrayJsValueToWasm0(stanza_ids, wasm.__wbindgen_export);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_room_messages_by_stanza_ids(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Fetch the current pinned-messages list for a MUC room (#414).
     * Resolves to a JS array of `WaddlePinEntry`. Empty array if the
     * room has no pins. Server gates on room occupancy: a non-occupant
     * caller will get a `<forbidden type='auth'/>` error which surfaces
     * here as a rejected Promise.
     * @param {string} room_jid
     * @returns {Promise<any>}
     */
    fetch_room_pins(room_jid) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_room_pins(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * Fetch the global threads view (`urn:waddle:threads:0`).
     * Returns a `WaddleThreadsPage` (empty page on transport failure).
     * @param {any} opts
     * @returns {Promise<any>}
     */
    fetch_threads(opts) {
        const ret = wasm.waddleclient_fetch_threads(this.__wbg_ptr, addHeapObject(opts));
        return takeObject(ret);
    }
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
     * @returns {Promise<any>}
     */
    fetch_user_bookmarks() {
        const ret = wasm.waddleclient_fetch_user_bookmarks(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} jid
     * @returns {Promise<any>}
     */
    fetch_user_pep_profile(jid) {
        const ptr0 = passStringToWasm0(jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_user_pep_profile(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
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
     * @param {string} service_jid
     * @returns {Promise<any>}
     */
    fetch_vapid_public_key(service_jid) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_vapid_public_key(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @param {string} jid
     * @returns {Promise<any>}
     */
    fetch_vcard4(jid) {
        const ptr0 = passStringToWasm0(jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_fetch_vcard4(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @returns {any}
     */
    get_resume_state() {
        const ret = wasm.waddleclient_get_resume_state(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {WaddleResumeState | undefined}
     */
    get_resume_state_handle() {
        const ret = wasm.waddleclient_get_resume_state_handle(this.__wbg_ptr);
        return ret === 0 ? undefined : WaddleResumeState.__wrap(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    get_server_version() {
        const ret = wasm.waddleclient_get_server_version(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * `true` iff the authenticated user is the community owner — i.e.
     * the server accepts a probe of the admin Users command. Any
     * stanza error (including `<forbidden/>`) resolves to `false`;
     * the wasm boundary doesn't try to distinguish "not owner" from
     * "server error" because the admin panel's empty state is the
     * right fallback in either case.
     * @returns {Promise<any>}
     */
    is_community_owner() {
        const ret = wasm.waddleclient_is_community_owner(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Join a MUC room. Always requests zero discussion history
     * (`<history maxstanzas='0'/>`, XEP-0045 §7.2.15): MAM catch-up is
     * the authoritative history source, so accepting the service's
     * default join history would double-deliver recent messages (#1255).
     * The canonical presence shape lives in
     * [`waddle_xmpp_client::messaging::build_muc_join_presence`].
     * @param {string} room_jid
     * @param {string} nick
     * @returns {Promise<any>}
     */
    join_room(room_jid, nick) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(nick, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_join_room(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {string} nick
     * @returns {Promise<any>}
     */
    leave_room(room_jid, nick) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(nick, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_leave_room(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {string} affiliation
     * @returns {Promise<any>}
     */
    list_room_members(room_jid, affiliation) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(affiliation, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_list_room_members(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    list_roster_contacts() {
        const ret = wasm.waddleclient_list_roster_contacts(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} partner_jid
     * @param {string | null} [thread_id]
     * @returns {Promise<any>}
     */
    mark_inbox_read(partner_jid, thread_id) {
        const ptr0 = passStringToWasm0(partner_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(thread_id) ? 0 : passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_mark_inbox_read(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {WaddleConfig} config
     */
    constructor(config) {
        _assertClass(config, WaddleConfig);
        var ptr0 = config.__destroy_into_raw();
        const ret = wasm.waddleclient_new(ptr0);
        this.__wbg_ptr = ret;
        WaddleClientFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * Publish a 1:1 DM pin request.
     * @param {string} peer_jid
     * @param {string} target_stanza_id
     * @returns {Promise<any>}
     */
    pin_direct_message(peer_jid, target_stanza_id) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_stanza_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_pin_direct_message(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Publish a pin request (#414). `room_jid` is the bare MUC JID;
     * `target_stanza_id` is the XEP-0359 `by=room` stanza-id of the
     * message to pin. Server gates on Owner/Admin affiliation; a
     * non-admin sender will receive a `<forbidden type='auth'/>`
     * reply via the inbound message stream.
     * @param {string} room_jid
     * @param {string} target_stanza_id
     * @returns {Promise<any>}
     */
    pin_message(room_jid, target_stanza_id) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_stanza_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_pin_message(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {any} activity_json
     * @returns {Promise<any>}
     */
    publish_activity(activity_json) {
        const ret = wasm.waddleclient_publish_activity(this.__wbg_ptr, addHeapObject(activity_json));
        return takeObject(ret);
    }
    /**
     * XEP-0490 §3 publish to `urn:xmpp:mds:displayed:0`. `chat_id` is
     * the JID of the chat (bare DM contact, bare MUC room, or full MUC
     * occupant for a private message) which becomes the PEP item id;
     * `stanza_id` is the XEP-0359 id of the latest
     * displayed message; `stanza_id_by` is the resource-less room or
     * account-server authority required by XEP-0490. The publish carries the spec-mandated
     * publish-options as preconditions.
     * @param {string} chat_id
     * @param {string} stanza_id
     * @param {string} stanza_id_by
     * @returns {Promise<any>}
     */
    publish_mds_displayed(chat_id, stanza_id, stanza_id_by) {
        const ptr0 = passStringToWasm0(chat_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(stanza_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(stanza_id_by, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_publish_mds_displayed(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {any} mood_json
     * @returns {Promise<any>}
     */
    publish_mood(mood_json) {
        const ret = wasm.waddleclient_publish_mood(this.__wbg_ptr, addHeapObject(mood_json));
        return takeObject(ret);
    }
    /**
     * @param {any} tune_json
     * @returns {Promise<any>}
     */
    publish_tune(tune_json) {
        const ret = wasm.waddleclient_publish_tune(this.__wbg_ptr, addHeapObject(tune_json));
        return takeObject(ret);
    }
    /**
     * @param {any} vcard_json
     * @returns {Promise<any>}
     */
    publish_vcard4(vcard_json) {
        const ret = wasm.waddleclient_publish_vcard4(this.__wbg_ptr, addHeapObject(vcard_json));
        return takeObject(ret);
    }
    /**
     * XEP-0050 `register-device` ad-hoc command on `push.<domain>`.
     * Drives the multi-step dance and resolves to the assigned
     * XEP-0357 node id. Polymorphic over Web Push / APNs / FCM via
     * the `platform`-discriminated [`RegisterPushDeviceOptions`].
     *
     * Replaces the pre-cutover `ensure_push_node` +
     * `register_web_push_device` pair: the XEP-0050 result form
     * carries the assigned node id directly.
     * @param {any} options
     * @returns {Promise<any>}
     */
    register_push_device(options) {
        const ret = wasm.waddleclient_register_push_device(this.__wbg_ptr, addHeapObject(options));
        return takeObject(ret);
    }
    /**
     * @param {string} jid
     * @returns {Promise<any>}
     */
    request_avatar(jid) {
        const ptr0 = passStringToWasm0(jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_request_avatar(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * Best-effort XEP-0198 acknowledgement request for page lifecycle
     * handoff. The shared runtime suppresses duplicates while another
     * request is already awaiting `<a/>`.
     * @returns {Promise<any>}
     */
    request_stream_management_ack() {
        const ret = wasm.waddleclient_request_stream_management_ack(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} service_jid
     * @param {string} filename
     * @param {bigint} size
     * @param {string} content_type
     * @returns {Promise<any>}
     */
    request_upload_slot(service_jid, filename, size, content_type) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(filename, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(content_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_request_upload_slot(this.__wbg_ptr, ptr0, len0, ptr1, len1, size, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    retract_activity() {
        const ret = wasm.waddleclient_retract_activity(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    retract_mood() {
        const ret = wasm.waddleclient_retract_mood(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    retract_tune() {
        const ret = wasm.waddleclient_retract_tune(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_jid
     * @param {string} query
     * @param {number} max
     * @returns {Promise<any>}
     */
    search_dm_history(peer_jid, query, max) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_search_dm_history(this.__wbg_ptr, ptr0, len0, ptr1, len1, max);
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {string} query
     * @param {number} max
     * @returns {Promise<any>}
     */
    search_room_history(room_jid, query, max) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_search_room_history(this.__wbg_ptr, ptr0, len0, ptr1, len1, max);
        return takeObject(ret);
    }
    /**
     * @param {string} query
     * @returns {Promise<any>}
     */
    search_users(query) {
        const ptr0 = passStringToWasm0(query, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_search_users(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_finish(peer_full_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_finish(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} old_sid_str
     * @param {string} new_sid_str
     * @returns {Promise<any>}
     */
    send_call_finish_migrated(peer_full_jid, old_sid_str, new_sid_str) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(old_sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(new_sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_finish_migrated(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_proceed(peer_full_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_proceed(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Send a JMI `<propose/>` to the peer's bare JID (XEP-0353
     * §5.1.1). The bare JID lets the responder's server ring every
     * connected resource until one of them proceeds/rejects.
     * @param {string} peer_bare_jid
     * @param {string} sid_str
     * @param {boolean} audio
     * @param {boolean} video
     * @returns {Promise<any>}
     */
    send_call_propose(peer_bare_jid, sid_str, audio, video) {
        const ptr0 = passStringToWasm0(peer_bare_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_propose(this.__wbg_ptr, ptr0, len0, ptr1, len1, audio, video);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_reject(peer_full_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_reject(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_reject_tie_break(peer_full_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_reject_tie_break(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Send a JMI `<retract/>` to the peer's bare JID (XEP-0353 §3).
     * Like `<propose/>`, retract is addressed to the BARE JID so the
     * responder's server can fan the disavowal out to every resource
     * that may have seen the original ring. Typing the parameter as a
     * bare JID (and routing through `message_with_jmi_to_bare`, which
     * rejects a resource-bearing JID) enforces that on the wire — the
     * resource-targeted variant is `send_call_retract_tie_break`.
     * @param {string} peer_bare_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_retract(peer_bare_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_bare_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_retract(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_retract_tie_break(peer_full_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_retract_tie_break(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Send a JMI `<ringing/>` to the caller's bare JID (XEP-0353
     * §3.2). The bare JID lets the initiator's server fan out the
     * responder's device-ring state to every caller resource.
     * @param {string} peer_bare_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_call_ringing(peer_bare_jid, sid_str) {
        const ptr0 = passStringToWasm0(peer_bare_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_ringing(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Send a Jingle `session-accept` IQ in response to a received
     * session-initiate. `responder` is validated as a full JID at
     * the wasm boundary so a malformed JID surfaces
     * as a clear `JsError` rather than a wire-rejected stanza.
     * @param {string} peer_full_jid
     * @param {string} responder_full_jid
     * @param {string} sid_str
     * @param {boolean} audio
     * @param {boolean} video
     * @returns {Promise<any>}
     */
    send_call_session_accept(peer_full_jid, responder_full_jid, sid_str, audio, video) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(responder_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_session_accept(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, audio, video);
        return takeObject(ret);
    }
    /**
     * Send a Jingle `session-initiate` IQ to the peer's full JID
     * (XEP-0166 §6.4). The `initiator` attribute names the call
     * originator; the server's Jingle handler additionally
     * validates the authenticated session matches it.
     * @param {string} peer_full_jid
     * @param {string} initiator_full_jid
     * @param {string} sid_str
     * @param {boolean} audio
     * @param {boolean} video
     * @returns {Promise<any>}
     */
    send_call_session_initiate(peer_full_jid, initiator_full_jid, sid_str, audio, video) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(initiator_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_session_initiate(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, audio, video);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @param {string | null} [reason]
     * @returns {Promise<any>}
     */
    send_call_session_terminate(peer_full_jid, sid_str, reason) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(reason) ? 0 : passStringToWasm0(reason, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_session_terminate(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_full_jid
     * @param {string} sid_str
     * @param {string | null} [reason]
     * @returns {Promise<any>}
     */
    send_call_session_terminate_with_outcome(peer_full_jid, sid_str, reason) {
        const ptr0 = passStringToWasm0(peer_full_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(reason) ? 0 : passStringToWasm0(reason, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_call_session_terminate_with_outcome(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_jid
     * @param {string} body
     * @param {any} options
     * @returns {Promise<any>}
     */
    send_chat_message(peer_jid, body, options) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(body, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_chat_message(this.__wbg_ptr, ptr0, len0, ptr1, len1, addHeapObject(options));
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} state
     * @param {string | null} [thread_id]
     * @param {string | null} [thread_parent]
     * @returns {Promise<any>}
     */
    send_chat_state(to, msg_type, state, thread_id, thread_parent) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(state, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(thread_id) ? 0 : passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(thread_parent) ? 0 : passStringToWasm0(thread_parent, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_chat_state(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} body
     * @param {string} replaces_id
     * @param {any} options
     * @returns {Promise<any>}
     */
    send_correction(to, msg_type, body, replaces_id, options) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(body, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(replaces_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_correction(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, addHeapObject(options));
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} message_id
     * @param {string | null} [thread_id]
     * @param {string | null} [thread_parent]
     * @returns {Promise<any>}
     */
    send_displayed(to, msg_type, message_id, thread_id, thread_parent) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(message_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(thread_id) ? 0 : passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(thread_parent) ? 0 : passStringToWasm0(thread_parent, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_displayed(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        return takeObject(ret);
    }
    /**
     * @param {string} room_jid
     * @param {string} body
     * @param {any} options
     * @returns {Promise<any>}
     */
    send_groupchat_message(room_jid, body, options) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(body, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_groupchat_message(this.__wbg_ptr, ptr0, len0, ptr1, len1, addHeapObject(options));
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} sid
     * @param {string} emoji
     * @returns {Promise<any>}
     */
    send_in_call_reaction(to, msg_type, sid, emoji) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(sid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(emoji, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_in_call_reaction(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} target_id
     * @param {string | null} [reason]
     * @returns {Promise<any>}
     */
    send_moderation(to, msg_type, target_id, reason) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(reason) ? 0 : passStringToWasm0(reason, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_moderation(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        return takeObject(ret);
    }
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
     * @param {string} room_jid
     * @param {string} sid_str
     * @param {boolean} video
     * @returns {Promise<any>}
     */
    send_muji_session_initiate(room_jid, sid_str, video) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_muji_session_initiate(this.__wbg_ptr, ptr0, len0, ptr1, len1, video);
        return takeObject(ret);
    }
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
     * @param {string} room_jid
     * @param {string} sid_str
     * @returns {Promise<any>}
     */
    send_muji_session_terminate(room_jid, sid_str) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(sid_str, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_muji_session_terminate(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Publish the user's own presence (RFC 6121 §4.7). `show` is an RFC
     * 6121 `<show>` value (`away` / `xa` / `dnd` / `chat`) or `None` for
     * plain Available (the absence of a `<show>`); `status` is the optional
     * free-text line; `idle_since` is the XEP-0319 last-interaction instant as
     * an xs:dateTime string (auto-away only), or `None` on a return-from-idle.
     * Shares `build_presence_stanza` with the native + FFI `send_presence`, so
     * every Waddle client emits byte-identical presence including the XEP-0115
     * caps advertisement.
     * @param {string | null} [status]
     * @param {string | null} [show]
     * @param {string | null} [idle_since]
     * @returns {Promise<any>}
     */
    send_presence(status, show, idle_since) {
        var ptr0 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(show) ? 0 : passStringToWasm0(show, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        var ptr2 = isLikeNone(idle_since) ? 0 : passStringToWasm0(idle_since, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_presence(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
    /**
     * @param {string} xml
     * @returns {Promise<any>}
     */
    send_raw_iq(xml) {
        const ptr0 = passStringToWasm0(xml, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_raw_iq(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} target_id
     * @param {string[]} emojis
     * @param {string | null} [thread_id]
     * @param {string | null} [thread_parent]
     * @returns {Promise<any>}
     */
    send_reaction(to, msg_type, target_id, emojis, thread_id, thread_parent) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArrayJsValueToWasm0(emojis, wasm.__wbindgen_export);
        const len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(thread_id) ? 0 : passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        var ptr5 = isLikeNone(thread_parent) ? 0 : passStringToWasm0(thread_parent, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len5 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_reaction(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4, ptr5, len5);
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} retracts_id
     * @param {string | null} [thread_id]
     * @param {string | null} [thread_parent]
     * @returns {Promise<any>}
     */
    send_retraction(to, msg_type, retracts_id, thread_id, thread_parent) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(retracts_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        var ptr3 = isLikeNone(thread_id) ? 0 : passStringToWasm0(thread_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len3 = WASM_VECTOR_LEN;
        var ptr4 = isLikeNone(thread_parent) ? 0 : passStringToWasm0(thread_parent, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len4 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_retraction(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        return takeObject(ret);
    }
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
     * @param {any} options
     * @returns {Promise<any>}
     */
    set_dm_notification_mode(options) {
        const ret = wasm.waddleclient_set_dm_notification_mode(this.__wbg_ptr, addHeapObject(options));
        return takeObject(ret);
    }
    /**
     * Register a callback for inbound XMPP-native call events
     * (XEP-0353 JMI envelopes + XEP-0166 Jingle session control
     * carrying a `urn:waddle:transports:livekit:0` transport).
     * The callback receives a [`WaddleCallEvent`]-shaped object
     * with a `kind` discriminator and optional `media` / `join` /
     * `reason` fields. See `messaging::call::parse_call_event` on
     * the Rust side for the typed input.
     * @param {Function} cb
     */
    set_on_call(cb) {
        wasm.waddleclient_set_on_call(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_connected(cb) {
        wasm.waddleclient_set_on_connected(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_disconnected(cb) {
        wasm.waddleclient_set_on_disconnected(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_error(cb) {
        wasm.waddleclient_set_on_error(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * XEP-0490 §3.2 PEP event handler. Invoked once per displayed
     * item carried in an inbound `urn:xmpp:mds:displayed:0` PEP
     * event, with a `WaddleMdsDisplayedEntry`-shaped JS value.
     * @param {Function} cb
     */
    set_on_mds_displayed(cb) {
        wasm.waddleclient_set_on_mds_displayed(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_message(cb) {
        wasm.waddleclient_set_on_message(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_message_delivery_acked(cb) {
        wasm.waddleclient_set_on_message_delivery_acked(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_message_delivery_failed(cb) {
        wasm.waddleclient_set_on_message_delivery_failed(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_presence(cb) {
        wasm.waddleclient_set_on_presence(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * Generic XEP-0060 pubsub event handler. Invoked once per
     * inbound `<items/>` event with a `WaddlePubsubEvent`-shaped JS
     * value.
     * @param {Function} cb
     */
    set_on_pubsub_event(cb) {
        wasm.waddleclient_set_on_pubsub_event(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_session_lifecycle(cb) {
        wasm.waddleclient_set_on_session_lifecycle(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {Function} cb
     */
    set_on_stream_management(cb) {
        wasm.waddleclient_set_on_stream_management(this.__wbg_ptr, addHeapObject(cb));
    }
    /**
     * @param {string} room_jid
     * @param {string} jid
     * @param {string} affiliation
     * @returns {Promise<any>}
     */
    set_room_affiliation(room_jid, jid, affiliation) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(affiliation, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_set_room_affiliation(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
    }
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
     * @param {any} options
     * @returns {Promise<any>}
     */
    set_room_notification_mode(options) {
        const ret = wasm.waddleclient_set_room_notification_mode(this.__wbg_ptr, addHeapObject(options));
        return takeObject(ret);
    }
    /**
     * Fetch the user's stored preference from their own PEP node.
     * Resolves to `null` when nothing is stored.
     *
     * Per XEP-0223 §Security Considerations (CVE-2023-28686), the
     * result IQ's `from` MUST be absent or the account's bare JID;
     * anything else is treated as spoofed and yields `null`.
     * @returns {Promise<any>}
     */
    status_preference_fetch() {
        const ret = wasm.waddleclient_status_preference_fetch(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Publish the user's picked presence mode. Overwrites the single
     * `current` item on every call (reset publishes `mode='automatic'`
     * rather than retracting, so the change fans out to other devices).
     * @param {any} input
     * @returns {Promise<any>}
     */
    status_preference_publish(input) {
        const ret = wasm.waddleclient_status_preference_publish(this.__wbg_ptr, addHeapObject(input));
        return takeObject(ret);
    }
    /**
     * Fetch the latest stories from the community stories node on
     * `community_jid`. Returns ALL items including expired ones —
     * the chat filters active vs expired locally so a story fades
     * out as the countdown hits zero without a server roundtrip.
     * @param {string} community_jid
     * @param {number | null} [max_items]
     * @returns {Promise<any>}
     */
    stories_items(community_jid, max_items) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_stories_items(this.__wbg_ptr, ptr0, len0, isLikeNone(max_items) ? Number.MAX_SAFE_INTEGER : (max_items) >>> 0);
        return takeObject(ret);
    }
    /**
     * Publish a new story. `media_url` is required by XEP-0501; `body`
     * is optional text content attached to that media. `expiry_hours`
     * defaults to 24.
     * @param {string} community_jid
     * @param {any} input
     * @returns {Promise<any>}
     */
    stories_publish(community_jid, input) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_stories_publish(this.__wbg_ptr, ptr0, len0, addHeapObject(input));
        return takeObject(ret);
    }
    /**
     * Fetch the latest read-state from the user's own PEP node.
     *
     * Per XEP-0223 §Security Considerations (CVE-2023-28686), the
     * result IQ's `from` attribute MUST be either absent or equal to
     * the account's bare JID. Anything else is treated as spoofed
     * and produces an empty result.
     * @returns {Promise<any>}
     */
    story_reads_fetch() {
        const ret = wasm.waddleclient_story_reads_fetch(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Publish the user's read-state. Overwrites the single `current`
     * item on every call.
     * @param {any} input
     * @returns {Promise<any>}
     */
    story_reads_publish(input) {
        const ret = wasm.waddleclient_story_reads_publish(this.__wbg_ptr, addHeapObject(input));
        return takeObject(ret);
    }
    /**
     * XEP-0060 explicit subscribe to the MDS node, used as a
     * fallback path for receiving `+notify` events when the chat
     * client's presence does not yet carry XEP-0115 caps. The
     * subscriber JID is derived from the bound session JID (bare).
     * @returns {Promise<any>}
     */
    subscribe_mds_displayed() {
        const ret = wasm.waddleclient_subscribe_mds_displayed(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} peer_jid
     * @returns {Promise<any>}
     */
    subscribe_to_presence(peer_jid) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_subscribe_to_presence(this.__wbg_ptr, ptr0, len0);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    supports_mds_publish_options() {
        const ret = wasm.waddleclient_supports_mds_publish_options(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * Publish a 1:1 DM unpin request.
     * @param {string} peer_jid
     * @param {string} target_stanza_id
     * @returns {Promise<any>}
     */
    unpin_direct_message(peer_jid, target_stanza_id) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_stanza_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_unpin_direct_message(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Publish an unpin request (#414). Same authorization rules as
     * [`Self::pin_message`].
     * @param {string} room_jid
     * @param {string} target_stanza_id
     * @returns {Promise<any>}
     */
    unpin_message(room_jid, target_stanza_id) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(target_stanza_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_unpin_message(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
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
     * @param {string} room_jid
     * @param {string} nick
     * @param {boolean} active
     * @param {boolean} preparing
     * @param {boolean} video
     * @param {{ handRaised?: boolean; muted?: boolean }} in_call
     * @returns {Promise<any>}
     */
    update_muji_presence(room_jid, nick, active, preparing, video, in_call) {
        const ptr0 = passStringToWasm0(room_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(nick, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_update_muji_presence(this.__wbg_ptr, ptr0, len0, ptr1, len1, active, preparing, video, addHeapObject(in_call));
        return takeObject(ret);
    }
    /**
     * Fetch the latest calendar events from the community events
     * node. Returns ALL items including past events; chat-side
     * composables filter by DTSTART for upcoming-only views.
     * @param {string} community_jid
     * @param {number | null} [max_items]
     * @returns {Promise<any>}
     */
    xcal_items(community_jid, max_items) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_xcal_items(this.__wbg_ptr, ptr0, len0, isLikeNone(max_items) ? Number.MAX_SAFE_INTEGER : (max_items) >>> 0);
        return takeObject(ret);
    }
    /**
     * Publish a new calendar event, optionally with an RRULE for
     * recurrence. SUMMARY is required (per RFC 5545); DTSTART is
     * required for the event to be useful on a timeline.
     * @param {string} community_jid
     * @param {any} input
     * @returns {Promise<any>}
     */
    xcal_publish(community_jid, input) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_xcal_publish(this.__wbg_ptr, ptr0, len0, addHeapObject(input));
        return takeObject(ret);
    }
    /**
     * Publish (or replace) a full CalendarItem at the given item
     * id — master event plus optional per-instance overrides and
     * EXDATE cancellations. Use this for the read-modify-write
     * flows ("edit this occurrence", "edit all occurrences",
     * "cancel this occurrence") after fetching current state via
     * `xcal_items`. Passing an existing item id overwrites that
     * item atomically; passing a new id creates a new item.
     * @param {string} community_jid
     * @param {string} item_id
     * @param {any} input
     * @returns {Promise<any>}
     */
    xcal_publish_item(community_jid, item_id, input) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(item_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_xcal_publish_item(this.__wbg_ptr, ptr0, len0, ptr1, len1, addHeapObject(input));
        return takeObject(ret);
    }
    /**
     * Retract a calendar item from the events node. Used for
     * "cancel entire series" — removes the master plus any
     * overrides in one shot.
     * @param {string} community_jid
     * @param {string} item_id
     * @returns {Promise<any>}
     */
    xcal_retract(community_jid, item_id) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(item_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_xcal_retract(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * Publish (or update) this session's RSVP for a calendar event.
     * `partstat` must be one of "ACCEPTED" | "DECLINED" | "TENTATIVE"
     * | "NEEDS-ACTION". The chat groups sibling `-rsvp-*` items back
     * into the master event on the next items fetch.
     * @param {string} community_jid
     * @param {string} master_uid
     * @param {string} self_localpart
     * @param {string} self_jid
     * @param {string} partstat
     * @returns {Promise<any>}
     */
    xcal_rsvp(community_jid, master_uid, self_localpart, self_jid, partstat) {
        const ptr0 = passStringToWasm0(community_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(master_uid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(self_localpart, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(self_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ptr4 = passStringToWasm0(partstat, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len4 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_xcal_rsvp(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3, ptr4, len4);
        return takeObject(ret);
    }
}
if (Symbol.dispose) WaddleClient.prototype[Symbol.dispose] = WaddleClient.prototype.free;

export class WaddleConfig {
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WaddleConfigFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_waddleconfig_free(ptr, 0);
    }
    /**
     * @param {string} server_url
     * @param {string} jid
     * @param {string} access_token
     * @param {string} resource
     */
    constructor(server_url, jid, access_token, resource) {
        const ptr0 = passStringToWasm0(server_url, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(access_token, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passStringToWasm0(resource, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len3 = WASM_VECTOR_LEN;
        const ret = wasm.waddleconfig_new(ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        this.__wbg_ptr = ret;
        WaddleConfigFinalization.register(this, this.__wbg_ptr, this);
        return this;
    }
    /**
     * @param {string} previd
     * @param {number} inbound_h
     * @param {number} outbound_h
     */
    with_resume_state(previd, inbound_h, outbound_h) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(previd, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.waddleconfig_with_resume_state(retptr, this.__wbg_ptr, ptr0, len0, inbound_h, outbound_h);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {string} previd
     * @param {number} inbound_h
     * @param {number} outbound_h
     * @param {any} entries
     */
    with_resume_state_entries(previd, inbound_h, outbound_h, entries) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(previd, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.waddleconfig_with_resume_state_entries(retptr, this.__wbg_ptr, ptr0, len0, inbound_h, outbound_h, addHeapObject(entries));
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {string} previd
     * @param {number} inbound_h
     * @param {number} outbound_h
     * @param {any} entries
     * @param {number} max_resume_seconds
     */
    with_resume_state_entries_with_max(previd, inbound_h, outbound_h, entries, max_resume_seconds) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(previd, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.waddleconfig_with_resume_state_entries_with_max(retptr, this.__wbg_ptr, ptr0, len0, inbound_h, outbound_h, addHeapObject(entries), max_resume_seconds);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
    /**
     * @param {WaddleResumeState} state
     */
    with_resume_state_handle(state) {
        _assertClass(state, WaddleResumeState);
        wasm.waddleconfig_with_resume_state_handle(this.__wbg_ptr, state.__wbg_ptr);
    }
    /**
     * @param {string} previd
     * @param {number} inbound_h
     * @param {number} outbound_h
     * @param {number} max_resume_seconds
     */
    with_resume_state_with_max(previd, inbound_h, outbound_h, max_resume_seconds) {
        try {
            const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
            const ptr0 = passStringToWasm0(previd, wasm.__wbindgen_export, wasm.__wbindgen_export2);
            const len0 = WASM_VECTOR_LEN;
            wasm.waddleconfig_with_resume_state_with_max(retptr, this.__wbg_ptr, ptr0, len0, inbound_h, outbound_h, max_resume_seconds);
            var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
            var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
            if (r1) {
                throw takeObject(r0);
            }
        } finally {
            wasm.__wbindgen_add_to_stack_pointer(16);
        }
    }
}
if (Symbol.dispose) WaddleConfig.prototype[Symbol.dispose] = WaddleConfig.prototype.free;

export class WaddleResumeState {
    static __wrap(ptr) {
        const obj = Object.create(WaddleResumeState.prototype);
        obj.__wbg_ptr = ptr;
        WaddleResumeStateFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        WaddleResumeStateFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_waddleresumestate_free(ptr, 0);
    }
}
if (Symbol.dispose) WaddleResumeState.prototype[Symbol.dispose] = WaddleResumeState.prototype.free;

/**
 * Compute a CSS `hsl(...)` string for `input` using XEP-0392 with
 * custom saturation/lightness (percentages, 0.0–100.0). The CVD
 * correction is "none" — pass the hue back through `xep0392_consistent_hue`
 * and `apply_cvd_correction` in a future iteration if CVD modes
 * become a user preference.
 * @param {string} input
 * @param {number} saturation
 * @param {number} lightness
 * @returns {string}
 */
export function xep0392_consistent_color(input, saturation, lightness) {
    let deferred2_0;
    let deferred2_1;
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        const ptr0 = passStringToWasm0(input, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        wasm.xep0392_consistent_color(retptr, ptr0, len0, saturation, lightness);
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        deferred2_0 = r0;
        deferred2_1 = r1;
        return getStringFromWasm0(r0, r1);
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
        wasm.__wbindgen_export5(deferred2_0, deferred2_1, 1);
    }
}

/**
 * Compute the XEP-0392 consistent-color hue (0.0–360.0) for `input`.
 *
 * Stateless free function — does not require a WaddleClient instance.
 * JS callers receive a Number.
 * @param {string} input
 * @returns {number}
 */
export function xep0392_consistent_hue(input) {
    const ptr0 = passStringToWasm0(input, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    const len0 = WASM_VECTOR_LEN;
    const ret = wasm.xep0392_consistent_hue(ptr0, len0);
    return ret;
}
export function __wbg_Error_3639a60ed15f87e7(arg0, arg1) {
    const ret = Error(getStringFromWasm0(arg0, arg1));
    return addHeapObject(ret);
}
export function __wbg_Number_a3d737fd183f7dca(arg0) {
    const ret = Number(getObject(arg0));
    return ret;
}
export function __wbg_String_8564e559799eccda(arg0, arg1) {
    const ret = String(getObject(arg1));
    const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    const len1 = WASM_VECTOR_LEN;
    getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
    getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
}
export function __wbg___wbindgen_bigint_get_as_i64_3af6d4ca77193a4b(arg0, arg1) {
    const v = getObject(arg1);
    const ret = typeof(v) === 'bigint' ? v : undefined;
    getDataViewMemory0().setBigInt64(arg0 + 8 * 1, isLikeNone(ret) ? BigInt(0) : ret, true);
    getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
}
export function __wbg___wbindgen_boolean_get_c3dd5c39f1b5a12b(arg0) {
    const v = getObject(arg0);
    const ret = typeof(v) === 'boolean' ? v : undefined;
    return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
}
export function __wbg___wbindgen_debug_string_07cb72cfcc952e2b(arg0, arg1) {
    const ret = debugString(getObject(arg1));
    const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    const len1 = WASM_VECTOR_LEN;
    getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
    getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
}
export function __wbg___wbindgen_in_2617fa76397620d3(arg0, arg1) {
    const ret = getObject(arg0) in getObject(arg1);
    return ret;
}
export function __wbg___wbindgen_is_bigint_d6a8167cac401b95(arg0) {
    const ret = typeof(getObject(arg0)) === 'bigint';
    return ret;
}
export function __wbg___wbindgen_is_function_2f0fd7ceb86e64c5(arg0) {
    const ret = typeof(getObject(arg0)) === 'function';
    return ret;
}
export function __wbg___wbindgen_is_null_066086be3abe9bb3(arg0) {
    const ret = getObject(arg0) === null;
    return ret;
}
export function __wbg___wbindgen_is_object_5b22ff2418063a9c(arg0) {
    const val = getObject(arg0);
    const ret = typeof(val) === 'object' && val !== null;
    return ret;
}
export function __wbg___wbindgen_is_string_eddc07a3efad52e6(arg0) {
    const ret = typeof(getObject(arg0)) === 'string';
    return ret;
}
export function __wbg___wbindgen_is_undefined_244a92c34d3b6ec0(arg0) {
    const ret = getObject(arg0) === undefined;
    return ret;
}
export function __wbg___wbindgen_jsval_eq_403eaa3610500a25(arg0, arg1) {
    const ret = getObject(arg0) === getObject(arg1);
    return ret;
}
export function __wbg___wbindgen_jsval_loose_eq_1978f1e77b4bce62(arg0, arg1) {
    const ret = getObject(arg0) == getObject(arg1);
    return ret;
}
export function __wbg___wbindgen_number_get_dd6d69a6079f26f1(arg0, arg1) {
    const obj = getObject(arg1);
    const ret = typeof(obj) === 'number' ? obj : undefined;
    getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
    getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
}
export function __wbg___wbindgen_string_get_965592073e5d848c(arg0, arg1) {
    const obj = getObject(arg1);
    const ret = typeof(obj) === 'string' ? obj : undefined;
    var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    var len1 = WASM_VECTOR_LEN;
    getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
    getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
}
export function __wbg___wbindgen_throw_9c75d47bf9e7731e(arg0, arg1) {
    throw new Error(getStringFromWasm0(arg0, arg1));
}
export function __wbg__wbg_cb_unref_158e43e869788cdc(arg0) {
    getObject(arg0)._wbg_cb_unref();
}
export function __wbg_call_a41d6421b30a32c5() { return handleError(function (arg0, arg1, arg2) {
    const ret = getObject(arg0).call(getObject(arg1), getObject(arg2));
    return addHeapObject(ret);
}, arguments); }
export function __wbg_call_add9e5a76382e668() { return handleError(function (arg0, arg1) {
    const ret = getObject(arg0).call(getObject(arg1));
    return addHeapObject(ret);
}, arguments); }
export function __wbg_clearTimeout_491493c517cfff1c(arg0, arg1) {
    getObject(arg0).clearTimeout(arg1);
}
export function __wbg_close_931d0c62e2aab92c() { return handleError(function (arg0) {
    getObject(arg0).close();
}, arguments); }
export function __wbg_code_be6f339819ebb2c4(arg0) {
    const ret = getObject(arg0).code;
    return ret;
}
export function __wbg_data_4a14fad4c5f216c4(arg0) {
    const ret = getObject(arg0).data;
    return addHeapObject(ret);
}
export function __wbg_done_b1afd6201ac045e0(arg0) {
    const ret = getObject(arg0).done;
    return ret;
}
export function __wbg_entries_bb9843ba73dc70d6(arg0) {
    const ret = Object.entries(getObject(arg0));
    return addHeapObject(ret);
}
export function __wbg_getRandomValues_ef12552bf5acd2fe() { return handleError(function (arg0, arg1) {
    globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
}, arguments); }
export function __wbg_getTime_e599bee315e19eba(arg0) {
    const ret = getObject(arg0).getTime();
    return ret;
}
export function __wbg_get_652f640b3b0b6e3e(arg0, arg1) {
    const ret = getObject(arg0)[arg1 >>> 0];
    return addHeapObject(ret);
}
export function __wbg_get_9cfea9b7bbf12a15() { return handleError(function (arg0, arg1) {
    const ret = Reflect.get(getObject(arg0), getObject(arg1));
    return addHeapObject(ret);
}, arguments); }
export function __wbg_get_unchecked_be562b1421656321(arg0, arg1) {
    const ret = getObject(arg0)[arg1 >>> 0];
    return addHeapObject(ret);
}
export function __wbg_get_with_ref_key_6412cf3094599694(arg0, arg1) {
    const ret = getObject(arg0)[getObject(arg1)];
    return addHeapObject(ret);
}
export function __wbg_instanceof_ArrayBuffer_eab9f28fbec23477(arg0) {
    let result;
    try {
        result = getObject(arg0) instanceof ArrayBuffer;
    } catch (_) {
        result = false;
    }
    const ret = result;
    return ret;
}
export function __wbg_instanceof_Map_10d4edf60fcf9327(arg0) {
    let result;
    try {
        result = getObject(arg0) instanceof Map;
    } catch (_) {
        result = false;
    }
    const ret = result;
    return ret;
}
export function __wbg_instanceof_Uint8Array_57d77acd50e4c44d(arg0) {
    let result;
    try {
        result = getObject(arg0) instanceof Uint8Array;
    } catch (_) {
        result = false;
    }
    const ret = result;
    return ret;
}
export function __wbg_instanceof_Window_4153c1818a1c0c0b(arg0) {
    let result;
    try {
        result = getObject(arg0) instanceof Window;
    } catch (_) {
        result = false;
    }
    const ret = result;
    return ret;
}
export function __wbg_isArray_c6c6ef8308995bcf(arg0) {
    const ret = Array.isArray(getObject(arg0));
    return ret;
}
export function __wbg_isSafeInteger_3c56c421a5b4cce4(arg0) {
    const ret = Number.isSafeInteger(getObject(arg0));
    return ret;
}
export function __wbg_iterator_9d68985a1d096fc2() {
    const ret = Symbol.iterator;
    return addHeapObject(ret);
}
export function __wbg_length_0a6ce016dc1460b0(arg0) {
    const ret = getObject(arg0).length;
    return ret;
}
export function __wbg_length_ba3c032602efe310(arg0) {
    const ret = getObject(arg0).length;
    return ret;
}
export function __wbg_new_0_e486ec9936f7edbf() {
    const ret = new Date();
    return addHeapObject(ret);
}
export function __wbg_new_2fad8ca02fd00684() {
    const ret = new Object();
    return addHeapObject(ret);
}
export function __wbg_new_3baa8d9866155c79() {
    const ret = new Array();
    return addHeapObject(ret);
}
export function __wbg_new_8454eee672b2ba6e(arg0) {
    const ret = new Uint8Array(getObject(arg0));
    return addHeapObject(ret);
}
export function __wbg_new_c9ea13ea803a692e(arg0, arg1) {
    const ret = new Error(getStringFromWasm0(arg0, arg1));
    return addHeapObject(ret);
}
export function __wbg_new_typed_1137602701dc87d4(arg0, arg1) {
    try {
        var state0 = {a: arg0, b: arg1};
        var cb0 = (arg0, arg1) => {
            const a = state0.a;
            state0.a = 0;
            try {
                return __wasm_bindgen_func_elem_6509(a, state0.b, arg0, arg1);
            } finally {
                state0.a = a;
            }
        };
        const ret = new Promise(cb0);
        return addHeapObject(ret);
    } finally {
        state0.a = 0;
    }
}
export function __wbg_new_with_str_sequence_d90cb07368a00c61() { return handleError(function (arg0, arg1, arg2) {
    const ret = new WebSocket(getStringFromWasm0(arg0, arg1), getObject(arg2));
    return addHeapObject(ret);
}, arguments); }
export function __wbg_next_261c3c48c6e309a5(arg0) {
    const ret = getObject(arg0).next;
    return addHeapObject(ret);
}
export function __wbg_next_aacee310bcfe6461() { return handleError(function (arg0) {
    const ret = getObject(arg0).next();
    return addHeapObject(ret);
}, arguments); }
export function __wbg_now_4f457f10f864aec5() {
    const ret = Date.now();
    return ret;
}
export function __wbg_now_b205f8c23840112e(arg0) {
    const ret = getObject(arg0).now();
    return ret;
}
export function __wbg_performance_8e9fec534a95f99f(arg0) {
    const ret = getObject(arg0).performance;
    return isLikeNone(ret) ? 0 : addHeapObject(ret);
}
export function __wbg_prototypesetcall_fd4050e806e1d519(arg0, arg1, arg2) {
    Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), getObject(arg2));
}
export function __wbg_push_60a5366c0bb22a7d(arg0, arg1) {
    const ret = getObject(arg0).push(getObject(arg1));
    return ret;
}
export function __wbg_queueMicrotask_40ac6ffc2848ba77(arg0) {
    queueMicrotask(getObject(arg0));
}
export function __wbg_queueMicrotask_74d092439f6494c1(arg0) {
    const ret = getObject(arg0).queueMicrotask;
    return addHeapObject(ret);
}
export function __wbg_reason_fe958bcb63725f3b(arg0, arg1) {
    const ret = getObject(arg1).reason;
    const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_export, wasm.__wbindgen_export2);
    const len1 = WASM_VECTOR_LEN;
    getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
    getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
}
export function __wbg_resolve_9feb5d906ca62419(arg0) {
    const ret = Promise.resolve(getObject(arg0));
    return addHeapObject(ret);
}
export function __wbg_send_0edb796d05cd3239() { return handleError(function (arg0, arg1, arg2) {
    getObject(arg0).send(getStringFromWasm0(arg1, arg2));
}, arguments); }
export function __wbg_setTimeout_d007c6f72100a5e1() { return handleError(function (arg0, arg1, arg2) {
    const ret = getObject(arg0).setTimeout(getObject(arg1), arg2);
    return ret;
}, arguments); }
export function __wbg_set_5337f8ac82364a3f() { return handleError(function (arg0, arg1, arg2) {
    const ret = Reflect.set(getObject(arg0), getObject(arg1), getObject(arg2));
    return ret;
}, arguments); }
export function __wbg_set_6be42768c690e380(arg0, arg1, arg2) {
    getObject(arg0)[takeObject(arg1)] = takeObject(arg2);
}
export function __wbg_set_binaryType_8564bdba0fbec720(arg0, arg1) {
    getObject(arg0).binaryType = __wbindgen_enum_BinaryType[arg1];
}
export function __wbg_set_f614f6a0608d1d1d(arg0, arg1, arg2) {
    getObject(arg0)[arg1 >>> 0] = takeObject(arg2);
}
export function __wbg_set_onclose_f756840519cd20b5(arg0, arg1) {
    getObject(arg0).onclose = getObject(arg1);
}
export function __wbg_set_onerror_02f33de339f1fa31(arg0, arg1) {
    getObject(arg0).onerror = getObject(arg1);
}
export function __wbg_set_onmessage_d2ff0c1d20584625(arg0, arg1) {
    getObject(arg0).onmessage = getObject(arg1);
}
export function __wbg_set_onopen_1da8a4f65e6180d2(arg0, arg1) {
    getObject(arg0).onopen = getObject(arg1);
}
export function __wbg_static_accessor_GLOBAL_THIS_1c7f1bd6c6941fdb() {
    const ret = typeof globalThis === 'undefined' ? null : globalThis;
    return isLikeNone(ret) ? 0 : addHeapObject(ret);
}
export function __wbg_static_accessor_GLOBAL_e039bc914f83e74e() {
    const ret = typeof global === 'undefined' ? null : global;
    return isLikeNone(ret) ? 0 : addHeapObject(ret);
}
export function __wbg_static_accessor_SELF_8bf8c48c28420ad5() {
    const ret = typeof self === 'undefined' ? null : self;
    return isLikeNone(ret) ? 0 : addHeapObject(ret);
}
export function __wbg_static_accessor_WINDOW_6aeee9b51652ee0f() {
    const ret = typeof window === 'undefined' ? null : window;
    return isLikeNone(ret) ? 0 : addHeapObject(ret);
}
export function __wbg_then_20a157d939b514f5(arg0, arg1) {
    const ret = getObject(arg0).then(getObject(arg1));
    return addHeapObject(ret);
}
export function __wbg_value_f852716acdeb3e82(arg0) {
    const ret = getObject(arg0).value;
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000001(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 868, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_6492);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000002(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("CloseEvent")], shim_idx: 676, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_4244);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000003(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("ErrorEvent")], shim_idx: 676, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_4244_2);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000004(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 676, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_4244_3);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000005(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 677, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_4243);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000006(arg0) {
    // Cast intrinsic for `F64 -> Externref`.
    const ret = arg0;
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000007(arg0) {
    // Cast intrinsic for `I64 -> Externref`.
    const ret = arg0;
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000008(arg0, arg1) {
    // Cast intrinsic for `Ref(String) -> Externref`.
    const ret = getStringFromWasm0(arg0, arg1);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000009(arg0) {
    // Cast intrinsic for `U64 -> Externref`.
    const ret = BigInt.asUintN(64, arg0);
    return addHeapObject(ret);
}
export function __wbindgen_object_clone_ref(arg0) {
    const ret = getObject(arg0);
    return addHeapObject(ret);
}
export function __wbindgen_object_drop_ref(arg0) {
    takeObject(arg0);
}
function __wasm_bindgen_func_elem_4243(arg0, arg1) {
    wasm.__wasm_bindgen_func_elem_4243(arg0, arg1);
}

function __wasm_bindgen_func_elem_4244(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_4244(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_4244_2(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_4244_2(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_4244_3(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_4244_3(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_6492(arg0, arg1, arg2) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.__wasm_bindgen_func_elem_6492(retptr, arg0, arg1, addHeapObject(arg2));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

function __wasm_bindgen_func_elem_6509(arg0, arg1, arg2, arg3) {
    wasm.__wasm_bindgen_func_elem_6509(arg0, arg1, addHeapObject(arg2), addHeapObject(arg3));
}


const __wbindgen_enum_BinaryType = ["blob", "arraybuffer"];
const WaddleClientFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_waddleclient_free(ptr, 1));
const WaddleConfigFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_waddleconfig_free(ptr, 1));
const WaddleResumeStateFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_waddleresumestate_free(ptr, 1));

function addHeapObject(obj) {
    if (heap_next === heap.length) heap.push(heap.length + 1);
    const idx = heap_next;
    heap_next = heap[idx];

    heap[idx] = obj;
    return idx;
}

function _assertClass(instance, klass) {
    if (!(instance instanceof klass)) {
        throw new Error(`expected instance of ${klass.name}`);
    }
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_export4(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function dropObject(idx) {
    if (idx < 1028) return;
    heap[idx] = heap_next;
    heap_next = idx;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function getObject(idx) { return heap[idx]; }

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        wasm.__wbindgen_export3(addHeapObject(e));
    }
}

let heap = new Array(1024).fill(undefined);
heap.push(undefined, null, true, false);

let heap_next = heap.length;

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_export4(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passArrayJsValueToWasm0(array, malloc) {
    const ptr = malloc(array.length * 4, 4) >>> 0;
    const mem = getDataViewMemory0();
    for (let i = 0; i < array.length; i++) {
        mem.setUint32(ptr + 4 * i, addHeapObject(array[i]), true);
    }
    WASM_VECTOR_LEN = array.length;
    return ptr;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeObject(idx) {
    const ret = getObject(idx);
    dropObject(idx);
    return ret;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;


let wasm;
export function __wbg_set_wasm(val) {
    wasm = val;
}
