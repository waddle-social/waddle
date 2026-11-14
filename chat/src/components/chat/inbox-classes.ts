import { css } from "styled-system/css";
import { empty } from "styled-system/recipes";

/**
 * Page frame shared by the Inbox (Unread / Discussions) and Home pages
 * (App-Unread, App-Discussions, App-Home boards): a scrolling column with a
 * 1000px reading width, the 30px display title and the board empty state.
 */
export const pageClasses = {
  scroll: css({ flex: "1 1 0%", minHeight: 0, overflowX: "hidden", overflowY: "auto", scrollbarGutter: "stable", bg: "bg", color: "fg" }),
  page: css({
    width: "100%",
    maxWidth: "1000px",
    boxSizing: "border-box",
    display: "flex",
    flexDirection: "column",
    gap: "2.5",
    padding: "12px",
    md: { gap: "6", padding: "32px 40px" },
  }),
  head: css({ display: "flex", alignItems: "flex-end", flexWrap: "wrap", gap: "4" }),
  headText: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "1.5" }),
  // On phones the rail header carries the title; keep it for screen readers.
  title: css({
    position: "absolute", width: "1px", height: "1px", overflow: "hidden", clip: "rect(0 0 0 0)", whiteSpace: "nowrap",
    md: { position: "static", width: "auto", height: "auto", overflow: "visible", clip: "auto", whiteSpace: "normal", margin: 0, font: "700 30px/1.2 {fonts.display}", letterSpacing: "-0.01em" },
  }),
  sub: css({ margin: 0, fontSize: "15px", color: "fg.muted" }),
  empty: empty(),
  emptyTitle: css({ margin: 0, font: "700 24px/1.25 {fonts.display}", color: "fg" }),
  status: css({ display: "flex", alignItems: "center", justifyContent: "center", gap: "2.5", padding: "48px 0", color: "fg.muted", fontSize: "14px" }),
  alert: css({
    display: "flex",
    alignItems: "center",
    gap: "3",
    padding: "12px 16px",
    borderRadius: "panel",
    bg: "danger.subtle",
    border: "1px solid {colors.danger}",
    fontSize: "14px",
    color: "fg",
  }),
};
