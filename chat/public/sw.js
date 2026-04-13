/** Service worker for Waddle push notifications. */

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data?.json() ?? {};
  } catch {
    const text = event.data?.text() ?? "";
    data = { title: "Waddle", body: text };
  }
  event.waitUntil(
    self.registration.showNotification(data.title ?? "Waddle", {
      body: data.body ?? "",
      tag: data.roomJid,
      icon: "/favicon.svg",
      data: { url: data.url ?? roomJidToPath(data.roomJid) },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data?.url ?? "/";
  event.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((windowClients) => {
      for (const client of windowClients) {
        if (client.url.includes(self.location.origin) && "focus" in client) {
          return client.focus().then(() => client.navigate(url));
        }
      }
      return clients.openWindow(url);
    }),
  );
});

function roomJidToPath(roomJid) {
  if (typeof roomJid !== "string") return "/";
  const localpart = roomJid.split("@")[0] ?? "";
  const parts = localpart.split("_");
  if (parts.length < 2) return "/";
  return `/${encodeURIComponent(parts[0])}/${encodeURIComponent(parts[1])}`;
}
