/* tslint:disable */
/* eslint-disable */

export class WaddleClient {
    free(): void;
    [Symbol.dispose](): void;
    connect(): Promise<any>;
    disable_push_notifications(service_jid: string, node: string): Promise<any>;
    disconnect(): Promise<any>;
    discover_extension_routes(user_jid?: string | null): Promise<any>;
    discover_upload_service(): Promise<any>;
    enable_push_notifications(service_jid: string, node: string, token: string): Promise<any>;
    fetch_dm_history(peer_jid: string, max: number, before_id?: string | null): Promise<any>;
    fetch_dm_history_page(peer_jid: string, max: number, page_param: any): Promise<any>;
    fetch_extension_route_items(route: any, room_jid: string): Promise<any>;
    fetch_inbox(opts: any): Promise<any>;
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
    fetch_user_pep_profile(jid: string): Promise<any>;
    get_resume_state(): any;
    get_resume_state_handle(): WaddleResumeState | undefined;
    get_server_version(): Promise<any>;
    join_room(room_jid: string, nick: string): Promise<any>;
    join_room_without_history(room_jid: string, nick: string): Promise<any>;
    leave_room(room_jid: string, nick: string): Promise<any>;
    list_room_members(room_jid: string, affiliation: string): Promise<any>;
    list_roster_contacts(): Promise<any>;
    mark_inbox_read(partner_jid: string, thread_id?: string | null): Promise<any>;
    constructor(config: WaddleConfig);
    /**
     * Publish a pin request (#414). `room_jid` is the bare MUC JID;
     * `target_stanza_id` is the XEP-0359 `by=room` stanza-id of the
     * message to pin. Server gates on Owner/Admin affiliation; a
     * non-admin sender will receive a `<forbidden type='auth'/>`
     * reply via the inbound message stream.
     */
    pin_message(room_jid: string, target_stanza_id: string): Promise<any>;
    publish_activity(activity_json: any): Promise<any>;
    publish_mood(mood_json: any): Promise<any>;
    publish_tune(tune_json: any): Promise<any>;
    request_avatar(jid: string): Promise<any>;
    request_upload_slot(service_jid: string, filename: string, size: bigint, content_type: string): Promise<any>;
    retract_activity(): Promise<any>;
    retract_mood(): Promise<any>;
    retract_tune(): Promise<any>;
    search_dm_history(peer_jid: string, query: string, max: number): Promise<any>;
    search_room_history(room_jid: string, query: string, max: number): Promise<any>;
    search_users(query: string): Promise<any>;
    send_chat_message(peer_jid: string, body: string, options: any): Promise<any>;
    send_chat_state(to: string, msg_type: string, state: string): Promise<any>;
    send_correction(to: string, msg_type: string, body: string, replaces_id: string, options: any): Promise<any>;
    send_displayed(to: string, msg_type: string, message_id: string): Promise<any>;
    send_groupchat_message(room_jid: string, body: string, options: any): Promise<any>;
    send_moderation(to: string, msg_type: string, target_id: string, reason?: string | null): Promise<any>;
    send_presence(status?: string | null, show?: string | null): Promise<any>;
    send_raw_iq(xml: string): Promise<any>;
    send_reaction(to: string, msg_type: string, target_id: string, emojis: string[]): Promise<any>;
    send_retraction(to: string, msg_type: string, retracts_id: string): Promise<any>;
    set_on_connected(cb: Function): void;
    set_on_disconnected(cb: Function): void;
    set_on_error(cb: Function): void;
    set_on_message(cb: Function): void;
    set_on_message_delivery_acked(cb: Function): void;
    set_on_message_delivery_failed(cb: Function): void;
    set_on_presence(cb: Function): void;
    set_on_session_lifecycle(cb: Function): void;
    set_room_affiliation(room_jid: string, jid: string, affiliation: string): Promise<any>;
    subscribe_to_presence(peer_jid: string): Promise<any>;
    /**
     * Publish an unpin request (#414). Same authorization rules as
     * [`Self::pin_message`].
     */
    unpin_message(room_jid: string, target_stanza_id: string): Promise<any>;
}

export class WaddleConfig {
    free(): void;
    [Symbol.dispose](): void;
    constructor(server_url: string, jid: string, access_token: string, resource: string);
    with_resume_state(previd: string, inbound_h: number, outbound_h: number): void;
    with_resume_state_handle(state: WaddleResumeState): void;
}

export class WaddleResumeState {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
}

export default function init(): Promise<void>;
