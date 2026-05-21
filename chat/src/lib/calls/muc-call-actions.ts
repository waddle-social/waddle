import { beginMucCall, reportCallError, type RawIqSender } from "./call-store";
import type { CallMedia } from "./types";

type MucCallStartActionOptions = {
  roomJid: string;
  media: CallMedia;
  isBusy: () => boolean;
  setStarting: (starting: boolean) => void;
  getSender: () => RawIqSender | null;
  getSelfNick: () => string | undefined;
  getExpectedMixerJid?: () => string | null | undefined;
};

/**
 * Shared action behind the group-call buttons. Kept outside the SFC
 * so tests can exercise the exact click handler semantics without
 * introducing a Vue test-utils dependency.
 */
export async function startMucCallAction({
  roomJid,
  media,
  isBusy,
  setStarting,
  getSender,
  getSelfNick,
  getExpectedMixerJid,
}: MucCallStartActionOptions): Promise<boolean> {
  if (isBusy()) return false;
  const sender = getSender();
  if (!sender) return false;
  setStarting(true);
  try {
    await beginMucCall(
      sender,
      roomJid,
      media,
      getSelfNick(),
      getExpectedMixerJid?.(),
    );
    return true;
  } catch (err) {
    reportCallError(err);
    return false;
  } finally {
    setStarting(false);
  }
}
