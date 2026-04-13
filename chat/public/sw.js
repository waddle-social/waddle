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
      data: { url: data.url ?? "/" },
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
          client.focus();
          client.navigate(url);
          return;
        }
      }
      return clients.openWindow(url);
    }),
  );
});
