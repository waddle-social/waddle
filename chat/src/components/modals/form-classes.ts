import { css, cx } from "styled-system/css";
import { btn, input } from "styled-system/recipes";

/**
 * Form parts shared by the room, community and member dialogs (Rooms-Dialogs
 * board): a muted label above a 44px input, two-up option cards, captions,
 * and the outlined danger button that sits at the start of the footer.
 */
export const formClasses = {
  field: css({ display: "flex", flexDirection: "column", gap: "1.5", minWidth: 0 }),
  label: css({ font: "600 13px/1.3 {fonts.body}", color: "fg.muted" }),
  input: cx(input(), css({ width: "100%" })),
  textarea: cx(input(), css({ width: "100%", height: "auto", minHeight: "72px", py: "2.5", font: "14px/1.5 {fonts.body}", resize: "vertical" })),
  options: css({ display: "grid", gridTemplateColumns: "repeat(2, minmax(0, 1fr))", gap: "2" }),
  optionTitle: css({ display: "inline-flex", alignItems: "center", gap: "1.5", font: "700 14px/1.3 {fonts.body}", color: "fg", "& svg": { width: "16px", height: "16px", flex: "none" } }),
  caption: css({ margin: 0, font: "400 13px/1.4 {fonts.body}", color: "fg.muted" }),
  dangerOutline: cx(btn(), css({ color: "danger.text", borderColor: "danger", mr: "auto", _hover: { color: "danger.text", borderColor: "danger" } })),
  /** A pickable person row (search result, contact); `aria-pressed` selects it. */
  pickRow: css({
    display: "flex",
    alignItems: "center",
    gap: "2.5",
    width: "100%",
    minHeight: "48px",
    px: "2",
    py: "1.5",
    border: "1px solid transparent",
    borderRadius: "control",
    bg: "transparent",
    color: "fg",
    font: "inherit",
    textAlign: "left",
    cursor: "pointer",
    _hover: { bg: "state.hover" },
    '&[aria-pressed="true"]': { bg: "state.selected", borderColor: "border.strong" },
  }),
  rowText: css({ display: "flex", flexDirection: "column", flex: 1, minWidth: 0, lineHeight: 1.3, "& > *": { overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" } }),
  rowName: css({ font: "700 14px/1.3 {fonts.body}", color: "fg" }),
  rowSub: css({ font: "400 13px/1.35 {fonts.body}", color: "fg.muted" }),
  status: css({ display: "flex", alignItems: "center", gap: "2", minHeight: "20px", font: "400 13px/1.4 {fonts.body}", color: "fg.muted" }),
  /** Footer that stacks on phones: danger action above Cancel / Save. */
  footSplit: css({ flexWrap: "wrap" }),
};
