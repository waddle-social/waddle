import { css, cx } from "styled-system/css";
import { btn, input, kicker, tag } from "styled-system/recipes";

const truncate = css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" });

/**
 * Parts of the "Communities on other servers" dialog: the address form, the
 * lookup result, and the cards for followed Spaces, joined rooms and
 * suggestions. Dialog head and body come from `dialogParts`, fields from
 * `formClasses`.
 */
export const remoteCommunityClasses = {
  title: truncate,
  intro: css({ margin: 0, font: "400 13px/1.45 {fonts.body}", color: "fg.muted" }),
  lookupRow: css({ display: "flex", alignItems: "center", gap: "2" }),
  addressInput: cx(input(), css({ flex: 1, minWidth: 0 })),
  lookupButton: btn({ variant: "primary" }),

  section: css({ display: "flex", flexDirection: "column", gap: "2.5" }),
  sectionTitle: cx(kicker(), css({ margin: 0 })),
  caption: css({ margin: 0, font: "400 13px/1.45 {fonts.body}", color: "fg.muted" }),
  error: css({ margin: 0, font: "400 13px/1.45 {fonts.body}", color: "danger.text" }),
  working: css({ display: "flex", alignItems: "center", gap: "1.5", margin: 0, font: "400 13px/1.45 {fonts.body}", color: "fg.muted", "& svg": { width: "16px", height: "16px", flex: "none" } }),

  /** A result or status message; `data-tone` is `danger`, `warning`, `progress` or `neutral`. */
  notice: css({
    "--notice-edge": "{colors.border.soft}",
    "--notice-bg": "{colors.surface.2}",
    "&[data-tone=danger]": { "--notice-edge": "{colors.danger}", "--notice-bg": "{colors.danger.subtle}" },
    "&[data-tone=warning]": { "--notice-edge": "{colors.warning}", "--notice-bg": "{colors.warning.subtle}" },
    display: "flex",
    flexDirection: "column",
    alignItems: "flex-start",
    gap: "1",
    padding: "{spacing.2.5} {spacing.3}",
    border: "1px solid var(--notice-edge)",
    borderRadius: "control",
    bg: "var(--notice-bg)",
    color: "fg",
    font: "400 13px/1.45 {fonts.body}",
    "& p": { margin: 0 },
  }),
  noticeTitle: css({ fontWeight: 700 }),
  noticeMeta: css({ color: "fg.muted" }),

  /** A bordered card for one room, Space or lookup result. */
  card: css({
    display: "flex",
    flexDirection: "column",
    gap: "2",
    minWidth: 0,
    padding: "3",
    border: "1px solid {colors.border.soft}",
    borderRadius: "control",
    bg: "surface",
  }),
  head: css({ display: "flex", alignItems: "center", gap: "2.5", minWidth: 0 }),
  text: css({ display: "flex", flexDirection: "column", gap: "0.5", flex: 1, minWidth: 0 }),
  name: cx(truncate, css({ font: "700 14px/1.3 {fonts.body}", color: "fg" })),
  meta: cx(truncate, css({ font: "400 13px/1.35 {fonts.body}", color: "fg.muted" })),
  body: css({ margin: 0, font: "400 14px/1.45 {fonts.body}", color: "fg" }),
  note: css({ margin: 0, font: "400 13px/1.45 {fonts.body}", color: "fg.muted" }),
  /** Stands in for a Space image, which is never fetched. */
  initial: css({ display: "flex", alignItems: "center", justifyContent: "center", flex: "none", width: "36px", height: "36px", borderRadius: "control", bg: "surface.2", font: "700 14px/1 {fonts.body}", color: "fg.muted" }),
  initialSm: css({ display: "flex", alignItems: "center", justifyContent: "center", flex: "none", width: "32px", height: "32px", borderRadius: "control", bg: "surface.2", font: "700 14px/1 {fonts.body}", color: "fg.muted" }),
  globe: css({ width: "18px", height: "18px", flex: "none", color: "fg.muted" }),
  flags: css({ display: "flex", flexWrap: "wrap", gap: "1" }),
  tag: tag(),
  actions: css({ display: "flex", flex: "none", alignItems: "center", gap: "1.5" }),
  end: css({ display: "flex", justifyContent: "flex-end" }),

  /** Rooms inside a card, one row each with its action at the end. */
  list: css({
    display: "flex",
    flexDirection: "column",
    margin: 0,
    padding: 0,
    listStyle: "none",
    border: "1px solid {colors.border.soft}",
    borderRadius: "control",
    overflow: "hidden",
    "& > li + li": { borderTop: "1px solid {colors.border.soft}" },
  }),
  row: css({ display: "flex", alignItems: "center", gap: "2.5", minWidth: 0, padding: "{spacing.2} {spacing.3}" }),
  rowEmpty: css({ padding: "{spacing.2} {spacing.3}", font: "400 13px/1.45 {fonts.body}", color: "fg.muted" }),

  button: btn({ size: "sm" }),
  primaryButton: btn({ variant: "primary", size: "sm" }),
  dangerButton: cx(btn({ size: "sm" }), css({ color: "danger.text" })),
};
