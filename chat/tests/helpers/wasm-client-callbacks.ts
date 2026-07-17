import type { WasmClientBinding } from "../../src/lib/xmpp/wasm-client-adapter";

type CallbackMethod =
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

export type WasmClientCallbacks = Pick<WasmClientBinding, CallbackMethod>;

type Callback<Method extends CallbackMethod> = Parameters<WasmClientBinding[Method]>[0];

export function noopWasmClientCallbacks(): WasmClientCallbacks {
  return {
    set_on_call() {},
    set_on_connected() {},
    set_on_disconnected() {},
    set_on_error() {},
    set_on_mds_displayed() {},
    set_on_message() {},
    set_on_message_delivery_acked() {},
    set_on_message_delivery_failed() {},
    set_on_presence() {},
    set_on_pubsub_event() {},
    set_on_session_lifecycle() {},
    set_on_stream_management() {},
  };
}

/** Closed, typed test control for the generated WASM callback registry. */
export class WasmClientCallbackDouble implements WasmClientCallbacks {
  private onCall: Callback<"set_on_call"> | null = null;
  private onConnected: Callback<"set_on_connected"> | null = null;
  private onDisconnected: Callback<"set_on_disconnected"> | null = null;
  private onError: Callback<"set_on_error"> | null = null;
  private onMdsDisplayed: Callback<"set_on_mds_displayed"> | null = null;
  private onMessage: Callback<"set_on_message"> | null = null;
  private onMessageDeliveryAcked: Callback<"set_on_message_delivery_acked"> | null = null;
  private onMessageDeliveryFailed: Callback<"set_on_message_delivery_failed"> | null = null;
  private onPresence: Callback<"set_on_presence"> | null = null;
  private onPubsubEvent: Callback<"set_on_pubsub_event"> | null = null;
  private onSessionLifecycle: Callback<"set_on_session_lifecycle"> | null = null;
  private onStreamManagement: Callback<"set_on_stream_management"> | null = null;

  set_on_call(callback: Callback<"set_on_call">): void {
    this.onCall = callback;
  }

  set_on_connected(
    callback: Callback<"set_on_connected">,
  ): void {
    this.onConnected = callback;
  }

  set_on_disconnected(
    callback: Callback<"set_on_disconnected">,
  ): void {
    this.onDisconnected = callback;
  }

  set_on_error(
    callback: Callback<"set_on_error">,
  ): void {
    this.onError = callback;
  }

  set_on_mds_displayed(
    callback: Callback<"set_on_mds_displayed">,
  ): void {
    this.onMdsDisplayed = callback;
  }

  set_on_message(
    callback: Callback<"set_on_message">,
  ): void {
    this.onMessage = callback;
  }

  set_on_message_delivery_acked(
    callback: Callback<"set_on_message_delivery_acked">,
  ): void {
    this.onMessageDeliveryAcked = callback;
  }

  set_on_message_delivery_failed(
    callback: Callback<"set_on_message_delivery_failed">,
  ): void {
    this.onMessageDeliveryFailed = callback;
  }

  set_on_presence(
    callback: Callback<"set_on_presence">,
  ): void {
    this.onPresence = callback;
  }

  set_on_pubsub_event(
    callback: Callback<"set_on_pubsub_event">,
  ): void {
    this.onPubsubEvent = callback;
  }

  set_on_session_lifecycle(
    callback: Callback<"set_on_session_lifecycle">,
  ): void {
    this.onSessionLifecycle = callback;
  }

  set_on_stream_management(
    callback: Callback<"set_on_stream_management">,
  ): void {
    this.onStreamManagement = callback;
  }

  emitCall(event: Parameters<Callback<"set_on_call">>[0]): void {
    this.onCall?.(event);
  }

  emitConnected(): void {
    this.onConnected?.();
  }

  emitDisconnected(): void {
    this.onDisconnected?.();
  }

  emitError(error: Parameters<Callback<"set_on_error">>[0]): void {
    this.onError?.(error);
  }

  emitMdsDisplayed(entry: Parameters<Callback<"set_on_mds_displayed">>[0]): void {
    this.onMdsDisplayed?.(entry);
  }

  emitMessage(message: Parameters<Callback<"set_on_message">>[0]): void {
    this.onMessage?.(message);
  }

  emitMessageDeliveryAcked(stanzaId: string): void {
    if (typeof stanzaId !== "string") {
      throw new TypeError("message delivery ACK must be a stanza-id string");
    }
    this.onMessageDeliveryAcked?.(stanzaId);
  }

  emitMessageDeliveryFailed(stanzaId: string): void {
    if (typeof stanzaId !== "string") {
      throw new TypeError("message delivery failure must be a stanza-id string");
    }
    this.onMessageDeliveryFailed?.(stanzaId);
  }

  emitPresence(presence: Parameters<Callback<"set_on_presence">>[0]): void {
    this.onPresence?.(presence);
  }

  emitPubsubEvent(event: Parameters<Callback<"set_on_pubsub_event">>[0]): void {
    this.onPubsubEvent?.(event);
  }

  emitSessionLifecycle(event: Parameters<Callback<"set_on_session_lifecycle">>[0]): void {
    if (event !== "fresh" && event !== "resumed") {
      throw new TypeError("session lifecycle must be fresh or resumed");
    }
    this.onSessionLifecycle?.(event);
  }

  emitStreamManagement(event: Parameters<Callback<"set_on_stream_management">>[0]): void {
    this.onStreamManagement?.(event);
  }
}
