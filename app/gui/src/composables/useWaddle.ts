import { ref, readonly } from 'vue';

/* ------------------------------------------------------------------ */
/*  Public types                                                       */
/* ------------------------------------------------------------------ */

export interface ChatMessage {
  id: string;
  from: string;
  to: string;
  body: string;
  timestamp: string;
  messageType?: string;
  thread?: string | null;
}

export interface RosterItem {
  jid: string;
  name: string | null;
  subscription: string;
  groups: string[];
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  status: string;
  errorReason: string | null;
  errorCount: number;
  capabilities: string[];
}

export interface UiConfig {
  notifications: boolean;
  theme: string;
  locale: string | null;
  themeName: string;
  customThemePath: string | null;
}

export interface ConnectionSnapshot {
  status: 'connecting' | 'connected' | 'reconnecting' | 'offline';
  jid: string | null;
  attempt: number | null;
}

export interface RoomInfo {
  jid: string;
  name: string;
}

export type PluginAction =
  | { action: 'install'; reference: string }
  | { action: 'uninstall'; pluginId: string }
  | { action: 'update'; pluginId: string }
  | { action: 'get'; pluginId: string };

export type UnlistenFn = () => void;

export interface EventCallback<T = unknown> {
  (event: { payload: T }): void;
}

export interface WaddleTransport {
  /* connection lifecycle */
  connect(jid: string, password: string, endpoint: string): Promise<void>;
  disconnect(): Promise<void>;

  /* messaging */
  sendMessage(to: string, body: string, type?: string): Promise<ChatMessage>;
  getHistory(jid: string, limit: number, before?: string): Promise<ChatMessage[]>;

  /* roster */
  getRoster(): Promise<RosterItem[]>;
  addContact(jid: string): Promise<void>;

  /* connection state */
  getConnectionState(): Promise<ConnectionSnapshot>;

  /* presence */
  setPresence(show: string, status?: string): Promise<void>;

  /* MUC rooms */
  joinRoom(roomJid: string, nick: string): Promise<void>;
  leaveRoom(roomJid: string): Promise<void>;
  discoverMucService(): Promise<string | null>;
  listRooms(serviceJid: string): Promise<RoomInfo[]>;
  createRoom(roomJid: string, nick: string): Promise<void>;
  deleteRoom(roomJid: string): Promise<void>;

  /* plugins */
  managePlugins(action: PluginAction): Promise<PluginInfo>;

  /* config */
  getConfig(): Promise<UiConfig>;

  /* event bus */
  listen<T>(channel: string, callback: EventCallback<T>): Promise<UnlistenFn>;
}

/* ------------------------------------------------------------------ */
/*  Helpers                                                            */
/* ------------------------------------------------------------------ */

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function toFrontendEventChannel(channel: string): string {
  return channel.replace(/\./g, ':');
}

function bareJid(value: string): string {
  return value.split('/')[0] || value;
}

function randomId(prefix = 'waddle'): string {
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return `${prefix}-${crypto.randomUUID()}`;
  }
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function notConnected(method: string): Error {
  return new Error(`Not connected — cannot call ${method}. Please log in first.`);
}

/* ------------------------------------------------------------------ */
/*  Tauri transport                                                    */
/* ------------------------------------------------------------------ */

async function createTauriTransport(): Promise<WaddleTransport> {
  const { invoke } = await import('@tauri-apps/api/core');
  const { listen } = await import('@tauri-apps/api/event');

  return {
    connect: (jid, password, endpoint) =>
      invoke<void>('connect', { jid, password, endpoint }),
    disconnect: () => invoke<void>('disconnect'),
    sendMessage: (to, body, type) =>
      invoke<ChatMessage>('send_message', { to, body, messageType: type ?? 'chat' }),
    getRoster: () => invoke<RosterItem[]>('get_roster'),
    addContact: (jid) => invoke<void>('add_contact', { jid }),
    getConnectionState: () => invoke<ConnectionSnapshot>('get_connection_state'),
    setPresence: (show, status) => invoke<void>('set_presence', { show, status }),
    joinRoom: (roomJid, nick) => invoke<void>('join_room', { roomJid, nick }),
    leaveRoom: (roomJid) => invoke<void>('leave_room', { roomJid }),
    discoverMucService: () => invoke<string | null>('discover_muc_service'),
    listRooms: (serviceJid) => invoke<RoomInfo[]>('list_rooms', { serviceJid }),
    createRoom: (roomJid, nick) => invoke<void>('create_room', { roomJid, nick }),
    deleteRoom: (roomJid) => invoke<void>('delete_room', { roomJid }),
    getHistory: (jid, limit, before) =>
      invoke<ChatMessage[]>('get_history', { jid, limit, before }),
    managePlugins: (action) => invoke<PluginInfo>('manage_plugins', { action }),
    getConfig: () => invoke<UiConfig>('get_config'),
    listen: <T>(channel: string, callback: EventCallback<T>) =>
      listen<T>(toFrontendEventChannel(channel), (event) =>
        callback({ payload: event.payload }),
      ),
  };
}

/* ------------------------------------------------------------------ */
/*  Browser XMPP transport (two-phase: shell → connect)                */
/* ------------------------------------------------------------------ */

async function createBrowserXmppTransport(): Promise<WaddleTransport> {
  const { client, xml } = (await import('@xmpp/client')) as {
    client: (config: Record<string, string>) => any;
    xml: (...args: any[]) => any;
  };

  /* ---- mutable session state (reset on each connect) ---- */
  let xmpp: any = null;
  let selfJid = '';
  let connectionSnapshot: ConnectionSnapshot = {
    status: 'offline',
    jid: null,
    attempt: null,
  };
  const listeners = new Map<string, Set<EventCallback<any>>>();
  let rosterByJid = new Map<string, RosterItem>();
  let historyByJid = new Map<string, ChatMessage[]>();
  /** Tracks the nick used to join each MUC room (keyed by bare room JID). */
  let roomNickByJid = new Map<string, string>();
  /* pendingIq / pendingRoster maps removed — we use xmpp.iqCaller.request()
     which properly integrates with @xmpp/client's middleware pipeline. */

  /* ---- internal helpers ---- */

  const emit = (channel: string, type: string, data: Record<string, unknown> = {}): void => {
    const callbacks = listeners.get(channel);
    if (!callbacks || callbacks.size === 0) return;
    const envelope = { channel, payload: { type, data } };
    for (const cb of callbacks) cb({ payload: envelope });
  };

  /**
   * Store a message in the in-memory history, keyed by conversation JID.
   * For 1:1 chats the key is the other party's bare JID.
   * For MUC the key is the room's bare JID.
   */
  const upsertHistory = (message: ChatMessage): void => {
    let key: string;
    if (message.messageType === 'groupchat') {
      // MUC: key is always the room's bare JID (from the `from` field)
      key = bareJid(message.from);
    } else {
      // 1:1: key is the other party
      key = bareJid(message.from) === selfJid ? bareJid(message.to) : bareJid(message.from);
    }
    const current = historyByJid.get(key) ?? [];
    if (current.some((m) => m.id === message.id)) {
      console.debug(`[waddle:history] dup skipped for key=${key} id=${message.id}`);
      return;
    }
    historyByJid.set(key, [...current, message]);
    console.debug(`[waddle:history] stored key=${key} total=${current.length + 1} from=${message.from}`);
  };

  const parseRosterItems = (stanza: any): RosterItem[] => {
    const query =
      stanza.getChild?.('query', 'jabber:iq:roster') ?? stanza.getChild?.('query');
    if (!query) return [];
    return (query.getChildren?.('item') ?? [])
      .map((item: any) => {
        const jid = String(item.attrs?.jid ?? '').trim();
        if (!jid) return null;
        const groups = (item.getChildren?.('group') ?? [])
          .map((g: any) => {
            const t = typeof g.text === 'function' ? g.text() : g.text?.toString?.();
            return String(t ?? '').trim();
          })
          .filter((g: string) => g.length > 0);
        return {
          jid,
          name: item.attrs?.name ? String(item.attrs.name) : null,
          subscription: String(item.attrs?.subscription ?? 'none'),
          groups,
        } satisfies RosterItem;
      })
      .filter((i: RosterItem | null): i is RosterItem => i !== null);
  };

  const requireConnection = (method: string): void => {
    if (!xmpp || connectionSnapshot.status === 'offline') {
      throw notConnected(method);
    }
  };

  /**
   * Send an IQ stanza via @xmpp/client's iqCaller, which correctly integrates
   * with the middleware pipeline. Returns the result stanza.
   */
  const sendIq = (attrs: Record<string, string>, ...children: any[]): Promise<any> => {
    const id = attrs.id ?? randomId('iq');
    attrs.id = id;
    return xmpp.iqCaller.request(xml('iq', attrs, ...children));
  };

  const fetchRoster = async (): Promise<RosterItem[]> => {
    console.debug('[waddle:roster] fetching roster via iqCaller...');
    const result = await sendIq(
      { type: 'get' },
      xml('query', { xmlns: 'jabber:iq:roster' }),
    );
    const items = parseRosterItems(result);
    console.debug(`[waddle:roster] received ${items.length} items:`, items.map(i => `${i.jid}(${i.subscription})`));
    for (const item of items) rosterByJid.set(item.jid, item);
    emit('xmpp.roster.received', 'rosterReceived', {
      items: Array.from(rosterByJid.values()),
    });
    return Array.from(rosterByJid.values());
  };

  const wireXmppEvents = (): void => {
    xmpp.on('status', (status: string) => {
      if (status === 'connecting' || status === 'connect') {
        connectionSnapshot = { status: 'connecting', jid: selfJid, attempt: null };
        emit('system.coming_online', 'comingOnline', {});
      }
      if (status === 'reconnecting') {
        connectionSnapshot = { status: 'reconnecting', jid: null, attempt: 1 };
        emit('system.connection.reconnecting', 'connectionReconnecting', { attempt: 1 });
      }
    });

    xmpp.on('online', (address: any) => {
      selfJid = bareJid(String(address));
      connectionSnapshot = { status: 'connected', jid: selfJid, attempt: null };
      emit('system.connection.established', 'connectionEstablished', { jid: selfJid });

      void fetchRoster().catch((err) => {
        emit('system.error.occurred', 'errorOccurred', {
          component: 'web-xmpp',
          message: err instanceof Error ? err.message : String(err),
          recoverable: true,
        });
      });
    });

    xmpp.on('offline', () => {
      connectionSnapshot = { status: 'offline', jid: null, attempt: null };
      emit('system.connection.lost', 'connectionLost', {
        reason: 'web transport offline',
        willRetry: true,
      });
      emit('system.going_offline', 'goingOffline', {});
    });

    xmpp.on('error', (error: unknown) => {
      emit('system.error.occurred', 'errorOccurred', {
        component: 'web-xmpp',
        message: error instanceof Error ? error.message : String(error),
        recoverable: true,
      });
    });

    xmpp.on('stanza', (stanza: any) => {
      const stanzaName = stanza.name ?? stanza.getName?.() ?? '?';
      const stanzaType = stanza.attrs?.type ?? '';
      const stanzaFrom = stanza.attrs?.from ?? '';
      console.debug(`[waddle:stanza] ${stanzaName} type=${stanzaType} from=${stanzaFrom}`);

      /* ---- IQ stanzas (roster pushes) ---- */
      if (stanza.is?.('iq')) {
        // Handle roster pushes (type="set" from server, e.g., after addContact)
        if (stanza.attrs?.type === 'set') {
          const query = stanza.getChild?.('query', 'jabber:iq:roster');
          if (query) {
            const items = parseRosterItems(stanza);
            for (const item of items) {
              if (item.subscription === 'remove') {
                rosterByJid.delete(item.jid);
              } else {
                rosterByJid.set(item.jid, item);
              }
            }
            emit('xmpp.roster.received', 'rosterReceived', {
              items: Array.from(rosterByJid.values()),
            });

            // Acknowledge the roster push
            void xmpp.send(xml('iq', { type: 'result', id: stanza.attrs.id })).catch(() => {});
          }
        }
        // Note: IQ result/error stanzas are handled by @xmpp/client's iqCaller middleware.
        return;
      }

      /* ---- Presence ---- */
      if (stanza.is?.('presence')) {
        const presenceType = String(stanza.attrs?.type ?? '').trim();
        const fromJid = bareJid(String(stanza.attrs?.from ?? ''));

        if (presenceType === 'subscribe' && fromJid && fromJid !== selfJid) {
          // Auto-approve the incoming subscription request
          void xmpp.send(xml('presence', { to: fromJid, type: 'subscribed' })).catch(() => {});

          // Only subscribe back if we don't already have a 'to' or 'both' subscription.
          // This prevents an infinite subscribe loop when both sides auto-approve.
          const existing = rosterByJid.get(fromJid);
          const alreadySubscribed =
            existing?.subscription === 'to' || existing?.subscription === 'both';
          if (!alreadySubscribed) {
            void xmpp.send(xml('presence', { to: fromJid, type: 'subscribe' })).catch(() => {});
          }

          // Roster push from server will trigger update via the IQ handler above.
          return;
        }

        // Ignore 'subscribed' — the server sends a roster push which we handle above.
        if (presenceType === 'subscribed') return;

        return;
      }

      /* ---- Messages ---- */
      if (!stanza.is?.('message')) return;

      const receipt = stanza.getChild?.('received', 'urn:xmpp:receipts');
      const receiptId = String(receipt?.attrs?.id ?? '').trim();
      if (receiptId) {
        emit('xmpp.message.delivered', 'messageDelivered', {
          id: receiptId,
          to: bareJid(String(stanza.attrs?.from ?? '')),
        });
        return;
      }

      const body = stanza.getChildText?.('body');
      if (typeof body !== 'string' || body.trim().length === 0) return;

      const rawFrom = String(stanza.attrs?.from ?? selfJid);
      const rawTo = String(stanza.attrs?.to ?? selfJid);
      const msgType = String(stanza.attrs?.type ?? 'chat');
      const isGroupchat = msgType === 'groupchat';

      // For MUC: preserve full JID (room@conf/nick) so the UI can extract the nick.
      // For 1:1: use bare JID as before.
      const from = isGroupchat ? rawFrom : bareJid(rawFrom);
      const to = isGroupchat ? bareJid(rawTo) : bareJid(rawTo);

      // Parse XEP-0203 <delay> for historical MUC messages
      const delay = stanza.getChild?.('delay', 'urn:xmpp:delay');
      const stamp = delay?.attrs?.stamp;
      const timestamp =
        stamp && !Number.isNaN(Date.parse(stamp))
          ? new Date(stamp).toISOString()
          : new Date().toISOString();

      const message: ChatMessage = {
        id: String(stanza.attrs?.id ?? randomId('msg')),
        from,
        to,
        body,
        timestamp,
        messageType: msgType,
        thread: null,
      };

      console.debug(`[waddle:msg] type=${msgType} from=${from} to=${to} body="${body.slice(0, 50)}" ts=${timestamp}`);
      upsertHistory(message);

      // For MUC: the server echoes our own messages back from room/nick.
      // Detect self-echo by checking if the nick matches our room nick.
      const isSelfEcho = isGroupchat &&
        roomNickByJid.get(bareJid(rawFrom)) === rawFrom.split('/')[1];

      if (!isSelfEcho && bareJid(rawFrom) !== selfJid) {
        emit('xmpp.message.received', 'messageReceived', { message });
      } else {
        console.debug(`[waddle:msg] suppressed (selfEcho=${isSelfEcho}, fromSelf=${bareJid(rawFrom) === selfJid})`);
      }
    });
  };

  /* ---- transport object ---- */
  const transport: WaddleTransport = {
    /* ---------- connection lifecycle ---------- */
    connect: async (jid: string, password: string, endpoint: string) => {
      // Tear down previous session if any
      if (xmpp) {
        try { await xmpp.stop(); } catch { /* ignore */ }
        xmpp = null;
      }

      // Reset session state
      rosterByJid = new Map();
      historyByJid = new Map();
      roomNickByJid = new Map();

      const bare = bareJid(jid);
      const [username, domain] = bare.split('@', 2);
      if (!username || !domain) {
        throw new Error(`Invalid JID: ${jid}`);
      }
      const [, resourcePart] = jid.split('/', 2);
      selfJid = bare;

      connectionSnapshot = { status: 'connecting', jid: bare, attempt: null };

      xmpp = client({
        service: endpoint,
        domain,
        username,
        password,
        resource: resourcePart || 'waddle-web',
      });

      wireXmppEvents();

      // Wait for 'online' or 'error', with a 15-second timeout
      await new Promise<void>((resolve, reject) => {
        let settled = false;
        const connectTimeout = setTimeout(() => {
          if (settled) return;
          settled = true;
          cleanup();
          connectionSnapshot = { status: 'offline', jid: null, attempt: null };
          try { xmpp?.stop(); } catch { /* ignore */ }
          reject(new Error('Connection timed out — server did not respond within 15 seconds.'));
        }, 15_000);

        const onOnline = () => {
          if (settled) return;
          settled = true;
          clearTimeout(connectTimeout);
          cleanup();
          resolve();
        };
        const onError = (err: unknown) => {
          if (settled) return;
          settled = true;
          clearTimeout(connectTimeout);
          cleanup();
          connectionSnapshot = { status: 'offline', jid: null, attempt: null };
          reject(err instanceof Error ? err : new Error(String(err)));
        };
        const cleanup = () => {
          xmpp?.removeListener?.('online', onOnline);
          xmpp?.removeListener?.('error', onError);
        };

        xmpp.on('online', onOnline);
        xmpp.on('error', onError);

        void xmpp.start().catch(onError);
      });
    },

    disconnect: async () => {
      if (xmpp) {
        try { await xmpp.stop(); } catch { /* ignore */ }
        xmpp = null;
      }
      connectionSnapshot = { status: 'offline', jid: null, attempt: null };
      rosterByJid = new Map();
      historyByJid = new Map();
      roomNickByJid = new Map();

      emit('system.connection.lost', 'connectionLost', {
        reason: 'user disconnected',
        willRetry: false,
      });
      emit('system.going_offline', 'goingOffline', {});
    },

    /* ---------- messaging ---------- */
    sendMessage: async (to, body, type) => {
      requireConnection('sendMessage');
      const normalizedTo = bareJid(to);
      const msgType = type ?? 'chat';
      const isGroupchat = msgType === 'groupchat';

      // For MUC: use room/nick as `from` so UI can identify sender.
      const nick = isGroupchat ? roomNickByJid.get(normalizedTo) : undefined;
      const fromJid = isGroupchat && nick
        ? `${normalizedTo}/${nick}`
        : selfJid;

      const message: ChatMessage = {
        id: randomId('msg'),
        from: fromJid,
        to: normalizedTo,
        body,
        timestamp: new Date().toISOString(),
        messageType: msgType,
        thread: null,
      };

      const children: any[] = [xml('body', {}, body)];
      if (msgType === 'chat') {
        children.push(xml('request', { xmlns: 'urn:xmpp:receipts' }));
      }

      await xmpp.send(
        xml('message', { to: normalizedTo, type: msgType, id: message.id }, ...children),
      );

      // For groupchat, the server echoes our message back — don't double-insert.
      // For 1:1, store immediately.
      if (!isGroupchat) {
        upsertHistory(message);
      }
      emit('xmpp.message.sent', 'messageSent', { message });
      return message;
    },

    getHistory: async (jid, limit, before) => {
      const normalizedJid = bareJid(jid);
      const all = historyByJid.get(normalizedJid) ?? [];
      console.debug(`[waddle:getHistory] jid=${normalizedJid} found=${all.length} keys=[${Array.from(historyByJid.keys()).join(', ')}]`);
      const filtered = before
        ? all.filter((m) => Date.parse(m.timestamp) < Date.parse(before))
        : all;
      const sorted = [...filtered].sort(
        (a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp),
      );
      return sorted.slice(0, Math.max(1, limit));
    },

    /* ---------- roster ---------- */
    getRoster: async () => {
      requireConnection('getRoster');
      let items: RosterItem[];
      if (rosterByJid.size > 0) {
        items = Array.from(rosterByJid.values());
      } else {
        items = await fetchRoster();
      }
      // Inject self if missing
      if (selfJid && !items.some((i) => bareJid(i.jid) === selfJid)) {
        items = [
          {
            jid: selfJid,
            name: selfJid.split('@')[0] || selfJid,
            subscription: 'self',
            groups: ['Self'],
          },
          ...items,
        ];
      }
      return items;
    },

    addContact: async (jid) => {
      requireConnection('addContact');
      const normalizedJid = bareJid(jid);

      // Use sendIq (via iqCaller) so the result is properly handled
      await sendIq(
        { type: 'set' },
        xml('query', { xmlns: 'jabber:iq:roster' }, xml('item', { jid: normalizedJid })),
      );

      // Request presence subscription
      await xmpp.send(xml('presence', { to: normalizedJid, type: 'subscribe' }));

      // Refresh roster to pick up the new entry
      await fetchRoster();
      emit('xmpp.roster.updated', 'rosterUpdated', {
        items: Array.from(rosterByJid.values()),
      });
    },

    /* ---------- connection state ---------- */
    getConnectionState: async () => ({ ...connectionSnapshot }),

    /* ---------- presence ---------- */
    setPresence: async (show, status) => {
      requireConnection('setPresence');
      if (show === 'unavailable') {
        await xmpp.send(xml('presence', { type: 'unavailable' }));
        return;
      }
      const children: any[] = [];
      if (show && show !== 'available') children.push(xml('show', {}, show));
      if (status) children.push(xml('status', {}, status));
      await xmpp.send(xml('presence', {}, ...children));
    },

    /* ---------- MUC rooms ---------- */
    joinRoom: async (roomJid, nick) => {
      requireConnection('joinRoom');
      const room = bareJid(roomJid);
      await xmpp.send(
        xml(
          'presence',
          { to: `${room}/${nick}` },
          xml(
            'x',
            { xmlns: 'http://jabber.org/protocol/muc' },
            xml('history', { maxstanzas: '50' }),
          ),
        ),
      );
      roomNickByJid.set(room, nick);
    },

    leaveRoom: async (roomJid) => {
      requireConnection('leaveRoom');
      const room = bareJid(roomJid);
      const nick = roomNickByJid.get(room) || selfJid.split('@')[0] || 'user';
      await xmpp.send(xml('presence', { to: `${room}/${nick}`, type: 'unavailable' }));
      roomNickByJid.delete(room);
    },

    discoverMucService: async () => {
      requireConnection('discoverMucService');
      const domain = selfJid.split('@')[1];
      if (!domain) return null;

      try {
        const result = await sendIq(
          { type: 'get', to: domain },
          xml('query', { xmlns: 'http://jabber.org/protocol/disco#items' }),
        );

        const query = result.getChild?.('query', 'http://jabber.org/protocol/disco#items');
        if (!query) return null;

        const items = query.getChildren?.('item') ?? [];
        for (const item of items) {
          const itemJid = String(item.attrs?.jid ?? '');
          // Check if this service is a MUC by querying its identity
          try {
            const infoResult = await sendIq(
              { type: 'get', to: itemJid },
              xml('query', { xmlns: 'http://jabber.org/protocol/disco#info' }),
            );
            const infoQuery = infoResult.getChild?.(
              'query',
              'http://jabber.org/protocol/disco#info',
            );
            const identities = infoQuery?.getChildren?.('identity') ?? [];
            for (const id of identities) {
              if (id.attrs?.category === 'conference' && id.attrs?.type === 'text') {
                return itemJid;
              }
            }
          } catch {
            // Skip items that don't respond to disco#info
          }
        }

        // Fallback: try the conventional subdomain
        return `conference.${domain}`;
      } catch {
        // disco#items failed entirely — fall back to convention
        return domain ? `conference.${domain}` : null;
      }
    },

    listRooms: async (serviceJid) => {
      requireConnection('listRooms');
      const result = await sendIq(
        { type: 'get', to: serviceJid },
        xml('query', { xmlns: 'http://jabber.org/protocol/disco#items' }),
      );

      const query = result.getChild?.('query', 'http://jabber.org/protocol/disco#items');
      if (!query) return [];

      return (query.getChildren?.('item') ?? [])
        .map((item: any) => ({
          jid: String(item.attrs?.jid ?? ''),
          name: String(item.attrs?.name ?? item.attrs?.jid ?? ''),
        }))
        .filter((r: RoomInfo) => r.jid.length > 0);
    },

    createRoom: async (roomJid, nick) => {
      requireConnection('createRoom');
      const room = bareJid(roomJid);
      // Join the room (creates it if it doesn't exist)
      await xmpp.send(
        xml(
          'presence',
          { to: `${room}/${nick}` },
          xml('x', { xmlns: 'http://jabber.org/protocol/muc' }),
        ),
      );
      roomNickByJid.set(room, nick);

      // Accept instant room defaults (send empty config form)
      // Small delay to let the server register us as owner
      await new Promise((r) => setTimeout(r, 300));

      await sendIq(
        { type: 'set', to: bareJid(roomJid) },
        xml(
          'query',
          { xmlns: 'http://jabber.org/protocol/muc#owner' },
          xml('x', { xmlns: 'jabber:x:data', type: 'submit' }),
        ),
      );
    },

    deleteRoom: async (roomJid) => {
      requireConnection('deleteRoom');
      await sendIq(
        { type: 'set', to: bareJid(roomJid) },
        xml(
          'query',
          { xmlns: 'http://jabber.org/protocol/muc#owner' },
          xml('destroy', {}),
        ),
      );
    },

    /* ---------- plugins ---------- */
    managePlugins: async (action) => ({
      id: action.action === 'get' ? action.pluginId : 'web-xmpp',
      name: 'Web XMPP transport',
      version: '0.1.0-web',
      status: 'active',
      errorReason: null,
      errorCount: 0,
      capabilities: [],
    }),

    /* ---------- config ---------- */
    getConfig: async () => ({
      notifications: true,
      theme: 'light',
      locale: 'en-US',
      themeName: 'light',
      customThemePath: null,
    }),

    /* ---------- event bus ---------- */
    listen: async <T>(channel: string, callback: EventCallback<T>) => {
      const callbacks = listeners.get(channel) ?? new Set<EventCallback<any>>();
      callbacks.add(callback as EventCallback<any>);
      listeners.set(channel, callbacks);
      return () => {
        const set = listeners.get(channel);
        if (!set) return;
        set.delete(callback as EventCallback<any>);
        if (set.size === 0) listeners.delete(channel);
      };
    },
  };

  return transport;
}

/* ------------------------------------------------------------------ */
/*  Mock / fallback transport                                          */
/* ------------------------------------------------------------------ */

function createDisconnectedTransport(): WaddleTransport {
  return {
    connect: async () => {
      throw new Error(
        'No XMPP transport available. Browser XMPP library failed to load.',
      );
    },
    disconnect: async () => {},
    sendMessage: async () => { throw notConnected('sendMessage'); },
    getHistory: async () => [],
    getRoster: async () => [],
    addContact: async () => { throw notConnected('addContact'); },
    getConnectionState: async () => ({ status: 'offline', jid: null, attempt: null }),
    setPresence: async () => { throw notConnected('setPresence'); },
    joinRoom: async () => { throw notConnected('joinRoom'); },
    leaveRoom: async () => { throw notConnected('leaveRoom'); },
    discoverMucService: async () => null,
    listRooms: async () => [],
    createRoom: async () => { throw notConnected('createRoom'); },
    deleteRoom: async () => { throw notConnected('deleteRoom'); },
    managePlugins: async (action) => ({
      id: action.action === 'get' ? action.pluginId : 'fallback',
      name: 'Disconnected',
      version: '0.0.0',
      status: 'unavailable',
      errorReason: 'Not connected',
      errorCount: 0,
      capabilities: [],
    }),
    getConfig: async () => ({
      notifications: true,
      theme: 'light',
      locale: 'en-US',
      themeName: 'light',
      customThemePath: null,
    }),
    listen: async () => () => {},
  };
}

/* ------------------------------------------------------------------ */
/*  Transport singleton — supports reset for disconnect→reconnect      */
/* ------------------------------------------------------------------ */

let currentTransport: WaddleTransport | null = null;
let transportInitPromise: Promise<WaddleTransport> | null = null;
const ready = ref(false);
const connected = ref(false);

async function initTransport(): Promise<WaddleTransport> {
  if (isTauri()) {
    return createTauriTransport();
  }
  try {
    return await createBrowserXmppTransport();
  } catch (err) {
    console.warn('[waddle] browser XMPP transport failed, using disconnected shell', err);
    return createDisconnectedTransport();
  }
}

function getTransport(): Promise<WaddleTransport> {
  if (!transportInitPromise) {
    transportInitPromise = initTransport().then((t) => {
      currentTransport = t;
      ready.value = true;
      return t;
    });
  }
  return transportInitPromise;
}

/** Reset the singleton so a fresh transport is created on next access. */
function resetTransport(): void {
  currentTransport = null;
  transportInitPromise = null;
  ready.value = false;
  connected.value = false;
}

/* ------------------------------------------------------------------ */
/*  Public composable                                                  */
/* ------------------------------------------------------------------ */

export function useWaddle() {
  const transport = getTransport();

  async function connect(jid: string, password: string, endpoint: string): Promise<void> {
    const t = await transport;
    await t.connect(jid, password, endpoint);
    connected.value = true;
  }

  async function disconnect(): Promise<void> {
    const t = await transport;
    await t.disconnect();
    connected.value = false;
    // Reset so the next login gets a fresh transport
    resetTransport();
  }

  async function sendMessage(to: string, body: string, type?: string): Promise<ChatMessage> {
    return (await transport).sendMessage(to, body, type);
  }

  async function getRoster(): Promise<RosterItem[]> {
    return (await transport).getRoster();
  }

  async function addContact(jid: string): Promise<void> {
    return (await transport).addContact(jid);
  }

  async function getConnectionState(): Promise<ConnectionSnapshot> {
    return (await transport).getConnectionState();
  }

  async function setPresence(show: string, status?: string): Promise<void> {
    return (await transport).setPresence(show, status);
  }

  async function joinRoom(roomJid: string, nick: string): Promise<void> {
    return (await transport).joinRoom(roomJid, nick);
  }

  async function leaveRoom(roomJid: string): Promise<void> {
    return (await transport).leaveRoom(roomJid);
  }

  async function discoverMucService(): Promise<string | null> {
    return (await transport).discoverMucService();
  }

  async function listRooms(serviceJid: string): Promise<RoomInfo[]> {
    return (await transport).listRooms(serviceJid);
  }

  async function createRoom(roomJid: string, nick: string): Promise<void> {
    return (await transport).createRoom(roomJid, nick);
  }

  async function deleteRoom(roomJid: string): Promise<void> {
    return (await transport).deleteRoom(roomJid);
  }

  async function getHistory(jid: string, limit: number, before?: string): Promise<ChatMessage[]> {
    return (await transport).getHistory(jid, limit, before);
  }

  async function managePlugins(action: PluginAction): Promise<PluginInfo> {
    return (await transport).managePlugins(action);
  }

  async function getConfig(): Promise<UiConfig> {
    return (await transport).getConfig();
  }

  async function listen<T>(channel: string, callback: EventCallback<T>): Promise<UnlistenFn> {
    return (await transport).listen(channel, callback);
  }

  return {
    ready: readonly(ready),
    connected: readonly(connected),
    connect,
    disconnect,
    sendMessage,
    getRoster,
    addContact,
    getConnectionState,
    setPresence,
    joinRoom,
    leaveRoom,
    discoverMucService,
    listRooms,
    createRoom,
    deleteRoom,
    getHistory,
    managePlugins,
    getConfig,
    listen,
  };
}
