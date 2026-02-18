declare module 'waddle-wasm' {
  export interface WaddleCore {
    send_message(to: string, body: string): Promise<import('./composables/useWaddle').ChatMessage>;
    get_roster(): Promise<import('./composables/useWaddle').RosterItem[]>;
    set_presence(show: string, status?: string): Promise<void>;
    join_room(roomJid: string, nick: string): Promise<void>;
    leave_room(roomJid: string): Promise<void>;
    get_history(
      jid: string,
      limit: number,
      before?: string,
    ): Promise<import('./composables/useWaddle').ChatMessage[]>;
    manage_plugins(
      action: import('./composables/useWaddle').PluginAction,
    ): Promise<import('./composables/useWaddle').PluginInfo>;
    get_config(): Promise<import('./composables/useWaddle').UiConfig>;
    on<T>(channel: string, callback: (payload: T) => void): () => void;
  }

  export const WaddleCore: {
    init(): Promise<WaddleCore>;
  };
}
