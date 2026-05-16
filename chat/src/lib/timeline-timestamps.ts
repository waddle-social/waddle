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
  left: { createdAt?: string; id: string },
  right: { createdAt?: string; id: string },
): number {
  const timeOrder = compareTimelineTimestamps(left.createdAt, right.createdAt);
  if (timeOrder !== 0) return timeOrder;
  return left.id.localeCompare(right.id);
}
