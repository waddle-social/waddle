import { css, cx } from "styled-system/css";
import { floe, kicker, toast } from "styled-system/recipes";

/**
 * Part classes for anatomies the Polar Huddle preset styles as one block:
 * a toast (`toast` recipe) and a content card (`floe` recipe) with its
 * kicker / title / body / footer. Plain `css()` so chat adds no recipes.
 */
export const toastClasses = {
  root: cx(toast(), css({ alignItems: "flex-start", minWidth: "min(320px, calc(100vw - 32px))", maxWidth: "400px" })),
  title: css({ font: "700 14px/1.3 {fonts.body}", color: "fg" }),
  description: css({ font: "400 13px/1.45 {fonts.body}", color: "fg.muted" }),
  actionTrigger: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    height: "32px",
    px: "3",
    border: 0,
    borderRadius: "pill",
    bg: "action",
    color: "action.fg",
    font: "700 13px/1 {fonts.body}",
    cursor: "pointer",
    _hover: { bg: "action.hover" },
    _disabled: { opacity: 0.5, cursor: "not-allowed" },
  }),
  closeTrigger: css({
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    width: "28px",
    height: "28px",
    flex: "none",
    border: 0,
    borderRadius: "sm",
    bg: "transparent",
    color: "fg.muted",
    cursor: "pointer",
    _hover: { bg: "state.hover", color: "fg" },
    "& svg": { width: "16px", height: "16px" },
  }),
};

export type CardTone = "live" | "active" | "warning" | "quiet";

const cardParts = {
  kicker: kicker(),
  title: css({ margin: 0, font: "700 17px/1.3 {fonts.display}", color: "fg" }),
  body: css({ margin: 0, fontSize: "sm", lineHeight: "1.45", color: "fg.muted" }),
  footer: css({ display: "flex", alignItems: "center", gap: "2", mt: "auto" }),
};

const CARD_ROOT: Record<CardTone, string> = {
  live: cx(floe(), css({ bg: "live.subtle", borderColor: "live" })),
  active: cx(floe(), css({ borderColor: "action.text" })),
  warning: cx(floe(), css({ borderColor: "warning" })),
  quiet: floe(),
};
const LIVE_KICKER = css({ color: "live.text" });

/** A content card: `live` (aurora, happening now), `active`, `warning` or `quiet`. */
export function cardClasses(tone: CardTone = "quiet") {
  return {
    ...cardParts,
    root: CARD_ROOT[tone],
    kicker: tone === "live" ? cx(cardParts.kicker, LIVE_KICKER) : cardParts.kicker,
  };
}

/** Eyebrow label in the live colour ("Happening now", "Live now"). */
export const liveKicker = cx(kicker(), LIVE_KICKER);

/**
 * Dialog layout inside `AppDialog` (the preset `dialog` recipe's head /
 * body / foot, with the padding moved onto the parts so a body can scroll).
 */
export const dialogParts = {
  head: css({ display: "flex", alignItems: "center", gap: "2", padding: "20px 20px 0 24px", "& h2": { margin: 0, flex: 1, minWidth: 0, font: "700 20px/1.25 {fonts.display}" } }),
  body: css({ display: "flex", flexDirection: "column", gap: "3", padding: "16px 24px", overflowY: "auto", minHeight: 0, color: "fg.muted" }),
  foot: css({ display: "flex", justifyContent: "flex-end", alignItems: "center", gap: "2", padding: "8px 24px 24px" }),
};
