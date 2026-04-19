import { afterEach, beforeEach, describe, expect, mock, test } from "bun:test";
import { useAppUpdate } from "../src/composables/useAppUpdate";

class FakeServiceWorker extends EventTarget {
	state: ServiceWorkerState;
	postMessage = mock((_message: unknown) => {});

	constructor(state: ServiceWorkerState) {
		super();
		this.state = state;
	}
}

class FakeServiceWorkerRegistration extends EventTarget {
	active: ServiceWorker | null = null;
	installing: ServiceWorker | null = null;
	waiting: ServiceWorker | null = null;
	update = mock(async () => {});
}

class FakeServiceWorkerContainer extends EventTarget {
	controller: ServiceWorker | null = null;
	registration: ServiceWorkerRegistration | null = null;
	getRegistration = mock(async () => this.registration);
	register = mock(async () => this.registration);
}

const originalWindow = globalThis.window;
const originalDocument = globalThis.document;
const originalNavigator = globalThis.navigator;
const originalDateNow = Date.now;

let serviceWorker: FakeServiceWorkerContainer;
let reload: ReturnType<typeof mock>;
let setIntervalMock: ReturnType<typeof mock>;
let clearIntervalMock: ReturnType<typeof mock>;
let now = 1_000_000;
let nextIntervalId = 0;
const intervalCallbacks = new Map<number, () => void>();

async function flushPromises() {
	await Promise.resolve();
	await Promise.resolve();
	await new Promise((resolve) => setTimeout(resolve, 0));
}

function advanceTime(ms: number) {
	now += ms;
}

function runScheduledUpdateCheck() {
	const callback = intervalCallbacks.values().next().value;
	callback?.();
}

beforeEach(() => {
	serviceWorker = new FakeServiceWorkerContainer();
	serviceWorker.controller = new FakeServiceWorker(
		"activated",
	) as unknown as ServiceWorker;
	reload = mock(() => {});
	now = 1_000_000;
	nextIntervalId = 0;
	intervalCallbacks.clear();
	setIntervalMock = mock((callback: TimerHandler) => {
		const id = ++nextIntervalId;
		intervalCallbacks.set(id, callback as () => void);
		return id;
	});
	clearIntervalMock = mock((id: number) => {
		intervalCallbacks.delete(id);
	});
	Date.now = () => now;

	const windowMock = Object.assign(new EventTarget(), {
		setInterval: setIntervalMock,
		clearInterval: clearIntervalMock,
		location: {
			reload,
		},
	});

	const documentMock = Object.assign(new EventTarget(), {
		visibilityState: "visible" as DocumentVisibilityState,
	});

	Object.defineProperty(globalThis, "window", {
		configurable: true,
		writable: true,
		value: windowMock,
	});
	Object.defineProperty(globalThis, "document", {
		configurable: true,
		writable: true,
		value: documentMock,
	});
	Object.defineProperty(globalThis, "navigator", {
		configurable: true,
		writable: true,
		value: {
			serviceWorker,
		},
	});
});

afterEach(() => {
	useAppUpdate().stop();
	Date.now = originalDateNow;

	if (originalWindow === undefined) {
		Reflect.deleteProperty(globalThis, "window");
	} else {
		Object.defineProperty(globalThis, "window", {
			configurable: true,
			writable: true,
			value: originalWindow,
		});
	}

	if (originalDocument === undefined) {
		Reflect.deleteProperty(globalThis, "document");
	} else {
		Object.defineProperty(globalThis, "document", {
			configurable: true,
			writable: true,
			value: originalDocument,
		});
	}

	if (originalNavigator === undefined) {
		Reflect.deleteProperty(globalThis, "navigator");
	} else {
		Object.defineProperty(globalThis, "navigator", {
			configurable: true,
			writable: true,
			value: originalNavigator,
		});
	}
});

describe("useAppUpdate activation flow", () => {
	test("tracks an installing worker until it becomes a waiting refresh", async () => {
		const appUpdate = useAppUpdate();
		const registration = new FakeServiceWorkerRegistration();
		const installingWorker = new FakeServiceWorker("installing");
		serviceWorker.registration =
			registration as unknown as ServiceWorkerRegistration;

		await appUpdate.start();

		registration.installing = installingWorker as unknown as ServiceWorker;
		registration.dispatchEvent(new Event("updatefound"));

		expect(appUpdate.installingState.value).toBe("installing");
		expect(appUpdate.updateAvailable.value).toBe(false);

		registration.installing = null;
		registration.waiting = installingWorker as unknown as ServiceWorker;
		installingWorker.state = "installed";
		installingWorker.dispatchEvent(new Event("statechange"));
		await flushPromises();

		expect(appUpdate.installingState.value).toBe("installed");
		expect(appUpdate.updateAvailable.value).toBe(true);
		expect(appUpdate.canApplyUpdate.value).toBe(true);
	});

	test("checks for updates again on focus and polling, then cleans up listeners", async () => {
		const appUpdate = useAppUpdate();
		const registration = new FakeServiceWorkerRegistration();
		serviceWorker.registration =
			registration as unknown as ServiceWorkerRegistration;

		await appUpdate.start();

		expect(registration.update).toHaveBeenCalledTimes(1);
		expect(appUpdate.lastCheckedAt.value).toBe(now);
		expect(setIntervalMock).toHaveBeenCalledTimes(1);
		expect(intervalCallbacks.size).toBe(1);

		window.dispatchEvent(new Event("focus"));
		await flushPromises();
		expect(registration.update).toHaveBeenCalledTimes(1);

		advanceTime(61_000);
		window.dispatchEvent(new Event("focus"));
		await flushPromises();
		expect(registration.update).toHaveBeenCalledTimes(2);

		advanceTime(61_000);
		runScheduledUpdateCheck();
		await flushPromises();
		expect(registration.update).toHaveBeenCalledTimes(3);

		appUpdate.stop();
		expect(clearIntervalMock).toHaveBeenCalledTimes(1);
		expect(intervalCallbacks.size).toBe(0);
	});

	test("deduplicates repeat apply requests while activation is pending", async () => {
		const appUpdate = useAppUpdate();
		const waitingWorker = new FakeServiceWorker("installed");
		const registration = new FakeServiceWorkerRegistration();
		registration.waiting = waitingWorker as unknown as ServiceWorker;
		serviceWorker.registration =
			registration as unknown as ServiceWorkerRegistration;

		await appUpdate.start();

		expect(appUpdate.updateAvailable.value).toBe(true);
		expect(await appUpdate.applyUpdate()).toBe(true);
		expect(await appUpdate.applyUpdate()).toBe(true);
		expect(waitingWorker.postMessage).toHaveBeenCalledTimes(1);
		expect(waitingWorker.postMessage).toHaveBeenCalledWith({
			type: "SKIP_WAITING",
		});
		expect(appUpdate.canApplyUpdate.value).toBe(false);
		expect(appUpdate.isApplyingUpdate.value).toBe(true);
	});

	test("reloads the current tab after controllerchange and treats repeat clicks as safe", async () => {
		const appUpdate = useAppUpdate();
		const waitingWorker = new FakeServiceWorker("installed");
		const activeWorker = new FakeServiceWorker("activated");
		const registration = new FakeServiceWorkerRegistration();
		registration.waiting = waitingWorker as unknown as ServiceWorker;
		serviceWorker.registration =
			registration as unknown as ServiceWorkerRegistration;

		expect(await appUpdate.applyUpdate()).toBe(true);
		expect(reload).not.toHaveBeenCalled();

		registration.waiting = null;
		registration.active = activeWorker as unknown as ServiceWorker;
		serviceWorker.controller = activeWorker as unknown as ServiceWorker;
		serviceWorker.dispatchEvent(new Event("controllerchange"));

		expect(reload).toHaveBeenCalledTimes(1);
		expect(appUpdate.controllerChanged.value).toBe(true);
		expect(appUpdate.isApplyingUpdate.value).toBe(false);
		expect(await appUpdate.applyUpdate()).toBe(true);
	});

	test("does not reload when another source changes the controller", async () => {
		const appUpdate = useAppUpdate();
		const registration = new FakeServiceWorkerRegistration();
		serviceWorker.registration =
			registration as unknown as ServiceWorkerRegistration;

		await appUpdate.start();
		serviceWorker.dispatchEvent(new Event("controllerchange"));

		expect(reload).not.toHaveBeenCalled();
		expect(appUpdate.controllerChanged.value).toBe(true);
	});

	test("resets applying state when skip-waiting fails", async () => {
		const appUpdate = useAppUpdate();
		const waitingWorker = new FakeServiceWorker("installed");
		waitingWorker.postMessage = mock(() => {
			throw new Error("boom");
		});
		const registration = new FakeServiceWorkerRegistration();
		registration.waiting = waitingWorker as unknown as ServiceWorker;
		serviceWorker.registration =
			registration as unknown as ServiceWorkerRegistration;

		expect(await appUpdate.applyUpdate()).toBe(false);
		expect(waitingWorker.postMessage).toHaveBeenCalledTimes(1);
		expect(appUpdate.isApplyingUpdate.value).toBe(false);
		expect(appUpdate.canApplyUpdate.value).toBe(true);
	});
});
