const SERVICE_WORKER_URL = "/sw.js";

let cachedRegistration: ServiceWorkerRegistration | null = null;
let registrationPromise: Promise<ServiceWorkerRegistration | null> | null = null;

export function supportsServiceWorker() {
  return typeof navigator !== "undefined" && "serviceWorker" in navigator;
}

export async function getServiceWorkerRegistration(): Promise<ServiceWorkerRegistration | null> {
  if (!supportsServiceWorker()) return null;

  try {
    const registration = await navigator.serviceWorker.getRegistration();
    if (registration) {
      cachedRegistration = registration;
      return registration;
    }
  } catch {
    // ignore lookup failures and fall back to the last known registration
  }

  return cachedRegistration;
}

export async function registerServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  if (!supportsServiceWorker()) return null;

  const existingRegistration = await getServiceWorkerRegistration();
  if (existingRegistration) {
    return existingRegistration;
  }

  if (!registrationPromise) {
    registrationPromise = navigator.serviceWorker.register(SERVICE_WORKER_URL, {
      updateViaCache: "none",
    }).then((registration) => {
      cachedRegistration = registration;
      return registration;
    }).catch(() => {
      registrationPromise = null;
      return null;
    });
  }

  return registrationPromise;
}

export async function getOrRegisterServiceWorker(): Promise<ServiceWorkerRegistration | null> {
  return (await getServiceWorkerRegistration()) ?? registerServiceWorker();
}
