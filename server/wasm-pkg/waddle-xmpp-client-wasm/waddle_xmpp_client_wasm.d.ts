/* tslint:disable */
/* eslint-disable */

export class WaddleClient {
    free(): void;
    [Symbol.dispose](): void;
    connect(): Promise<any>;
    disable_push_notifications(service_jid: string, node: string): Promise<any>;
    disconnect(): Promise<any>;
    discover_upload_service(): Promise<any>;
    enable_push_notifications(service_jid: string, node: string, token: string): Promise<any>;
    fetch_dm_history(peer_jid: string, max: number, before_id?: string | null): Promise<any>;
    fetch_dm_history_page(peer_jid: string, max: number, page_param: any): Promise<any>;
    fetch_inbox(opts: any): Promise<any>;
    fetch_room_history(room_jid: string, max: number, before_id?: string | null): Promise<any>;
    fetch_room_history_by_thread(room_jid: string, thread_id: string, max: number, before_id?: string | null): Promise<any>;
    fetch_room_history_page(room_jid: string, max: number, page_param: any): Promise<any>;
    fetch_user_pep_profile(jid: string): Promise<any>;
    get_server_version(): Promise<any>;
    join_room(room_jid: string, nick: string): Promise<any>;
    leave_room(room_jid: string, nick: string): Promise<any>;
    list_room_members(room_jid: string, affiliation: string): Promise<any>;
    list_rooms(): Promise<any>;
    list_roster_contacts(): Promise<any>;
    mark_inbox_read(partner_jid: string, thread_id?: string | null): Promise<any>;
    constructor(config: WaddleConfig);
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
    set_on_session_lifecycle(cb: (event: string) => void): void;
    set_room_affiliation(room_jid: string, jid: string, affiliation: string): Promise<any>;
    subscribe_to_presence(peer_jid: string): Promise<any>;
}

export class WaddleConfig {
    free(): void;
    [Symbol.dispose](): void;
    constructor(server_url: string, jid: string, access_token: string, resource: string);
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_waddleclient_free: (a: number, b: number) => void;
    readonly __wbg_waddleconfig_free: (a: number, b: number) => void;
    readonly waddleclient_connect: (a: number) => number;
    readonly waddleclient_disable_push_notifications: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_disconnect: (a: number) => number;
    readonly waddleclient_discover_upload_service: (a: number) => number;
    readonly waddleclient_enable_push_notifications: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly waddleclient_fetch_dm_history: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly waddleclient_fetch_dm_history_page: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_fetch_inbox: (a: number, b: number) => number;
    readonly waddleclient_fetch_room_history: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly waddleclient_fetch_room_history_by_thread: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => number;
    readonly waddleclient_fetch_room_history_page: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_fetch_user_pep_profile: (a: number, b: number, c: number) => number;
    readonly waddleclient_get_server_version: (a: number) => number;
    readonly waddleclient_join_room: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_leave_room: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_list_room_members: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_list_rooms: (a: number) => number;
    readonly waddleclient_list_roster_contacts: (a: number) => number;
    readonly waddleclient_mark_inbox_read: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_new: (a: number) => number;
    readonly waddleclient_publish_activity: (a: number, b: number) => number;
    readonly waddleclient_publish_mood: (a: number, b: number) => number;
    readonly waddleclient_publish_tune: (a: number, b: number) => number;
    readonly waddleclient_request_avatar: (a: number, b: number, c: number) => number;
    readonly waddleclient_request_upload_slot: (a: number, b: number, c: number, d: number, e: number, f: bigint, g: number, h: number) => number;
    readonly waddleclient_retract_activity: (a: number) => number;
    readonly waddleclient_retract_mood: (a: number) => number;
    readonly waddleclient_retract_tune: (a: number) => number;
    readonly waddleclient_search_dm_history: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly waddleclient_search_room_history: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly waddleclient_search_users: (a: number, b: number, c: number) => number;
    readonly waddleclient_send_chat_message: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly waddleclient_send_chat_state: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly waddleclient_send_correction: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number, j: number) => number;
    readonly waddleclient_send_displayed: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly waddleclient_send_groupchat_message: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly waddleclient_send_moderation: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => number;
    readonly waddleclient_send_presence: (a: number, b: number, c: number, d: number, e: number) => number;
    readonly waddleclient_send_raw_iq: (a: number, b: number, c: number) => number;
    readonly waddleclient_send_reaction: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number, i: number) => number;
    readonly waddleclient_send_retraction: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly waddleclient_set_on_connected: (a: number, b: number) => void;
    readonly waddleclient_set_on_disconnected: (a: number, b: number) => void;
    readonly waddleclient_set_on_error: (a: number, b: number) => void;
    readonly waddleclient_set_on_message: (a: number, b: number) => void;
    readonly waddleclient_set_on_message_delivery_acked: (a: number, b: number) => void;
    readonly waddleclient_set_on_message_delivery_failed: (a: number, b: number) => void;
    readonly waddleclient_set_on_presence: (a: number, b: number) => void;
    readonly waddleclient_set_on_session_lifecycle: (a: number, b: number) => void;
    readonly waddleclient_set_room_affiliation: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => number;
    readonly waddleclient_subscribe_to_presence: (a: number, b: number, c: number) => number;
    readonly waddleconfig_new: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => number;
    readonly __wasm_bindgen_func_elem_2647: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_2653: (a: number, b: number, c: number, d: number) => void;
    readonly __wasm_bindgen_func_elem_1511: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_1511_2: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_1511_3: (a: number, b: number, c: number) => void;
    readonly __wasm_bindgen_func_elem_1510: (a: number, b: number) => void;
    readonly __wbindgen_export: (a: number, b: number) => number;
    readonly __wbindgen_export2: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_export3: (a: number) => void;
    readonly __wbindgen_export4: (a: number, b: number) => void;
    readonly __wbindgen_add_to_stack_pointer: (a: number) => number;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
