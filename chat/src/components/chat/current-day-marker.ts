import { formatTimelineDayDivider } from "@/channels/timeline";

/**
 * Resolve the sticky "current day" label for the timeline scroller by
 * probing which day-marker / message element sits at the top edge of
 * the container. Returns "" when nothing is rendered yet.
 */
export function currentDayMarkerLabelFor(container: HTMLElement | null): string {
  if (!container) return "";

  const markerEls = [
    ...container.querySelectorAll<HTMLElement>("[data-day-marker-created-at], [data-message-created-at]"),
  ];
  if (markerEls.length === 0) return "";

  const containerTop = container.getBoundingClientRect().top;
  const probeTop = containerTop + 1;
  let current = markerEls[0];
  for (const el of markerEls) {
    const rect = el.getBoundingClientRect();
    if (rect.bottom < probeTop) {
      current = el;
      continue;
    }
    if (rect.top <= probeTop || current === markerEls[0]) {
      current = el;
    }
    break;
  }

  const createdAt = current.dataset.dayMarkerCreatedAt ?? current.dataset.messageCreatedAt;
  return createdAt ? formatTimelineDayDivider(createdAt) : "";
}
