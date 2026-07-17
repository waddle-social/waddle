import type { AppState } from "@/lib/chat-ui";

export type ProviderFixtureSnapshot = {
  appState: AppState;
  sessionId: string | null;
  appError: string;
  activeServerUrl: string;
  providerIds: string[];
  activeClientId: string | null;
  disposeCalls: Record<string, number>;
  events: string[];
};

export type ProviderBrowserFixture = {
  snapshot(): ProviderFixtureSnapshot;
  login(): Promise<void>;
  logout(): Promise<void>;
  bootstrap(): Promise<void>;
  unmount(): void;
};

declare global {
  interface Window {
    __waddleProviderFixture: ProviderBrowserFixture;
  }
}
