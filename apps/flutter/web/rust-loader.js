/* Browser boundary for the unchanged wasm-bindgen protocol client.
 * Build its module with wasm-pack --target web into web/rust; no bundler/Vite imports. */
(() => {
  "use strict";
  const signatures = {
  "admin_channels_affiliations": [
    "args"
  ],
  "admin_channels_create": [
    "args"
  ],
  "admin_channels_delete": [
    "args"
  ],
  "admin_channels_kick": [
    "args"
  ],
  "admin_channels_list": [
    "args"
  ],
  "admin_channels_occupants": [
    "args"
  ],
  "admin_channels_set_affiliation": [
    "args"
  ],
  "admin_channels_update": [
    "args"
  ],
  "admin_spaces_create": [
    "args"
  ],
  "admin_spaces_delete": [
    "args"
  ],
  "admin_spaces_list": [
    "args"
  ],
  "admin_spaces_members": [
    "args"
  ],
  "admin_spaces_set_role": [
    "args"
  ],
  "admin_spaces_update": [
    "args"
  ],
  "admin_users_list": [
    "prefix",
    "page_size",
    "after_cursor"
  ],
  "cancel_raw_iq": [
    "id"
  ],
  "connect": [],
  "disable_push_device": [
    "service_jid",
    "node",
    "device_id"
  ],
  "disable_push_notifications": [
    "service_jid",
    "node"
  ],
  "disconnect": [],
  "discover_extension_routes": [
    "user_jid"
  ],
  "discover_upload_service": [],
  "enable_push_notifications": [
    "service_jid",
    "node"
  ],
  "feed_items": [
    "spaces_jid",
    "max_items"
  ],
  "feed_publish": [
    "spaces_jid",
    "entry"
  ],
  "fetch_direct_messages_by_stanza_ids": [
    "peer_jid",
    "stanza_ids"
  ],
  "fetch_dm_bookmarks": [],
  "fetch_dm_history": [
    "peer_jid",
    "max",
    "before_id"
  ],
  "fetch_dm_history_by_thread": [
    "peer_jid",
    "thread_id",
    "max",
    "before_id"
  ],
  "fetch_dm_history_page": [
    "peer_jid",
    "max",
    "page_param"
  ],
  "fetch_extension_route_items": [
    "route",
    "room_jid"
  ],
  "fetch_external_services": [],
  "fetch_inbox": [
    "opts"
  ],
  "fetch_mds_displayed": [],
  "fetch_personal_history_page": [
    "max",
    "page_param"
  ],
  "fetch_room_history": [
    "room_jid",
    "max",
    "before_id"
  ],
  "fetch_room_history_by_thread": [
    "room_jid",
    "thread_id",
    "max",
    "before_id"
  ],
  "fetch_room_history_page": [
    "room_jid",
    "max",
    "page_param"
  ],
  "fetch_room_messages_by_stanza_ids": [
    "room_jid",
    "stanza_ids"
  ],
  "fetch_room_pins": [
    "room_jid"
  ],
  "fetch_threads": [
    "opts"
  ],
  "fetch_user_bookmarks": [],
  "fetch_user_pep_profile": [
    "jid"
  ],
  "fetch_vapid_public_key": [
    "service_jid"
  ],
  "fetch_vcard4": [
    "jid"
  ],
  "resume_state": [],
  "get_server_version": [],
  "is_community_owner": [],
  "join_room": [
    "room_jid",
    "nick"
  ],
  "leave_room": [
    "room_jid",
    "nick"
  ],
  "list_room_bots": [
    "room_jid"
  ],
  "list_room_members": [
    "room_jid",
    "affiliation"
  ],
  "list_roster_contacts": [],
  "mark_inbox_read": [
    "partner_jid",
    "thread_id"
  ],
  "pin_direct_message": [
    "peer_jid",
    "target_stanza_id"
  ],
  "pin_message": [
    "room_jid",
    "target_stanza_id"
  ],
  "publish_activity": [
    "activity_json"
  ],
  "publish_mds_displayed": [
    "chat_id",
    "stanza_id",
    "stanza_id_by"
  ],
  "publish_mood": [
    "mood_json"
  ],
  "publish_tune": [
    "tune_json"
  ],
  "publish_vcard4": [
    "vcard_json"
  ],
  "register_push_device": [
    "options"
  ],
  "request_avatar": [
    "jid",
    "known_ids"
  ],
  "request_stream_management_ack": [],
  "request_upload_slot": [
    "service_jid",
    "filename",
    "size",
    "content_type"
  ],
  "retract_activity": [],
  "retract_mood": [],
  "retract_tune": [],
  "search_dm_history": [
    "peer_jid",
    "query",
    "max"
  ],
  "search_room_history": [
    "room_jid",
    "query",
    "max"
  ],
  "search_users": [
    "query"
  ],
  "send_call_finish": [
    "peer_full_jid",
    "sid_str"
  ],
  "send_call_finish_migrated": [
    "peer_full_jid",
    "old_sid_str",
    "new_sid_str"
  ],
  "send_call_proceed": [
    "peer_full_jid",
    "sid_str"
  ],
  "send_call_propose": [
    "peer_bare_jid",
    "sid_str",
    "audio",
    "video"
  ],
  "send_call_reject": [
    "peer_full_jid",
    "sid_str"
  ],
  "send_call_reject_tie_break": [
    "peer_full_jid",
    "sid_str"
  ],
  "send_call_retract": [
    "peer_bare_jid",
    "sid_str"
  ],
  "send_call_retract_tie_break": [
    "peer_full_jid",
    "sid_str"
  ],
  "send_call_ringing": [
    "peer_bare_jid",
    "sid_str"
  ],
  "send_call_session_accept": [
    "peer_full_jid",
    "responder_full_jid",
    "sid_str",
    "audio",
    "video"
  ],
  "send_call_session_initiate": [
    "peer_full_jid",
    "initiator_full_jid",
    "sid_str",
    "audio",
    "video"
  ],
  "send_call_session_terminate": [
    "peer_full_jid",
    "sid_str",
    "reason"
  ],
  "send_call_session_terminate_with_outcome": [
    "peer_full_jid",
    "sid_str",
    "reason"
  ],
  "send_chat_message": [
    "peer_jid",
    "body",
    "options"
  ],
  "send_chat_state": [
    "to",
    "msg_type",
    "state",
    "thread_id",
    "thread_parent"
  ],
  "send_correction": [
    "to",
    "msg_type",
    "body",
    "replaces_id",
    "options"
  ],
  "send_displayed": [
    "to",
    "msg_type",
    "message_id",
    "thread_id",
    "thread_parent"
  ],
  "send_groupchat_message": [
    "room_jid",
    "body",
    "options"
  ],
  "send_in_call_reaction": [
    "to",
    "msg_type",
    "sid",
    "emoji"
  ],
  "send_moderation": [
    "to",
    "msg_type",
    "target_id",
    "reason"
  ],
  "send_muji_session_initiate": [
    "room_jid",
    "sid_str",
    "video"
  ],
  "send_muji_session_terminate": [
    "room_jid",
    "sid_str",
    "iq_id"
  ],
  "send_presence": [
    "status",
    "show",
    "idle_since"
  ],
  "send_reaction": [
    "to",
    "msg_type",
    "target_id",
    "emojis",
    "thread_id",
    "thread_parent"
  ],
  "send_retraction": [
    "to",
    "msg_type",
    "retracts_id",
    "thread_id",
    "thread_parent"
  ],
  "set_dm_notification_mode": [
    "options"
  ],
  "set_room_affiliation": [
    "room_jid",
    "jid",
    "affiliation"
  ],
  "set_room_notification_mode": [
    "options"
  ],
  "status_preference_fetch": [],
  "status_preference_publish": [
    "input"
  ],
  "stories_items": [
    "community_jid",
    "max_items"
  ],
  "stories_publish": [
    "community_jid",
    "input"
  ],
  "story_reads_fetch": [],
  "story_reads_publish": [
    "input"
  ],
  "subscribe_mds_displayed": [],
  "subscribe_to_presence": [
    "peer_jid"
  ],
  "supports_mds_publish_options": [],
  "unpin_direct_message": [
    "peer_jid",
    "target_stanza_id"
  ],
  "unpin_message": [
    "room_jid",
    "target_stanza_id"
  ],
  "update_muji_presence": [
    "room_jid",
    "nick",
    "active",
    "preparing",
    "video",
    "in_call"
  ],
  "xcal_items": [
    "community_jid",
    "max_items"
  ],
  "xcal_publish": [
    "community_jid",
    "input"
  ],
  "xcal_publish_item": [
    "community_jid",
    "item_id",
    "input"
  ],
  "xcal_retract": [
    "community_jid",
    "item_id"
  ],
  "xcal_rsvp": [
    "community_jid",
    "master_uid",
    "self_localpart",
    "self_jid",
    "partstat"
  ]
};
  const callbacks = {
    set_on_message: "message", set_on_presence: "presence",
    set_on_connected: "connected", set_on_session_lifecycle: "sessionLifecycle",
    set_on_disconnected: "disconnected", set_on_error: "error",
    set_on_message_delivery_acked: "deliveryAcked", set_on_message_delivery_failed: "deliveryFailed",
    set_on_message_rejected: "messageRejected", set_on_mds_displayed: "mdsDisplayed",
    set_on_avatar_changed: "avatarChanged", set_on_pubsub_event: "pubsub",
    set_on_call: "call", set_on_stream_management: "streamManagement",
    set_on_authentication_failed: "authenticationFailed",
  };
  const snakeKeys = {serviceType:"service_type",tieBreak:"tie_break",migratedTo:"migrated_to",publicKey:"public_key",deviceId:"device_id",notifyMode:"notify_mode",richPayloadOptIn:"rich_payload_opt_in"};
  function plain(value) {
    if (value == null) return null;
    if (typeof value === "bigint") {
      const number = Number(value);
      return Number.isSafeInteger(number) ? number : value.toString();
    }
    if (ArrayBuffer.isView(value)) return Array.from(value);
    if (value instanceof Map) return Object.fromEntries([...value].map(([k,v]) => [String(k),plain(v)]));
    if (Array.isArray(value)) return value.map(plain);
    if (typeof value === "object") return Object.fromEntries(Object.entries(value).map(([k,v]) => [snakeKeys[k] ?? k,plain(v)]));
    return value;
  }
  function inboxEntry(entry) {
    const value=plain(entry);
    if (value && value.thread_id == null && value.thread != null) { value.thread_id=value.thread;delete value.thread; }
    return value;
  }
  const booleanMethods = new Set(["disable_push_device", "disable_push_notifications", "enable_push_notifications", "is_community_owner", "pin_direct_message", "pin_message", "publish_mds_displayed", "retract_sticker_pack", "send_call_finish", "send_call_finish_migrated", "send_call_finish_with_reason", "send_call_proceed", "send_call_propose", "send_call_reject", "send_call_reject_tie_break", "send_call_retract", "send_call_retract_tie_break", "send_call_ringing", "send_call_session_accept", "send_call_session_initiate", "send_call_session_terminate", "send_chat_state", "send_displayed", "send_moderation", "send_reaction", "send_retraction", "subscribe_mds_displayed", "supports_mds_publish_options", "unpin_direct_message", "unpin_message"]);
  function outcome(method,result) {
    const value=plain(result);
    if (method === "fetch_status_preference") return value?.mode === "manual" ? {type:"manual",status:value.status} : {type:"automatic"};
    if (method === "fetch_vcard4" && value) {const {fn,...fields}=value;return {...fields,full_name:fields.full_name ?? fn ?? null};}
    if (method === "fetch_user_pep_profile" && value?.tune) {const {length,...fields}=value.tune;return {...value,tune:{...fields,length_seconds:fields.length_seconds ?? length ?? null}};}
    if (method === "fetch_inbox" && value) return {...value,all_unread:value.all_unread ?? value.total_unread,unread:value.unread ?? value.unread_conversations,conversations:(value.conversations ?? []).map(inboxEntry)};
    if (["send_chat_message","send_groupchat_message","send_correction"].includes(method)) {
      if (typeof value === "string") return {type:"sent",stanza_id:value};
      if (value && typeof value.kind === "string") {
        const {kind,...fields}=value;return {type:kind.replaceAll("-","_"),...fields};
      }
    }
    if (["set_room_notification_mode","set_dm_notification_mode"].includes(method) && value && typeof value.kind === "string") {
      const {kind,...fields}=value;return {type:kind.replaceAll("-","_"),...fields};
    }
    if (method === "send_call_session_terminate_with_outcome" && value && typeof value.kind === "string") {
      return value.kind.replaceAll("-","_");
    }
    if (method === "fetch_push_vapid_public_key") return value == null ? {type:"not_advertised"} : {type:"advertised",...value};
    if (booleanMethods.has(method) && value == null) return true;
    return value;
  }
  function messageType(args) { return args.msg_type ?? (args.audience === "muc" ? "groupchat" : "chat"); }
  function argument(name,args) {
    if (name === "size") return BigInt(args.size ?? 0);
    if (name === "msg_type") return messageType(args);
    if (Object.hasOwn(args,name)) return args[name];
    const aliases = {
      sid_str:"sid", old_sid_str:"old_sid", new_sid_str:"new_sid",
      to:"target_jid", target_id:"target_stanza_id", replaces_id:"target_id",
      retracts_id:"target_stanza_id", message_id:"stanza_id", max:"max_messages",
      options:"options", opts:"options", vcard_json:"vcard", tune_json:"tune", chat_id:"chat_jid",
    };
    if (name === "args") return args.args ?? args;
    if (name === "vcard_json") {const {full_name,...fields}=args.vcard ?? {};return {...fields,fn:full_name ?? null};}
    if (name === "mood_json" || name === "activity_json") return args;
    if (name === "tune_json") {
      const tune = args.tune ?? args;
      return {...tune,length:tune.length_seconds ?? tune.length};
    }
    if (name === "to") return args.target_jid ?? args.peer_jid;
    if (name === "service_jid") return args.service_jid ?? args.push_service_jid;
    if (name === "max") return args.max ?? args.max_messages ?? args.max_results ?? 50;
    if (name === "opts" || name === "options") return args.options ?? args;
    if (name === "body") return args.body ?? args.new_body;
    if (name === "in_call") { const flags = args.flags ?? args.in_call ?? {}; return {handRaised:flags.hand_raised ?? flags.handRaised ?? false,muted:flags.muted ?? false}; }
    if (aliases[name]) return args[aliases[name]] ?? null;
    return null;
  }
  async function createAdapter(emit, loadModule, {closeTimeout = 15000} = {}) {
    let modulePromise, client, timeline, retired = false, fullyClosed = false, generation = 0, ready = false;
    let lifecycle = Promise.resolve(), closePromise, transitions = 0;
    function serial(operation) {
      const result=lifecycle.catch(()=>{}).then(operation);
      lifecycle=result;
      return result;
    }
    async function bounded(operation) {
      let timer;
      try { return await Promise.race([operation,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error("The Rust connection has not finished closing.")),closeTimeout);})]); }
      finally { clearTimeout(timer); }
    }
    async function stopClient() {
      const old=client;
      if (!old) return;
      // The additive Rust witness joins startup, driver/event tasks and the
      // browser's CLOSED state. Typed disconnection alone can precede socket closure.
      if (typeof old.disconnect_and_wait !== "function") throw new Error("Rebuild WASM to support confirmed connection shutdown.");
      await bounded(old.disconnect_and_wait());
      client=null;old.free();
    }
    const module = () => modulePromise ??= loadModule().then(async value => { await value.default(); return value; });
    async function connect(args) {
      if (retired) throw new Error("The Rust transport is closed.");
      const current = ++generation;ready=false;
      await stopClient();
      const wasm = await module();
      if (retired || current !== generation) throw new Error("The session changed.");
      const config = new wasm.WaddleConfig(args.server_url,args.jid,args.access_token,args.resource);
      if (typeof config.set_resumable === "function") config.set_resumable(args.resumable === true);
      else if (args.resumable === false) { config.free?.();throw new Error("Rebuild WASM to support non-resuming sessions."); }
      const next = new wasm.WaddleClient(config);
      client = next;
      for (const [setter,type] of Object.entries(callbacks)) {
        if (typeof next[setter] !== "function") continue;
        next[setter](data => {
          if (!retired && current === generation && client === next) {
            if (type === "connected") ready=true;
            if (type === "disconnected" || type === "authenticationFailed") ready=false;
            emit({type,data:type === "sessionLifecycle" ? {mode:plain(data)} : plain(data)});
          }
        });
      }
      await next.connect();
      if (retired || current !== generation) throw new Error("The session changed.");
      return null;
    }
    return {
      async rpc(method,args) {
        if (retired) throw new Error("The Rust transport is closed.");
        if (method === "send_raw_iq" || method === "send_stanza") throw new Error("Raw XML is not part of the Flutter protocol boundary.");
        if (method.startsWith("timeline_")) {
          const wasm = await module();
          if (retired) throw new Error("The Rust transport is closed.");
          if (typeof wasm.WaddleFlutterTimeline !== "function") throw new Error("The shared Rust timeline is missing from this WASM build.");
          timeline ??= new wasm.WaddleFlutterTimeline();
          return plain(timeline.execute({method,args}));
        }
        if (method === "connect") {
          transitions++;
          try { return await serial(()=>connect(args)); }
          finally { transitions--; }
        }
        if (method === "disconnect") {
          transitions++;generation++;ready=false;
          try { return await serial(async()=>{await stopClient();return null;}); }
          finally { transitions--; }
        }
        if (transitions > 0) throw new Error("The Rust connection is changing.");
        if (!client) throw new Error("The XMPP client is disconnected.");
        const current = generation;
        const next = client;
        let parameters;
        let target = method;
        switch (method) {
          case "admin_channels_delete":
          case "admin_spaces_delete": parameters=[{...args,confirm:"yes"}];break;
          case "fetch_room_thread_history": target="fetch_room_history_by_thread";parameters=[args.room_jid,args.thread_id,args.max_messages,args.before_id ?? null];break;
          case "fetch_status_preference": target="status_preference_fetch";parameters=[];break;
          case "fetch_inbox": parameters=[{only_unread:args.read_filter === "unread_only",no_messages:args.message_content === "omit"}];break;
          case "fetch_push_vapid_public_key": target="fetch_vapid_public_key";parameters=[args.push_service_jid];break;
          case "register_push_device": {
            const credentials=args.credentials ?? {};
            const platform=credentials.type === "web_push" ? "web" : credentials.type;
            parameters=[{serviceJid:args.push_service_jid,appId:args.app_id,environment:args.environment === "production" ? "prod" : args.environment,
              platform,endpoint:credentials.endpoint,p256dh:credentials.p256dh,auth:credentials.auth,
              apnsToken:credentials.device_token,fcmToken:credentials.registration_token,fcmP256dh:credentials.p256dh,fcmAuth:credentials.auth}];break;
          }
          case "send_call_session_terminate":
          case "send_call_session_terminate_with_outcome": parameters=[args.peer_full_jid,args.sid,args.reason?.replaceAll("_","-") ?? null];break;
          case "send_muji_session_terminate": parameters=[args.room_jid,args.sid,`flutter-${crypto.randomUUID()}`];break;
          case "send_correction": parameters=[args.peer_jid,messageType(args),args.new_body,args.target_id,args.options ?? null];break;
          case "send_reaction": parameters=[args.target_jid,messageType(args),args.target_stanza_id,args.emojis,null,null];break;
          case "send_retraction": parameters=[args.target_jid ?? args.peer_jid,messageType(args),args.target_stanza_id ?? args.target_id,null,null];break;
          case "send_moderation": parameters=[args.target_jid ?? args.room_jid,"groupchat",args.target_stanza_id,args.reason ?? null];break;
          case "send_chat_state": parameters=[args.peer_jid,messageType(args),args.state,null,null];break;
          case "send_displayed": parameters=[args.peer_jid,messageType(args),args.stanza_id,null,null];break;
          case "set_room_notification_mode": parameters=[{roomJid:args.room_jid,mode:args.mode,name:args.name ?? null,richPayloadOptIn:args.rich_payload_opt_in ?? false}];break;
          case "set_dm_notification_mode": parameters=[{dmJid:args.dm_jid,mode:args.mode,richPayloadOptIn:args.rich_payload_opt_in ?? false}];break;
          case "set_room_affiliation": target="flutter_rpc";parameters=[{method,args}];break;
          case "publish_tune": parameters=[argument("tune_json",args)];break;
          default: {
            const signature = signatures[method];
            if (!signature && typeof next.flutter_rpc === "function") {
              const result = await next.flutter_rpc({method,args});
              if (retired || generation !== current || client !== next) throw new Error("The session changed.");
              return outcome(method,result);
            }
            if (!signature) throw new Error(`Unsupported Rust WASM method: ${method}`);
            parameters = signature.map(name => argument(name,args));
          }
        }
        if (typeof next[target] !== "function") throw new Error(`This WASM build does not expose ${target}. Rebuild its artifacts.`);
        let result;
        try { result = await next[target](...parameters); }
        catch (error) {
          if (retired || generation !== current || client !== next) throw new Error("The session changed.");
          if (method === "send_correction") return {type:ready ? "transport_error" : "not_connected"};
          throw error;
        }
        if (retired || generation !== current || client !== next) throw new Error("The session changed.");
        return outcome(method,result);
      },
      close() {
        if (fullyClosed) return Promise.resolve();
        if (closePromise) return closePromise;
        retired=true;generation++;ready=false;
        const pending=serial(async()=>{
          await stopClient();
          timeline?.free();timeline=null;fullyClosed=true;
        });
        closePromise=pending;
        pending.catch(()=>{if(closePromise===pending)closePromise=null;});
        return pending;
      },
    };
  }
  globalThis.waddleRust = {
    create: emit => createAdapter(emit, () => import(new URL("rust/waddle_xmpp_client_wasm.js",document.baseURI).href)),
  };
  // A deterministic test seam that never loads WASM or touches the network.
  if (typeof module !== "undefined" && module.exports) module.exports = {createAdapter,plain,outcome};
})();
