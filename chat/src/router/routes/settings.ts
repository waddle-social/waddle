type SettingsOrigin = "app" | "direct";

export interface SettingsMatch {
  readonly id: "settings";
  // `origin` is only set via in-app navigation so `closeUserSettings`
  // can decide whether to `history.back()` or push a fresh entry. It
  // doesn't round-trip through the URL — a hard refresh of `/settings`
  // yields an undefined origin.
  readonly origin?: SettingsOrigin;
}

interface SettingsMatchInput {
  origin?: SettingsOrigin;
}

export const settingsRoute = {
  id: "settings" as const,
  match(input?: SettingsMatchInput): SettingsMatch {
    return input?.origin ? { id: "settings", origin: input.origin } : { id: "settings" };
  },
  href(_input?: SettingsMatchInput): string {
    return "/settings";
  },
  tryParse(pathname: string, _searchString: string): SettingsMatch | null {
    return pathname === "/settings" ? { id: "settings" } : null;
  },
};
