const ADMIN_PANELS = [
  "users",
  "spaces",
  "channels",
  "audit",
  "push-health",
  "settings",
] as const;

export type AdminPanel = (typeof ADMIN_PANELS)[number];

const DEFAULT_ADMIN_PANEL: AdminPanel = "users";

function decodeAdminPanel(raw: string | undefined): AdminPanel {
  if (!raw) return DEFAULT_ADMIN_PANEL;
  return (ADMIN_PANELS as readonly string[]).includes(raw)
    ? (raw as AdminPanel)
    : DEFAULT_ADMIN_PANEL;
}

export interface AdminMatch {
  readonly id: "admin";
  readonly params: { panel: AdminPanel };
}

interface AdminMatchInput {
  params?: { panel?: AdminPanel };
}

export const adminRoute = {
  id: "admin" as const,
  match(input?: AdminMatchInput): AdminMatch {
    return {
      id: "admin",
      params: { panel: input?.params?.panel ?? DEFAULT_ADMIN_PANEL },
    };
  },
  href(input?: AdminMatchInput): string {
    return `/admin/${input?.params?.panel ?? DEFAULT_ADMIN_PANEL}`;
  },
  tryParse(pathname: string, _searchString: string): AdminMatch | null {
    const segments = pathname.split("/").filter(Boolean);
    if (segments[0] !== "admin") return null;
    // Match the prior parser's tolerance: extra segments after the
    // panel are ignored (e.g. `/admin/users/anything` → admin/users)
    // rather than falling through to the home fallback.
    return {
      id: "admin",
      params: { panel: decodeAdminPanel(segments[1]) },
    };
  },
};
