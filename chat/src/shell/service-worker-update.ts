import { computed, ref } from "vue";
import {
	getOrRegisterServiceWorker,
	getServiceWorkerRegistration,
	supportsServiceWorker,
} from "@/lib/service-worker-registration";

const UPDATE_CHECK_INTERVAL_MS = 15 * 60_000;
const UPDATE_CHECK_THROTTLE_MS = 60_000;

const registration = ref<ServiceWorkerRegistration | null>(null);
const installingState = ref<ServiceWorkerState | null>(null);
const hasWaitingWorker = ref(false);
const isChecking = ref(false);
const isApplyingUpdate = ref(false);
const controllerChanged = ref(false);
const lastCheckedAt = ref<number | null>(null);

const updateAvailable = computed(() => hasWaitingWorker.value);
const canApplyUpdate = computed(
	() => hasWaitingWorker.value && !isApplyingUpdate.value,
);

let started = false;
let updateCheckTimer: number | null = null;
let observedRegistration: ServiceWorkerRegistration | null = null;
let observedInstallingWorker: ServiceWorker | null = null;
let pendingCheck: Promise<ServiceWorkerRegistration | null> | null = null;
let lastCheckStartedAt = 0;
let didReloadForControllerChange = false;
let lifecycleToken = 0;

const handleFocus = () => {
	void checkForUpdate();
};

const handleVisibilityChange = () => {
	if (document.visibilityState === "visible") {
		void checkForUpdate();
	}
};

const handleControllerChange = () => {
	controllerChanged.value = true;
	hasWaitingWorker.value = false;
	installingState.value = null;

	if (isApplyingUpdate.value && !didReloadForControllerChange) {
		didReloadForControllerChange = true;
		isApplyingUpdate.value = false;
		window.location.reload();
		return;
	}

	isApplyingUpdate.value = false;
	void refreshObservedRegistration();
};

const handleUpdateFound = () => {
	observeInstallingWorker(observedRegistration?.installing ?? null);
	syncRegistrationState(observedRegistration);
};

const handleInstallingStateChange = () => {
	if (!observedInstallingWorker) {
		installingState.value = null;
		return;
	}

	installingState.value = observedInstallingWorker.state;

	if (observedInstallingWorker.state === "installed") {
		if (navigator.serviceWorker.controller) {
			hasWaitingWorker.value = true;
		}
		void refreshObservedRegistration();
		return;
	}

	if (
		observedInstallingWorker.state === "activating" ||
		observedInstallingWorker.state === "activated" ||
		observedInstallingWorker.state === "redundant"
	) {
		if (observedInstallingWorker.state === "redundant") {
			isApplyingUpdate.value = false;
		}
		void refreshObservedRegistration();
	}
};

function removeRegistrationObserver() {
	observedRegistration?.removeEventListener("updatefound", handleUpdateFound);
	observedRegistration = null;
}

function removeInstallingWorkerObserver() {
	observedInstallingWorker?.removeEventListener(
		"statechange",
		handleInstallingStateChange,
	);
	observedInstallingWorker = null;
}

function observeInstallingWorker(worker: ServiceWorker | null) {
	if (observedInstallingWorker === worker) {
		if (!worker) {
			installingState.value = null;
		}
		return;
	}

	removeInstallingWorkerObserver();
	observedInstallingWorker = worker;

	if (!worker) {
		installingState.value = null;
		return;
	}

	installingState.value = worker.state;
	worker.addEventListener("statechange", handleInstallingStateChange);
}

function syncRegistrationState(
	nextRegistration: ServiceWorkerRegistration | null,
) {
	registration.value = nextRegistration;
	observeInstallingWorker(
		nextRegistration?.installing ?? nextRegistration?.waiting ?? null,
	);
	hasWaitingWorker.value = Boolean(
		navigator.serviceWorker.controller && nextRegistration?.waiting,
	);
}

function observeRegistration(
	nextRegistration: ServiceWorkerRegistration | null,
) {
	if (observedRegistration === nextRegistration) {
		syncRegistrationState(nextRegistration);
		return;
	}

	removeRegistrationObserver();
	observedRegistration = nextRegistration;

	if (nextRegistration) {
		nextRegistration.addEventListener("updatefound", handleUpdateFound);
	}

	syncRegistrationState(nextRegistration);
}

async function refreshObservedRegistration() {
	const currentRegistration = await getServiceWorkerRegistration();
	observeRegistration(currentRegistration);
	return registration.value;
}

function clearUpdateCheckTimer() {
	if (updateCheckTimer !== null) {
		window.clearInterval(updateCheckTimer);
		updateCheckTimer = null;
	}
}

function resetState() {
	removeInstallingWorkerObserver();
	removeRegistrationObserver();
	registration.value = null;
	installingState.value = null;
	hasWaitingWorker.value = false;
	isChecking.value = false;
	isApplyingUpdate.value = false;
	controllerChanged.value = false;
	lastCheckedAt.value = null;
	pendingCheck = null;
	lastCheckStartedAt = 0;
	didReloadForControllerChange = false;
}

async function checkForUpdate(
	force = false,
): Promise<ServiceWorkerRegistration | null> {
	if (!supportsServiceWorker()) return null;

	if (pendingCheck) {
		return pendingCheck;
	}

	const now = Date.now();
	if (!force && now - lastCheckStartedAt < UPDATE_CHECK_THROTTLE_MS) {
		return registration.value;
	}

	lastCheckStartedAt = now;
	const currentLifecycleToken = lifecycleToken;

	pendingCheck = (async () => {
		isChecking.value = true;

		try {
			const currentRegistration = await getOrRegisterServiceWorker();
			if (currentLifecycleToken !== lifecycleToken) {
				return null;
			}

			observeRegistration(currentRegistration);

			if (currentRegistration) {
				try {
					await currentRegistration.update();
				} catch {
					// ignore update check failures
				}
			}

			if (currentLifecycleToken !== lifecycleToken) {
				return null;
			}

			await refreshObservedRegistration();
			if (currentLifecycleToken !== lifecycleToken) {
				return null;
			}

			lastCheckedAt.value = Date.now();
			return registration.value;
		} finally {
			isChecking.value = false;
			pendingCheck = null;
		}
	})();

	return pendingCheck;
}

async function applyUpdate() {
	if (!supportsServiceWorker()) return false;
	if (isApplyingUpdate.value) return true;

	ensureStarted();

	const currentRegistration = registration.value?.waiting
		? registration.value
		: await checkForUpdate(true);
	const waitingWorker = currentRegistration?.waiting ?? null;
	if (!waitingWorker) {
		await refreshObservedRegistration();
		return controllerChanged.value && !registration.value?.waiting;
	}

	controllerChanged.value = false;
	isApplyingUpdate.value = true;
	didReloadForControllerChange = false;

	try {
		waitingWorker.postMessage({ type: "SKIP_WAITING" });
		return true;
	} catch {
		isApplyingUpdate.value = false;
		return false;
	}
}

function ensureStarted() {
	if (started || !supportsServiceWorker()) {
		return;
	}

	started = true;
	lifecycleToken += 1;
	window.addEventListener("focus", handleFocus);
	document.addEventListener("visibilitychange", handleVisibilityChange);
	navigator.serviceWorker.addEventListener(
		"controllerchange",
		handleControllerChange,
	);
	updateCheckTimer = window.setInterval(() => {
		void checkForUpdate();
	}, UPDATE_CHECK_INTERVAL_MS);
}

async function start() {
	if (!supportsServiceWorker()) return null;

	ensureStarted();

	return checkForUpdate(true);
}

function stop() {
	if (!started || !supportsServiceWorker()) {
		resetState();
		started = false;
		return;
	}

	started = false;
	lifecycleToken += 1;
	clearUpdateCheckTimer();
	window.removeEventListener("focus", handleFocus);
	document.removeEventListener("visibilitychange", handleVisibilityChange);
	navigator.serviceWorker.removeEventListener(
		"controllerchange",
		handleControllerChange,
	);
	resetState();
}

export function useServiceWorkerUpdate() {
	return {
		registration,
		installingState,
		hasWaitingWorker,
		updateAvailable,
		canApplyUpdate,
		isChecking,
		isApplyingUpdate,
		controllerChanged,
		lastCheckedAt,
		start,
		stop,
		checkForUpdate,
		applyUpdate,
	};
}
