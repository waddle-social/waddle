import { ref } from "vue";

export type ThemeMode = "light" | "system" | "dark";

const STORAGE_KEY = "waddle:theme";

function readStored(): ThemeMode {
  if (typeof localStorage === "undefined") return "system";
  const value = localStorage.getItem(STORAGE_KEY);
  return value === "light" || value === "dark" ? value : "system";
}

function applyMode(value: ThemeMode) {
  if (typeof document === "undefined") return;
  const html = document.documentElement;
  if (value === "system") {
    html.removeAttribute("data-theme");
  } else {
    html.setAttribute("data-theme", value);
  }
}

const mode = ref<ThemeMode>(readStored());

if (typeof window !== "undefined") {
  applyMode(mode.value);
}

function setTheme(value: ThemeMode) {
  mode.value = value;
  if (typeof localStorage !== "undefined") {
    if (value === "system") {
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
