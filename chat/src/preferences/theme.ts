import { ref } from "vue";

/**
 * Night is Waddle's canonical theme; daylight is opt-in; system follows
 * `prefers-color-scheme` live. The stored value is "daylight" or
 * "system"; nothing stored means night. The resolved theme is
 * `data-theme` on <html>: "daylight", or absent for night (the preset
 * treats a missing attribute as night). The head script in
 * layouts/AppLayout.astro applies the stored value before first paint.
 */
export type ThemeMode = "system" | "night" | "daylight";

const STORAGE_KEY = "waddle:theme";

function readStored(): ThemeMode {
  if (typeof localStorage === "undefined") return "night";
  const stored = localStorage.getItem(STORAGE_KEY);
  return stored === "daylight" || stored === "system" ? stored : "night";
}

const prefersLight = typeof matchMedia === "undefined" ? null : matchMedia("(prefers-color-scheme: light)");

function applyMode(value: ThemeMode) {
  if (typeof document === "undefined") return;
  const html = document.documentElement;
  if (value === "daylight" || (value === "system" && prefersLight?.matches)) {
    html.dataset.theme = "daylight";
  } else {
    delete html.dataset.theme;
  }
}

const mode = ref<ThemeMode>(readStored());

if (typeof window !== "undefined") {
  applyMode(mode.value);
  prefersLight?.addEventListener("change", () => applyMode(mode.value));
}

function setTheme(value: ThemeMode) {
  mode.value = value;
  if (typeof localStorage !== "undefined") {
    if (value === "night") {
      localStorage.removeItem(STORAGE_KEY);
    } else {
      localStorage.setItem(STORAGE_KEY, value);
    }
  }
  applyMode(value);
}

export function useTheme() {
  return { mode, setTheme };
}
