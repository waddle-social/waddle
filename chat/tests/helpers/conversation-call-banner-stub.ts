import { h } from "vue";

type BannerProbe = (
  props: Record<string, unknown>,
  emit: (event: string, ...args: unknown[]) => void,
) => void;

export default {
  name: "ConversationCallBannerTestStub",
  setup(
    props: Record<string, unknown>,
    {
      attrs,
      emit,
    }: {
      attrs: Record<string, unknown>;
      emit: (event: string, ...args: unknown[]) => void;
    },
  ) {
    const allProps = { ...attrs, ...props };
    (globalThis as { __waddleConversationCallBannerProbe?: BannerProbe })
      .__waddleConversationCallBannerProbe?.({
        ...allProps,
        roomJid: allProps.roomJid ?? allProps["room-jid"],
        channelId: allProps.channelId ?? allProps["channel-id"],
        channelName: allProps.channelName ?? allProps["channel-name"],
        dmPeerJid: allProps.dmPeerJid ?? allProps["dm-peer-jid"],
        dmPeerName: allProps.dmPeerName ?? allProps["dm-peer-name"],
        selfFullJid: allProps.selfFullJid ?? allProps["self-full-jid"],
      }, emit);
    return () => h("span", { "data-conversation-call-banner-stub": "true" });
  },
};
