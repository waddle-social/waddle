import { css, cx } from "styled-system/css";
import { iconbtn, kicker } from "styled-system/recipes";

/**
 * Page frame shared by the People and Browse rooms pages (Members-Page,
 * Rooms-Page and Mobile-Members boards): an ink header bar with the sidebar
 * button on phones, then a scrolling column with a display heading.
 */
export const pageClasses = {
  root: css({ display: "flex", flexDirection: "column", flex: "1 1 0%", minWidth: 0, minHeight: 0, bg: "bg", color: "fg" }),
  mobileBar: css({
    display: "flex",
    alignItems: "center",
    gap: "1.5",
    flex: "none",
    padding: "2",
    borderBottom: "1px solid {colors.border.soft}",
    bg: "bg.rail",
    color: "fg",
    md: { display: "none" },
  }),
  mobileTitle: css({ flex: 1, minWidth: 0, font: "700 18px/1.3 {fonts.display}" }),
  mobileNav: cx(iconbtn(), css({ width: "44px", height: "44px" })),
  scroll: css({ flex: "1 1 0%", minHeight: 0, overflowX: "hidden", overflowY: "auto" }),
  content: css({
    display: "flex",
    flexDirection: "column",
    gap: "5",
    width: "100%",
    maxWidth: "1000px",
    boxSizing: "border-box",
    padding: "{spacing.4} {spacing.3} {spacing.8}",
    md: { padding: "{spacing.8} {spacing.10}" },
  }),
  head: css({ display: "flex", alignItems: "flex-end", gap: "3", flexWrap: "wrap" }),
  /** The phone header bar already names the page, so below md the heading block is visually hidden (still read). */
  heading: css({
    display: "flex",
    flexDirection: "column",
    gap: "1.5",
    flex: 1,
    minWidth: 0,
    mdDown: { position: "absolute", width: "1px", height: "1px", overflow: "hidden", clip: "rect(0 0 0 0)", whiteSpace: "nowrap" },
  }),
  h1: css({ margin: 0, font: "700 30px/1.2 {fonts.display}", letterSpacing: "-0.01em", _narrow: { fontSize: "24px" } }),
  lead: css({ margin: 0, font: "400 15px/1.45 {fonts.body}", color: "fg.muted" }),
  kicker: kicker(),
  liveKicker: cx(kicker(), css({ display: "inline-flex", alignItems: "center", gap: "1.5", color: "live.text" })),
  /** Small live dot beside a live kicker. */
  ember: css({ width: "8px", height: "8px", flex: "none", borderRadius: "pill", bg: "live" }),
  sectionTitle: cx(kicker(), css({ margin: "{spacing.2} 0 0" })),
};
