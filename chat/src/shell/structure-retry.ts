import type { XmppStatusSnapshot } from "@/lib/xmpp/types";

export interface MissingStructureRetryInput {
  appReady: boolean;
  hasClient: boolean;
  initialLoadFinished: boolean;
  inFlight: boolean;
  isLoadingStructure: boolean;
  spaceCount: number;
  channelCount: number;
  routeTargetMissing: boolean;
  xmppStatus: XmppStatusSnapshot["state"];
  onlineEpoch: number;
  lastAttemptedOnlineEpoch: number;
}

export function shouldRetryMissingStructureLoad(input: MissingStructureRetryInput): boolean {
  return (
    input.appReady &&
    input.hasClient &&
    input.initialLoadFinished &&
    !input.inFlight &&
    !input.isLoadingStructure &&
    (input.spaceCount === 0 || input.channelCount === 0 || input.routeTargetMissing) &&
    input.xmppStatus === "online" &&
    input.onlineEpoch > input.lastAttemptedOnlineEpoch
  );
}

export function shouldPreserveActiveChannelDuringStructureRetry(input: {
  activeChannelListed: boolean;
  routeTargetMissing: boolean;
}): boolean {
  return input.activeChannelListed && !input.routeTargetMissing;
}
