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
     * @returns {Promise<any>}
     */
    connect() {
        const ret = wasm.waddleclient_connect(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} service_jid
     * @param {string} node
     * @returns {Promise<any>}
     */
    disable_push_notifications(service_jid, node) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
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
     * @returns {Promise<any>}
     */
    discover_upload_service() {
        const ret = wasm.waddleclient_discover_upload_service(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
     * @param {string} service_jid
     * @param {string} node
     * @param {string} token
     * @returns {Promise<any>}
     */
    enable_push_notifications(service_jid, node, token) {
        const ptr0 = passStringToWasm0(service_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(token, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_enable_push_notifications(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
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
     * @param {any} opts
     * @returns {Promise<any>}
     */
    fetch_inbox(opts) {
        const ret = wasm.waddleclient_fetch_inbox(this.__wbg_ptr, addHeapObject(opts));
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
     * Query pubsub items from a node (XEP-0060).
     *
     * Returns an array of `{ jid, name }` objects extracted from the `<item>`
     * elements in the result IQ.  The item `id` attribute is used as the JID;
     * the optional `name` attribute on a `<conference>` child (XEP-0402) is
     * used as the human-readable name.
     * @param {string} to
     * @param {string} node
     * @returns {Promise<any>}
     */
    get_pubsub_items(to, node) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(node, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_get_pubsub_items(this.__wbg_ptr, ptr0, len0, ptr1, len1);
        return takeObject(ret);
    }
    /**
     * @returns {Promise<any>}
     */
    get_server_version() {
        const ret = wasm.waddleclient_get_server_version(this.__wbg_ptr);
        return takeObject(ret);
    }
    /**
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
    list_rooms() {
        const ret = wasm.waddleclient_list_rooms(this.__wbg_ptr);
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
     * @param {any} activity_json
     * @returns {Promise<any>}
     */
    publish_activity(activity_json) {
        const ret = wasm.waddleclient_publish_activity(this.__wbg_ptr, addHeapObject(activity_json));
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
     * @returns {Promise<any>}
     */
    send_chat_state(to, msg_type, state) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(state, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_chat_state(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
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
     * @returns {Promise<any>}
     */
    send_displayed(to, msg_type, message_id) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(message_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_displayed(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
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
     * @param {string | null} [status]
     * @param {string | null} [show]
     * @returns {Promise<any>}
     */
    send_presence(status, show) {
        var ptr0 = isLikeNone(status) ? 0 : passStringToWasm0(status, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len0 = WASM_VECTOR_LEN;
        var ptr1 = isLikeNone(show) ? 0 : passStringToWasm0(show, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        var len1 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_presence(this.__wbg_ptr, ptr0, len0, ptr1, len1);
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
     * @returns {Promise<any>}
     */
    send_reaction(to, msg_type, target_id, emojis) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(target_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ptr3 = passArrayJsValueToWasm0(emojis, wasm.__wbindgen_export);
        const len3 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_reaction(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2, ptr3, len3);
        return takeObject(ret);
    }
    /**
     * @param {string} to
     * @param {string} msg_type
     * @param {string} retracts_id
     * @returns {Promise<any>}
     */
    send_retraction(to, msg_type, retracts_id) {
        const ptr0 = passStringToWasm0(to, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ptr1 = passStringToWasm0(msg_type, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len1 = WASM_VECTOR_LEN;
        const ptr2 = passStringToWasm0(retracts_id, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len2 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_send_retraction(this.__wbg_ptr, ptr0, len0, ptr1, len1, ptr2, len2);
        return takeObject(ret);
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
     * @param {Function} cb
     */
    set_on_session_lifecycle(cb) {
        wasm.waddleclient_set_on_session_lifecycle(this.__wbg_ptr, addHeapObject(cb));
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
     * @param {string} peer_jid
     * @returns {Promise<any>}
     */
    subscribe_to_presence(peer_jid) {
        const ptr0 = passStringToWasm0(peer_jid, wasm.__wbindgen_export, wasm.__wbindgen_export2);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.waddleclient_subscribe_to_presence(this.__wbg_ptr, ptr0, len0);
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
}
if (Symbol.dispose) WaddleConfig.prototype[Symbol.dispose] = WaddleConfig.prototype.free;
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
export function __wbg_arrayBuffer_87e3ac06d961f7a0() { return handleError(function (arg0) {
    const ret = getObject(arg0).arrayBuffer();
    return addHeapObject(ret);
}, arguments); }
export function __wbg_call_a41d6421b30a32c5() { return handleError(function (arg0, arg1, arg2) {
    const ret = getObject(arg0).call(getObject(arg1), getObject(arg2));
    return addHeapObject(ret);
}, arguments); }
export function __wbg_call_add9e5a76382e668() { return handleError(function (arg0, arg1) {
    const ret = getObject(arg0).call(getObject(arg1));
    return addHeapObject(ret);
}, arguments); }
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
export function __wbg_fetch_dc020402ef5b5b70(arg0, arg1, arg2) {
    const ret = getObject(arg0).fetch(getStringFromWasm0(arg1, arg2));
    return addHeapObject(ret);
}
export function __wbg_getRandomValues_ef12552bf5acd2fe() { return handleError(function (arg0, arg1) {
    globalThis.crypto.getRandomValues(getArrayU8FromWasm0(arg0, arg1));
}, arguments); }
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
export function __wbg_instanceof_Response_370b83aa6c17e88a(arg0) {
    let result;
    try {
        result = getObject(arg0) instanceof Response;
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
export function __wbg_new_typed_1137602701dc87d4(arg0, arg1) {
    try {
        var state0 = {a: arg0, b: arg1};
        var cb0 = (arg0, arg1) => {
            const a = state0.a;
            state0.a = 0;
            try {
                return __wasm_bindgen_func_elem_2684(a, state0.b, arg0, arg1);
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
export function __wbg_ok_b6a9978bb5f66f33(arg0) {
    const ret = getObject(arg0).ok;
    return ret;
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
export function __wbg_status_157e67ab07d01f8a(arg0) {
    const ret = getObject(arg0).status;
    return ret;
}
export function __wbg_then_20a157d939b514f5(arg0, arg1) {
    const ret = getObject(arg0).then(getObject(arg1));
    return addHeapObject(ret);
}
export function __wbg_then_5ef9b762bc91555c(arg0, arg1, arg2) {
    const ret = getObject(arg0).then(getObject(arg1), getObject(arg2));
    return addHeapObject(ret);
}
export function __wbg_value_f852716acdeb3e82(arg0) {
    const ret = getObject(arg0).value;
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000001(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 319, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_2667);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000002(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("CloseEvent")], shim_idx: 253, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_1530);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000003(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("ErrorEvent")], shim_idx: 253, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_1530_2);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000004(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 253, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_1530_3);
    return addHeapObject(ret);
}
export function __wbindgen_cast_0000000000000005(arg0, arg1) {
    // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 254, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
    const ret = makeMutClosure(arg0, arg1, __wasm_bindgen_func_elem_1529);
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
function __wasm_bindgen_func_elem_1529(arg0, arg1) {
    wasm.__wasm_bindgen_func_elem_1529(arg0, arg1);
}

function __wasm_bindgen_func_elem_1530(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_1530(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_1530_2(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_1530_2(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_1530_3(arg0, arg1, arg2) {
    wasm.__wasm_bindgen_func_elem_1530_3(arg0, arg1, addHeapObject(arg2));
}

function __wasm_bindgen_func_elem_2667(arg0, arg1, arg2) {
    try {
        const retptr = wasm.__wbindgen_add_to_stack_pointer(-16);
        wasm.__wasm_bindgen_func_elem_2667(retptr, arg0, arg1, addHeapObject(arg2));
        var r0 = getDataViewMemory0().getInt32(retptr + 4 * 0, true);
        var r1 = getDataViewMemory0().getInt32(retptr + 4 * 1, true);
        if (r1) {
            throw takeObject(r0);
        }
    } finally {
        wasm.__wbindgen_add_to_stack_pointer(16);
    }
}

function __wasm_bindgen_func_elem_2684(arg0, arg1, arg2, arg3) {
    wasm.__wasm_bindgen_func_elem_2684(arg0, arg1, addHeapObject(arg2), addHeapObject(arg3));
}


const __wbindgen_enum_BinaryType = ["blob", "arraybuffer"];
const WaddleClientFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_waddleclient_free(ptr, 1));
const WaddleConfigFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_waddleconfig_free(ptr, 1));

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
