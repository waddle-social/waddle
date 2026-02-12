import { ref, readonly } from 'vue';

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
  sendMessage(to: string, body: string): Promise<ChatMessage>;
  getRoster(): Promise<RosterItem[]>;
  addContact(jid: string): Promise<void>;
  getConnectionState(): Promise<ConnectionSnapshot>;
  setPresence(show: string, status?: string): Promise<void>;
  joinRoom(roomJid: string, nick: string): Promise<void>;
  leaveRoom(roomJid: string): Promise<void>;
  getHistory(jid: string, limit: number, before?: string): Promise<ChatMessage[]>;
  managePlugins(action: PluginAction): Promise<PluginInfo>;
  getConfig(): Promise<UiConfig>;
  listen<T>(channel: string, callback: EventCallback<T>): Promise<UnlistenFn>;
}

function isTauri(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function toFrontendEventChannel(channel: string): string {
  return channel.replace(/\./g, ':');
}

async function createTauriTransport(): Promise<WaddleTransport> {
  const { invoke } = await import('@tauri-apps/api/core');
  const { listen } = await import('@tauri-apps/api/event');

  return {
    sendMessage: (to, body) => invoke<ChatMessage>('send_message', { to, body }),
    getRoster: () => invoke<RosterItem[]>('get_roster'),
    addContact: (jid) => invoke<void>('add_contact', { jid }),
    getConnectionState: () => invoke<ConnectionSnapshot>('get_connection_state'),
    setPresence: (show, status) => invoke<void>('set_presence', { show, status }),
    joinRoom: (roomJid, nick) => invoke<void>('join_room', { roomJid, nick }),
    leaveRoom: (roomJid) => invoke<void>('leave_room', { roomJid }),
    getHistory: (jid, limit, before) => invoke<ChatMessage[]>('get_history', { jid, limit, before }),
    managePlugins: (action) => invoke<PluginInfo>('manage_plugins', { action }),
    getConfig: () => invoke<UiConfig>('get_config'),
    listen: <T>(channel: string, callback: EventCallback<T>) =>
      listen<T>(toFrontendEventChannel(channel), (event) => callback({ payload: event.payload })),
  };
}

function createMockWebTransport(reason: string): WaddleTransport {
  const unsupported = async <T>(): Promise<T> => {
    throw new Error(reason);
  };

  return {
    sendMessage: unsupported,
    getRoster: unsupported,
    addContact: unsupported,
    getConnectionState: async () => ({
      status: 'offline',
      jid: null,
      attempt: null,
    }),
    setPresence: unsupported,
    joinRoom: unsupported,
    leaveRoom: unsupported,
    getHistory: unsupported,
    managePlugins: async (action) => ({
      id: action.action === 'get' ? action.pluginId : 'web-transport',
      name: 'Web transport unavailable',
      version: '0.0.0-web',
      status: 'unavailable',
      errorReason: reason,
      errorCount: 1,
      capabilities: [],
    }),
    getConfig: async () => ({
      notifications: true,
      theme: 'light',
      locale: 'en-US',
      themeName: 'light',
      customThemePath: null,
    }),
    listen: async <T>(channel: string, callback: EventCallback<T>) => {
      if (channel === 'system.connection.lost') {
        queueMicrotask(() => {
          callback({
            payload: {
              channel,
              payload: {
                type: 'connectionLost',
                data: {
                  reason,
                  willRetry: false,
                },
              },
            } as T,
          });
        });
      }

      return () => {};
    },
  };
}

interface WebXmppConfig {
  jid: string;
  username: string;
  domain: string;
  resource: string;
  password: string;
  service: string;
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

function parseWebXmppConfig(): WebXmppConfig | null {
  const env = import.meta.env as Record<string, string | undefined>;
  const jid = env.VITE_XMPP_JID?.trim();
  const password = env.VITE_XMPP_PASSWORD?.trim();
  const service = env.VITE_XMPP_WEBSOCKET_URL?.trim();

  if (!jid || !password || !service) {
    return null;
  }

  const [jidWithoutResource = '', resourcePart] = jid.split('/', 2);
  const [username, domain] = jidWithoutResource.split('@', 2);

  if (!username || !domain) {
    return null;
  }

  return {
    jid,
    username,
    domain,
    resource: resourcePart || 'waddle-web',
    password,
    service,
  };
}

async function createBrowserXmppTransport(): Promise<WaddleTransport> {
  const config = parseWebXmppConfig();
  if (!config) {
    throw new Error(
      'missing VITE_XMPP_JID, VITE_XMPP_PASSWORD, or VITE_XMPP_WEBSOCKET_URL for web XMPP transport',
    );
  }

  const { client, xml } = (await import('@xmpp/client')) as {
    client: (config: Record<string, string>) => any;
    xml: (...args: any[]) => any;
  };

  const xmpp = client({
    service: config.service,
    domain: config.domain,
    username: config.username,
    password: config.password,
    resource: config.resource,
  });

  const listeners = new Map<string, Set<EventCallback<any>>>();
  const rosterByJid = new Map<string, RosterItem>();
  const historyByJid = new Map<string, ChatMessage[]>();
  const pendingRoster = new Map<
    string,
    {
      resolve: (items: RosterItem[]) => void;
      reject: (error: Error) => void;
      timeout: ReturnType<typeof setTimeout>;
    }
  >();

  let selfJid = bareJid(config.jid);
  let connectionSnapshot: ConnectionSnapshot = {
    status: 'connecting',
    jid: selfJid,
    attempt: null,
  };

  const emit = (channel: string, type: string, data: Record<string, unknown> = {}): void => {
    const callbacks = listeners.get(channel);
    if (!callbacks || callbacks.size === 0) {
      return;
    }

    const envelope = {
      channel,
      payload: {
        type,
        data,
      },
    };

    for (const callback of callbacks) {
      callback({ payload: envelope });
    }
  };

  const upsertHistory = (message: ChatMessage): void => {
    const key =
      bareJid(message.from) === selfJid ? bareJid(message.to) : bareJid(message.from);

    const current = historyByJid.get(key) ?? [];
    const alreadyPresent = current.some((existing) => existing.id === message.id);
    if (alreadyPresent) {
      return;
    }

    historyByJid.set(key, [...current, message]);
  };

  const parseRosterItems = (stanza: any): RosterItem[] => {
    const query = stanza.getChild?.('query', 'jabber:iq:roster') ?? stanza.getChild?.('query');
    if (!query) {
      return [];
    }

    const items = query.getChildren?.('item') ?? [];
    return items
      .map((item: any) => {
        const jid = String(item.attrs?.jid ?? '').trim();
        if (!jid) {
          return null;
        }

        const groups = (item.getChildren?.('group') ?? [])
          .map((group: any) => {
            const textValue =
              typeof group.text === 'function' ? group.text() : group.text?.toString?.();
            return String(textValue ?? '').trim();
          })
          .filter((group: string) => group.length > 0);

        return {
          jid,
          name: item.attrs?.name ? String(item.attrs.name) : null,
          subscription: String(item.attrs?.subscription ?? 'none'),
          groups,
        } satisfies RosterItem;
      })
      .filter((item: RosterItem | null): item is RosterItem => item !== null);
  };

  const fetchRoster = async (): Promise<RosterItem[]> => {
    const requestId = randomId('roster');

    const response = new Promise<RosterItem[]>((resolve, reject) => {
      const timeout = setTimeout(() => {
        pendingRoster.delete(requestId);
        reject(new Error('roster request timed out'));
      }, 5000);

      pendingRoster.set(requestId, { resolve, reject, timeout });
    });

    await xmpp.send(xml('iq', { type: 'get', id: requestId }, xml('query', { xmlns: 'jabber:iq:roster' })));
    return response;
  };

  xmpp.on('status', (status: string) => {
    if (status === 'connecting' || status === 'connect') {
      connectionSnapshot = {
        status: 'connecting',
        jid: selfJid,
        attempt: null,
      };
      emit('system.coming_online', 'comingOnline', {});
    }

    if (status === 'reconnecting') {
      connectionSnapshot = {
        status: 'reconnecting',
        jid: null,
        attempt: 1,
      };
      emit('system.connection.reconnecting', 'connectionReconnecting', { attempt: 1 });
    }
  });

  xmpp.on('online', (address: any) => {
    selfJid = bareJid(String(address));
    connectionSnapshot = {
      status: 'connected',
      jid: selfJid,
      attempt: null,
    };
    emit('system.connection.established', 'connectionEstablished', { jid: selfJid });
    emit('system.coming_online', 'comingOnline', {});

    void fetchRoster().catch((error) => {
      emit('system.error.occurred', 'errorOccurred', {
        component: 'web-xmpp',
        message: error instanceof Error ? error.message : String(error),
        recoverable: true,
      });
    });
  });

  xmpp.on('offline', () => {
    connectionSnapshot = {
      status: 'offline',
      jid: null,
      attempt: null,
    };
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
    // --- IQ stanzas (roster responses) ---
    if (stanza.is?.('iq')) {
      const query = stanza.getChild?.('query', 'jabber:iq:roster');
      if (query) {
        const items = parseRosterItems(stanza);

        for (const item of items) {
          rosterByJid.set(item.jid, item);
        }

        emit('xmpp.roster.received', 'rosterReceived', {
          items: Array.from(rosterByJid.values()),
        });

        const requestId = String(stanza.attrs?.id ?? '');
        const pending = pendingRoster.get(requestId);
        if (pending) {
          clearTimeout(pending.timeout);
          pending.resolve(Array.from(rosterByJid.values()));
          pendingRoster.delete(requestId);
        }

        return;
      }
    }

    // --- Presence stanzas (subscription handling) ---
    if (stanza.is?.('presence')) {
      const presenceType = String(stanza.attrs?.type ?? '').trim();
      const fromJid = bareJid(String(stanza.attrs?.from ?? ''));

      if (presenceType === 'subscribe' && fromJid && fromJid !== selfJid) {
        // Auto-approve the incoming subscription request
        void xmpp.send(xml('presence', { to: fromJid, type: 'subscribed' })).catch(() => {});

        // Reciprocally subscribe so both sides see each other
        void xmpp.send(xml('presence', { to: fromJid, type: 'subscribe' })).catch(() => {});

        // Re-fetch roster from server after a short delay to let the server process
        setTimeout(() => {
          void fetchRoster()
            .then(() => {
              emit('xmpp.roster.updated', 'rosterUpdated', {
                items: Array.from(rosterByJid.values()),
              });
            })
            .catch(() => {});
        }, 500);

        return;
      }

      // When the server confirms our subscription was approved, refresh the roster
      if (presenceType === 'subscribed' && fromJid) {
        setTimeout(() => {
          void fetchRoster()
            .then(() => {
              emit('xmpp.roster.updated', 'rosterUpdated', {
                items: Array.from(rosterByJid.values()),
              });
            })
            .catch(() => {});
        }, 500);

        return;
      }

      // Ignore other presence stanzas for now (available, unavailable, etc.)
      return;
    }

    // --- Message stanzas ---
    if (!stanza.is?.('message')) {
      return;
    }

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
    if (typeof body !== 'string' || body.trim().length === 0) {
      return;
    }

    const from = bareJid(String(stanza.attrs?.from ?? selfJid));
    const to = bareJid(String(stanza.attrs?.to ?? selfJid));
    const message: ChatMessage = {
      id: String(stanza.attrs?.id ?? randomId('msg')),
      from,
      to,
      body,
      timestamp: new Date().toISOString(),
      messageType: String(stanza.attrs?.type ?? 'chat'),
      thread: null,
    };

    upsertHistory(message);

    if (from !== selfJid) {
      emit('xmpp.message.received', 'messageReceived', { message });
    }
  });

  await xmpp.start();

  return {
    sendMessage: async (to, body) => {
      const normalizedTo = bareJid(to);
      const message: ChatMessage = {
        id: randomId('msg'),
        from: selfJid,
        to: normalizedTo,
        body,
        timestamp: new Date().toISOString(),
        messageType: 'chat',
        thread: null,
      };

      await xmpp.send(
        xml(
          'message',
          { to: normalizedTo, type: 'chat', id: message.id },
          xml('body', {}, body),
          xml('request', { xmlns: 'urn:xmpp:receipts' }),
        ),
      );

      upsertHistory(message);
      emit('xmpp.message.sent', 'messageSent', { message });

      return message;
    },
    getRoster: async () => {
      let items: RosterItem[];

      if (rosterByJid.size > 0) {
        items = Array.from(rosterByJid.values());
      } else {
        items = await fetchRoster();
      }

      // Inject a synthetic entry for the connected user so they always see themselves
      const selfAlreadyPresent = items.some((item) => bareJid(item.jid) === selfJid);
      if (!selfAlreadyPresent && selfJid) {
        const selfLocalpart = selfJid.split('@')[0] || selfJid;
        items = [
          {
            jid: selfJid,
            name: selfLocalpart,
            subscription: 'self',
            groups: ['Self'],
          },
          ...items,
        ];
      }

      return items;
    },
    addContact: async (jid) => {
      const normalizedJid = bareJid(jid);

      // 1. Add the contact to the server-side roster via roster-set IQ
      const setId = randomId('roster-set');
      await xmpp.send(
        xml(
          'iq',
          { type: 'set', id: setId },
          xml(
            'query',
            { xmlns: 'jabber:iq:roster' },
            xml('item', { jid: normalizedJid }),
          ),
        ),
      );

      // 2. Request presence subscription so both sides can see each other
      await xmpp.send(xml('presence', { to: normalizedJid, type: 'subscribe' }));

      // 3. Re-fetch the roster from the server to pick up the new entry
      await fetchRoster();

      emit('xmpp.roster.updated', 'rosterUpdated', {
        items: Array.from(rosterByJid.values()),
      });
    },
    getConnectionState: async () => ({ ...connectionSnapshot }),
    setPresence: async (show, status) => {
      if (show === 'unavailable') {
        await xmpp.send(xml('presence', { type: 'unavailable' }));
        return;
      }

      const children = [] as any[];
      if (show && show !== 'available') {
        children.push(xml('show', {}, show));
      }
      if (status) {
        children.push(xml('status', {}, status));
      }

      await xmpp.send(xml('presence', {}, ...children));
    },
    joinRoom: async (roomJid, nick) => {
      await xmpp.send(
        xml(
          'presence',
          { to: `${bareJid(roomJid)}/${nick}` },
          xml('x', { xmlns: 'http://jabber.org/protocol/muc' }),
        ),
      );
    },
    leaveRoom: async (roomJid) => {
      await xmpp.send(xml('presence', { to: bareJid(roomJid), type: 'unavailable' }));
    },
    getHistory: async (jid, limit, before) => {
      const normalizedJid = bareJid(jid);
      const all = historyByJid.get(normalizedJid) ?? [];

      const filtered = before
        ? all.filter((message) => Date.parse(message.timestamp) < Date.parse(before))
        : all;

      const sorted = [...filtered].sort(
        (left, right) => Date.parse(right.timestamp) - Date.parse(left.timestamp),
      );

      return sorted.slice(0, Math.max(1, limit));
    },
    managePlugins: async (action) => ({
      id: action.action === 'get' ? action.pluginId : 'web-xmpp',
      name: 'Web XMPP transport',
      version: '0.1.0-web',
      status: 'active',
      errorReason: null,
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
    listen: async <T>(channel: string, callback: EventCallback<T>) => {
      const callbacks = listeners.get(channel) ?? new Set<EventCallback<any>>();
      callbacks.add(callback as EventCallback<any>);
      listeners.set(channel, callbacks);

      return () => {
        const set = listeners.get(channel);
        if (!set) {
          return;
        }

        set.delete(callback as EventCallback<any>);
        if (set.size === 0) {
          listeners.delete(channel);
        }
      };
    },
  };
}

async function createWasmTransport(): Promise<WaddleTransport> {
  const wasmModuleName = 'waddle-wasm';

  try {
    const { WaddleCore } = (await import(/* @vite-ignore */ wasmModuleName)) as {
      WaddleCore: {
        init(): Promise<{
          send_message(to: string, body: string): Promise<ChatMessage>;
          get_roster(): Promise<RosterItem[]>;
          set_presence(show: string, status?: string): Promise<void>;
          join_room(roomJid: string, nick: string): Promise<void>;
          leave_room(roomJid: string): Promise<void>;
          get_history(jid: string, limit: number, before?: string): Promise<ChatMessage[]>;
          manage_plugins(action: PluginAction): Promise<PluginInfo>;
          get_config(): Promise<UiConfig>;
          on<T>(channel: string, callback: (payload: T) => void): () => void;
        }>;
      };
    };
    const core = await WaddleCore.init();

    return {
      sendMessage: (to, body) => core.send_message(to, body),
      getRoster: () => core.get_roster(),
      addContact: async () => {
        throw new Error('addContact is not supported in the WASM transport');
      },
      getConnectionState: async () => ({
        status: 'offline',
        jid: null,
        attempt: null,
      }),
      setPresence: (show, status) => core.set_presence(show, status),
      joinRoom: (roomJid, nick) => core.join_room(roomJid, nick),
      leaveRoom: (roomJid) => core.leave_room(roomJid),
      getHistory: (jid, limit, before) => core.get_history(jid, limit, before),
      managePlugins: (action) => core.manage_plugins(action),
      getConfig: () => core.get_config(),
      listen: <T>(channel: string, callback: EventCallback<T>) => {
        const unsubscribe = core.on(channel, (payload: T) => callback({ payload }));
        return Promise.resolve(unsubscribe);
      },
    };
  } catch (wasmError) {
    console.warn('[waddle] failed to initialize wasm runtime, trying browser XMPP', wasmError);

    try {
      return await createBrowserXmppTransport();
    } catch (webXmppError) {
      console.warn('[waddle] falling back to mock web transport', webXmppError);
      const wasmReason =
        wasmError instanceof Error
          ? `waddle-wasm unavailable: ${wasmError.message}`
          : 'waddle-wasm unavailable';
      const xmppReason =
        webXmppError instanceof Error
          ? `browser XMPP unavailable: ${webXmppError.message}`
          : 'browser XMPP unavailable';

      return createMockWebTransport(`${wasmReason}; ${xmppReason}`);
    }
  }
}

let transportPromise: Promise<WaddleTransport> | null = null;
const ready = ref(false);

function getTransport(): Promise<WaddleTransport> {
  if (!transportPromise) {
    transportPromise = (isTauri() ? createTauriTransport() : createWasmTransport()).then(
      (transport) => {
        ready.value = true;
        return transport;
      },
    );
  }
  return transportPromise;
}

export function useWaddle() {
  const transport = getTransport();

  async function sendMessage(to: string, body: string): Promise<ChatMessage> {
    return (await transport).sendMessage(to, body);
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
    sendMessage,
    getRoster,
    addContact,
    getConnectionState,
    setPresence,
    joinRoom,
    leaveRoom,
    getHistory,
    managePlugins,
    getConfig,
    listen,
  };
}
