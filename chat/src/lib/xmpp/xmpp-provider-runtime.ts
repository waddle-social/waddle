import type { InjectionKey } from "vue";
import { useSessionAuth } from "@/auth/session";
import { connectionStore } from "@/lib/connection-store";
import type { WaddleSession } from "@/lib/server-auth";
import {
  BrowserXmppClient,
  type XmppStatusSnapshot,
} from "@/lib/xmpp-client";
import type { PagehideXmppClient } from "@/lib/xmpp/pagehide-lifecycle";
import type { StatusAwareProviderClient } from "@/lib/xmpp/provider-client-coordinator";
import { installInstrumentation } from "@/lib/xmpp/xmpp-instrumentation";
import { $xmppStatus } from "@/stores/xmpp-status";

type XmppProviderAuth = ReturnType<typeof useSessionAuth>;

export type XmppProviderClient = PagehideXmppClient
  & StatusAwareProviderClient<XmppStatusSnapshot>;

export type XmppProviderRuntime = {
  auth: XmppProviderAuth;
  getClient(): XmppProviderClient | null;
  setClient(client: XmppProviderClient | null): void;
  createClient(session: WaddleSession): XmppProviderClient;
  instrumentClient(client: XmppProviderClient): void;
  handleStatus(status: XmppStatusSnapshot): void;
};

export const XMPP_PROVIDER_RUNTIME: InjectionKey<XmppProviderRuntime> = Symbol(
  "waddle.xmpp-provider-runtime",
);

function requireBrowserClient(
  client: XmppProviderClient,
): BrowserXmppClient {
  if (!(client instanceof BrowserXmppClient)) {
    throw new TypeError("Default XMPP provider runtime received a foreign client");
  }
  return client;
}

export function createBrowserXmppProviderRuntime(
  serverBaseUrl: string,
): XmppProviderRuntime {
  return {
    auth: useSessionAuth(serverBaseUrl),
    getClient: () => connectionStore.client,
    setClient: (client) => {
      connectionStore.client = client === null
        ? null
        : requireBrowserClient(client);
    },
    createClient: (session) => new BrowserXmppClient(session),
    instrumentClient: (client) => {
      installInstrumentation(requireBrowserClient(client));
    },
    handleStatus: (status) => {
      $xmppStatus.set(status);
    },
  };
}
