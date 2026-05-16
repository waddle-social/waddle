export function compareTimelineTimestamps(
  left: string | null | undefined,
  right: string | null | undefined,
): number {
  const leftValue = typeof left === "string" ? left : "";
  const rightValue = typeof right === "string" ? right : "";
  const leftMs = Date.parse(leftValue);
  const rightMs = Date.parse(rightValue);
  if (Number.isFinite(leftMs) && Number.isFinite(rightMs)) {
    return leftMs === rightMs ? 0 : leftMs < rightMs ? -1 : 1;
  }
  return leftValue.localeCompare(rightValue);
}

export function compareTimelineMessages(
  left: { createdAt?: string },
  right: { createdAt?: string },
): number {
  return compareTimelineTimestamps(left.createdAt, right.createdAt);
}
