/* Independent Flutter Web Push. This file runs as a classic window script and
 * as the per-account service worker; it never uses the chat/PWA registration. */
const waddlePushBase = new URL('.', globalThis.location.href);
const waddlePushDatabase = `waddle.flutter.push:${waddlePushBase.pathname}`;
const waddlePushClasses = new Set(['dm', 'dm_mention', 'personal_mention', 'channel_mention', 'active_channel_mention', 'notify_all']);

function parseWaddlePush(value) {
  if (!value || value.v !== 1 || !waddlePushClasses.has(value.class)) return null;
  const jid = value.conversation;
  if (typeof jid !== 'string' || !jid.length || jid.length > 3071 || /[/\s\x00-\x1f\x7f]/u.test(jid) || (jid.match(/@/g) || []).length > 1 || jid.startsWith('@') || jid.endsWith('@')) return null;
  if (typeof value.item !== 'string' || !value.item.trim() || value.item.length > 4096) return null;
  if (value.thread != null && (typeof value.thread !== 'string' || value.thread.length > 4096)) return null;
  return {v: 1, class: value.class, conversation: jid, item: value.item, ...(value.thread == null ? {} : {thread: value.thread}), ...(Number.isSafeInteger(value.unread) ? {unread: value.unread} : {})};
}

function ownsWaddleRegistration(active, instance) {
  return !!active && active.enabled === true && typeof active.owner === 'string' && active.owner.length > 0 && active.instance === instance;
}

async function waddlePushStore(change) {
  const database = await new Promise((resolve, reject) => {
    const request = indexedDB.open(waddlePushDatabase, 1);
    request.onupgradeneeded = () => request.result.createObjectStore('state');
    request.onsuccess = () => resolve(request.result);
    request.onerror = () => reject(new Error('Push storage unavailable'));
  });
  try {
    return await new Promise((resolve, reject) => {
      const transaction = database.transaction('state', change ? 'readwrite' : 'readonly');
      const store = transaction.objectStore('state');
      let result;
      const request = store.get('active');
      request.onsuccess = () => {
        result = request.result || {};
        if (change) { result = change(result); store.put(result, 'active'); }
      };
      transaction.oncomplete = () => resolve(result);
      transaction.onabort = transaction.onerror = () => reject(new Error('Push storage unavailable'));
    });
  } finally { database.close(); }
}

if (typeof window !== 'undefined') {
  let waddleEmit = null;
  let waddleAccountRevision = 0;
  const registrations = async () => (await navigator.serviceWorker.getRegistrations()).filter(registration => {
    const script = registration.active?.scriptURL || registration.waiting?.scriptURL || registration.installing?.scriptURL;
    return script && new URL(script).pathname === new URL('waddle_push.js', waddlePushBase).pathname;
  });
  async function activateWaddleRegistration(registration) {
    const worker = registration.active || registration.installing || registration.waiting;
    if (!worker) throw new Error('Push worker unavailable');
    if (worker.state === 'activated') return;
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('Push worker activation timed out')), 15000);
      worker.addEventListener('statechange', () => {
        if (worker.state === 'activated') { clearTimeout(timer); resolve(); }
        if (worker.state === 'redundant') { clearTimeout(timer); reject(new Error('Push worker retired')); }
      });
    });
  }
  function vapidBytes(value) {
    const padded = value.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - value.length % 4) % 4);
    return Uint8Array.from(atob(padded), char => char.charCodeAt(0));
  }
  globalThis.waddlePush = {
    async initialize(emit) {
      waddleEmit = emit;
      const supported = isSecureContext && 'serviceWorker' in navigator && 'PushManager' in window && 'Notification' in window;
      if (!supported) return false;
      navigator.serviceWorker.addEventListener('message', event => {
        if (!event.source?.scriptURL || new URL(event.source.scriptURL).pathname !== new URL('waddle_push.js', waddlePushBase).pathname) return;
        if (event.data?.source === 'waddle-flutter-push') waddleEmit?.(event.data.event);
      });
      const active = await waddlePushStore();
      if (active.pendingAction) {
        waddleEmit?.(active.pendingAction);
        await waddlePushStore(current => ({...current, pendingAction: null}));
      }
      return true;
    },
    async invoke(method, args = {}) {
      if (method === 'permission') return await Notification.requestPermission() === 'granted';
      if (method === 'permissionStatus') return Notification.permission === 'granted';
      if (method === 'account') {
        const revision = ++waddleAccountRevision;
        await waddlePushStore(current => revision !== waddleAccountRevision ? current : ({
          owner: args.owner, instance: args.instance, enabled: args.enabled === true,
          sound: args.sound !== false,
          pageUrl: `${location.origin}${location.pathname}`,
          seen: current.instance === args.instance ? current.seen || [] : [],
          pendingAction: current.instance === args.instance ? current.pendingAction : null,
        }));
      } else if (method === 'subscribe') {
        if (Notification.permission !== 'granted') return;
        const url = new URL('waddle_push.js', waddlePushBase);
        url.searchParams.set('instance', args.instance);
        const registration = await navigator.serviceWorker.register(url.href, {scope: new URL(`push/${encodeURIComponent(args.instance)}/`, waddlePushBase).href});
        await activateWaddleRegistration(registration);
        let subscription = await registration.pushManager.getSubscription();
        const key = vapidBytes(args.vapid);
        if (subscription && JSON.stringify(Array.from(new Uint8Array(subscription.options.applicationServerKey || []))) !== JSON.stringify(Array.from(key))) {
          await subscription.unsubscribe(); subscription = null;
        }
        subscription ||= await registration.pushManager.subscribe({userVisibleOnly: true, applicationServerKey: key});
        const json = subscription.toJSON();
        if (json.endpoint && json.keys?.p256dh && json.keys?.auth) waddleEmit?.({type: 'endpoint', instance: args.instance, endpoint: {endpoint: json.endpoint, p256dh: json.keys.p256dh, auth: json.keys.auth}});
      } else if (method === 'unsubscribe') {
        await waddlePushStore(current => current.instance === args.instance ? {...current, enabled: false} : current);
        for (const registration of await registrations()) {
          const script = registration.active?.scriptURL || registration.waiting?.scriptURL || registration.installing?.scriptURL;
          if (!script) continue;
          if (new URL(script).searchParams.get('instance') !== args.instance) continue;
          await (await registration.pushManager.getSubscription())?.unsubscribe();
          for (const notice of await registration.getNotifications()) notice.close();
          await registration.unregister();
        }
      } else if (method === 'clear') {
        for (const registration of await registrations()) for (const notice of await registration.getNotifications()) if (notice.data?.kind !== 'call') notice.close();
      } else if (method === 'cancel') {
        for (const registration of await registrations()) for (const notice of await registration.getNotifications({tag: `waddle-flutter-${args.id}`})) notice.close();
      } else if (method === 'show') {
        const active = await waddlePushStore();
        if (!ownsWaddleRegistration(active, args.payload?.instance) || active.owner !== args.payload?.owner) return;
        const registration = (await registrations()).find(value => new URL(value.active?.scriptURL || location.href).searchParams.get('instance') === active.instance);
        if (registration) await registration.showNotification(args.title, {body: args.body, tag: `waddle-flutter-${args.id}`, data: args.payload, silent: !args.sound, requireInteraction: args.payload?.kind === 'call', actions: args.payload?.kind === 'call' ? [{action:'call_answer',title:'Answer'}, {action:'call_decline',title:'Decline'}] : []});
      } else if (method === 'detach') {
        waddleEmit = null;
      }
      return null;
    },
  };
} else {
  const instance = new URL(self.location.href).searchParams.get('instance');
  self.addEventListener('install', event => event.waitUntil(self.skipWaiting()));
  self.addEventListener('push', event => event.waitUntil((async () => {
    let envelope;
    try { envelope = parseWaddlePush(event.data?.json()); } catch { return; }
    if (!envelope) return;
    const active = await waddlePushStore();
    if (!ownsWaddleRegistration(active, instance) || (active.seen || []).includes(envelope.item)) return;
    const windows = await self.clients.matchAll({type: 'window', includeUncontrolled: true});
    if (windows.some(client => client.visibilityState === 'visible' && client.url.split('#')[0].split('?')[0] === active.pageUrl)) return;
    const payload = {...envelope, owner: active.owner, instance};
    const unread = Math.max(1, Math.min(2147483647, envelope.unread || 1));
    const mention = envelope.class !== 'dm' && envelope.class !== 'notify_all';
    const before = await waddlePushStore();
    if (!ownsWaddleRegistration(before, instance) || before.owner !== active.owner) return;
    await self.registration.showNotification(envelope.conversation, {body: mention ? 'Mentioned you' : `${unread} new messages`, tag: `waddle-flutter-${envelope.conversation}`, data: payload, silent: active.sound === false});
    await waddlePushStore(current => ownsWaddleRegistration(current, instance) ? {...current, seen: [...(current.seen || []).filter(item => item !== envelope.item), envelope.item].slice(-256)} : current);
    const after = await waddlePushStore();
    if (!ownsWaddleRegistration(after, instance) || after.owner !== active.owner) for (const notice of await self.registration.getNotifications()) notice.close();
  })()));
  self.addEventListener('notificationclick', event => {
    event.notification.close();
    event.waitUntil((async () => {
      const active = await waddlePushStore();
      const payload = event.notification.data;
      const call = payload?.kind === 'call' && typeof payload.sid === 'string' && payload.sid.length > 0;
      if (!ownsWaddleRegistration(active, instance) || payload?.owner !== active.owner || payload?.instance !== instance || (!call && !parseWaddlePush(payload))) return;
      const action = {type: 'action', action: call && event.action === 'call_answer' ? 'answer' : call && event.action === 'call_decline' ? 'decline' : 'open', payload};
      const windows = await self.clients.matchAll({type: 'window', includeUncontrolled: true});
      const client = windows.find(value => value.url.split('#')[0].split('?')[0] === active.pageUrl);
      if (client) { client.postMessage({source: 'waddle-flutter-push', event: action}); await client.focus(); }
      else {
        await waddlePushStore(current => ownsWaddleRegistration(current, instance) ? {...current, pendingAction: action} : current);
        const target = new URL(active.pageUrl);
        if (target.origin === self.location.origin) await self.clients.openWindow(target.href);
      }
    })());
  });
}
