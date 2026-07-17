import type { CallEvent } from "@/lib/calls/types";
import type {
  WaddleClient as GeneratedWaddleClient,
  WaddleConfig as GeneratedWaddleConfig,
  WaddleResumeStateSnapshot,
} from "@waddle/xmpp-client-wasm";
import type {
  WasmControlErrorPayload,
  WasmMdsDisplayedEntry,
  WasmMessage,
  WasmPresence,
  WasmPubsubEvent,
  WasmSendMessageOutcome,
  WasmSendOptions,
  WasmStreamManagementTelemetry,
} from "./wasm-types";

type GeneratedCallbackMethod =
  | "set_on_call"
  | "set_on_connected"
  | "set_on_disconnected"
  | "set_on_error"
  | "set_on_mds_displayed"
  | "set_on_message"
  | "set_on_message_delivery_acked"
  | "set_on_message_delivery_failed"
  | "set_on_presence"
  | "set_on_pubsub_event"
  | "set_on_session_lifecycle"
  | "set_on_stream_management";

type GeneratedTypedMethod =
  | GeneratedCallbackMethod
  | "get_resume_state"
  | "send_chat_message"
  | "send_groupchat_message";

export type WasmClientBinding = Omit<
  GeneratedWaddleClient,
  GeneratedTypedMethod
> & {
  get_resume_state(): WaddleResumeStateSnapshot | null;
  set_on_call(callback: (event: CallEvent) => void): void;
  set_on_connected(callback: () => void): void;
  set_on_disconnected(callback: () => void): void;
  set_on_error(callback: (error: WasmControlErrorPayload) => void): void;
  set_on_mds_displayed(
    callback: (entry: WasmMdsDisplayedEntry) => void,
  ): void;
  set_on_message(callback: (message: WasmMessage) => void): void;
  set_on_message_delivery_acked(callback: (stanzaId: string) => void): void;
  set_on_message_delivery_failed(callback: (stanzaId: string) => void): void;
  set_on_presence(callback: (presence: WasmPresence) => void): void;
  set_on_pubsub_event(callback: (event: WasmPubsubEvent) => void): void;
  set_on_session_lifecycle(
    callback: (event: "fresh" | "resumed") => void,
  ): void;
  set_on_stream_management(
    callback: (event: WasmStreamManagementTelemetry) => void,
  ): void;
  send_chat_message(
    peerJid: string,
    body: string,
    options: WasmSendOptions,
  ): Promise<WasmSendMessageOutcome>;
  send_groupchat_message(
    roomJid: string,
    body: string,
    options: WasmSendOptions,
  ): Promise<WasmSendMessageOutcome>;
};

export type WasmConfigBinding = Omit<GeneratedWaddleConfig, "with_resume_state"> & {
  with_resume_state(state: WaddleResumeStateSnapshot): void;
};

const REQUIRED_GENERATED_METHODS = [
  "connect",
  "disconnect",
  "dispose",
  "get_resume_state",
  "request_stream_management_ack",
  "send_chat_message",
  "send_groupchat_message",
  "set_on_call",
  "set_on_connected",
  "set_on_disconnected",
  "set_on_error",
  "set_on_mds_displayed",
  "set_on_message",
  "set_on_message_delivery_acked",
  "set_on_message_delivery_failed",
  "set_on_presence",
  "set_on_pubsub_event",
  "set_on_session_lifecycle",
  "set_on_stream_management",
] as const satisfies ReadonlyArray<keyof WasmClientBinding>;

/**
 * Validate the generated binding once, at construction, before it can become
 * the active transport. Downstream code receives one strict surface rather
 * than probing optional callbacks or falling back to legacy emitters.
 */
export function validatedWasmClientBinding(
  client: GeneratedWaddleClient,
): WasmClientBinding {
  const candidate = client as unknown as Record<string, unknown>;
  for (const method of REQUIRED_GENERATED_METHODS) {
    if (typeof candidate[method] !== "function") {
      throw new TypeError(
        `Generated XMPP WASM binding is missing required method ${method}`,
      );
    }
  }
  return client as unknown as WasmClientBinding;
}

/** Validate the one generated resume-state installation entrypoint. */
export function validatedWasmConfigBinding(
  config: GeneratedWaddleConfig,
): WasmConfigBinding {
  const candidate = config as unknown as Record<string, unknown>;
  if (typeof candidate.with_resume_state !== "function") {
    throw new TypeError(
      "Generated XMPP WASM config is missing required method with_resume_state",
    );
  }
  return config as WasmConfigBinding;
}
