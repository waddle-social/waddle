import { ref } from "vue";

/**
 * Night is Waddle's canonical theme; daylight is opt-in. A stored
 * "system" value from the previous three-way switch is read as night.
 */
export type ThemeMode = "light" | "dark";

const STORAGE_KEY = "waddle:theme";

function readStored(): ThemeMode {
  if (typeof localStorage === "undefined") return "dark";
  return localStorage.getItem(STORAGE_KEY) === "light" ? "light" : "dark";
}

function applyMode(value: ThemeMode) {
  if (typeof document === "undefined") return;
  const html = document.documentElement;
  if (value === "light") {
    html.setAttribute("data-theme", "light");
  } else {
    html.removeAttribute("data-theme");
  }
}

const mode = ref<ThemeMode>(readStored());

if (typeof window !== "undefined") {
  applyMode(mode.value);
}

function setTheme(value: ThemeMode) {
  mode.value = value;
  if (typeof localStorage !== "undefined") {
    if (value === "light") {
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
