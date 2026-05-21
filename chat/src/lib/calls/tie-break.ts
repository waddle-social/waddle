const textEncoder = new TextEncoder();

export function compareOctetStrings(left: string, right: string): number {
  const leftBytes = textEncoder.encode(left);
  const rightBytes = textEncoder.encode(right);
  const len = Math.min(leftBytes.length, rightBytes.length);
  for (let i = 0; i < len; i += 1) {
    const diff = leftBytes[i] - rightBytes[i];
    if (diff !== 0) return diff;
  }
  return leftBytes.length - rightBytes.length;
}

function bareCallJid(jid: string): string {
  return (jid.split("/")[0] ?? "").toLowerCase();
}

export function isSameCallBareJid(left: string, right: string): boolean {
  return bareCallJid(left) === bareCallJid(right);
}

export function incomingProposeWinsTieBreak(
  incomingSid: string,
  outgoingSid: string,
  incomingFrom: string,
  ourFullJid: string,
): boolean {
  const sidOrder = compareOctetStrings(incomingSid, outgoingSid);
  if (sidOrder !== 0) return sidOrder < 0;
  return compareOctetStrings(incomingFrom, ourFullJid) < 0;
}
