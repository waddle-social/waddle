import { ref } from "vue";

/**
 * Night is Waddle's canonical theme; daylight is opt-in. The choice is
 * `data-theme` on <html>: "daylight", or absent for night (the preset
 * treats a missing attribute as night). The head script in
 * layouts/AppLayout.astro applies the stored value before first paint.
 */
export type ThemeMode = "night" | "daylight";

const STORAGE_KEY = "waddle:theme";

function readStored(): ThemeMode {
  if (typeof localStorage === "undefined") return "night";
  return localStorage.getItem(STORAGE_KEY) === "daylight" ? "daylight" : "night";
}

function applyMode(value: ThemeMode) {
  if (typeof document === "undefined") return;
  const html = document.documentElement;
  if (value === "daylight") {
    html.dataset.theme = "daylight";
  } else {
    delete html.dataset.theme;
  }
}

const mode = ref<ThemeMode>(readStored());

if (typeof window !== "undefined") {
  applyMode(mode.value);
}

function setTheme(value: ThemeMode) {
  mode.value = value;
  if (typeof localStorage !== "undefined") {
    if (value === "daylight") {
      localStorage.setItem(STORAGE_KEY, value);
    } else {
      localStorage.removeItem(STORAGE_KEY);
    }
  }
  applyMode(value);
}

export function useTheme() {
  return { mode, setTheme };
}
