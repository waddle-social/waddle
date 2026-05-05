/// <reference path="../.astro/types.d.ts" />
/// <reference path="../worker-configuration.d.ts" />

type TurnstileRenderOptions = {
	sitekey: string;
	action?: string;
	theme?: "auto" | "light" | "dark";
	size?: "normal" | "compact" | "flexible" | "invisible";
	callback?: (token: string) => void;
	"error-callback"?: (errorCode?: string) => void;
	"expired-callback"?: () => void;
};

type TurnstileApi = {
	ready(callback: () => void): void;
	render(target: string | HTMLElement, options: TurnstileRenderOptions): string;
	reset(widgetId: string): void;
	remove(widgetId: string): void;
	execute(widgetId: string): void;
};

interface Window {
	turnstile?: TurnstileApi;
}
