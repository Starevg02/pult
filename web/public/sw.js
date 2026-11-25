// Service worker портала: только уведомления на устройство (Web Push).
// Ответы сервера не кешируются: данные портала не должны оставаться на устройстве после выхода.
self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("push", (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = {};
  }
  const title = typeof data.title === "string" ? data.title : "Pult";
  event.waitUntil(
    self.registration.showNotification(title, {
      body: typeof data.body === "string" ? data.body : "",
      icon: "/icon.svg",
      badge: "/icon.svg",
      tag: typeof data.kind === "string" ? data.kind : "pult",
      data: { url: localPath(data.url) },
    }),
  );
});

// Только путь этого портала: «//host/…» и «/\host/…» браузер понимает как другой сайт.
function localPath(raw) {
  if (typeof raw !== "string" || !raw.startsWith("/")) return "/notifications";
  try {
    const u = new URL(raw, self.location.origin);
    return u.origin === self.location.origin ? u.pathname + u.search + u.hash : "/notifications";
  } catch {
    return "/notifications";
  }
}

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = new URL(localPath(event.notification.data?.url), self.location.origin).href;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((list) => {
      for (const c of list) {
        if (new URL(c.url).origin === self.location.origin && "focus" in c) {
          c.navigate(url);
          return c.focus();
        }
      }
      return self.clients.openWindow(url);
    }),
  );
});
