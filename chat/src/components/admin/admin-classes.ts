import { css, cx } from "styled-system/css";
import { btn, input, tag } from "styled-system/recipes";

/** Shared admin layout classes (App-Admin / Admin-Spaces / Admin-Channels boards). */
export const adminClasses = {
  /** Labelled control: label text above, input below. */
  field: css({ display: "flex", flexDirection: "column", gap: "1.5", minWidth: 0, fontSize: "13px", fontWeight: 600, color: "fg.muted" }),
  input: cx(input(), css({ width: "100%", color: "fg", fontWeight: 400 })),
  textarea: cx(input(), css({ width: "100%", height: "auto", py: "2.5", color: "fg", fontWeight: 400, font: "400 14px/1.5 {fonts.body}", resize: "vertical" })),
  smallSelect: cx(input(), css({ height: "36px", minHeight: "36px", width: "120px", flex: "none", fontSize: "13px", _coarse: { height: "44px", minHeight: "44px" } })),
  search: cx(input(), css({ width: "100%", maxWidth: "480px" })),
  alert: css({ padding: "10px 14px", borderRadius: "control", border: "1px solid {colors.danger}", bg: "danger.subtle", color: "fg", fontSize: "14px" }),
  status: css({ padding: "12", textAlign: "center", color: "fg.muted", fontSize: "14px", "& p": { margin: 0 } }),
  list: css({ display: "flex", flexDirection: "column", margin: 0, padding: 0, listStyle: "none", border: "1px solid {colors.border.soft}", borderRadius: "panel", bg: "surface", overflow: "hidden", "& > li + li": { borderTop: "1px solid {colors.border.soft}" } }),
  row: css({
    display: "flex",
    alignItems: "center",
    gap: "3",
    width: "100%",
    padding: "12px 16px",
    border: 0,
    bg: "transparent",
    color: "fg",
    font: "inherit",
    textAlign: "left",
    "&:is(button)": { cursor: "pointer", _hover: { bg: "state.hover" } },
    _narrow: { flexWrap: "wrap" },
  }),
  rowMain: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5" }),
  rowTitle: css({ display: "flex", alignItems: "center", gap: "1.5", flexWrap: "wrap", minWidth: 0, fontWeight: 700 }),
  truncate: css({ overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  jid: css({ font: "13px {fonts.mono}", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  sub: css({ fontSize: "13px", color: "fg.muted", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  meta: css({ fontSize: "13px", color: "fg.muted", whiteSpace: "nowrap" }),
  tag: tag(),
  more: cx(btn(), css({ alignSelf: "center" })),
  /** Drawer body and its sections. */
  drawerBody: css({ display: "flex", flexDirection: "column", gap: "4", padding: "4px 22px 22px" }),
  section: css({ display: "flex", flexDirection: "column", gap: "2.5", pt: "14px", borderTop: "1px solid {colors.border.soft}", "& h3": { margin: 0, font: "700 15px/1.3 {fonts.display}" } }),
  danger: css({ display: "flex", flexDirection: "column", gap: "2.5", padding: "14px", borderRadius: "control", border: "1px solid {colors.danger}", bg: "danger.subtle", "& h3": { margin: 0, font: "700 15px/1.3 {fonts.display}", color: "danger.text" }, "& p": { margin: 0, fontSize: "13px", lineHeight: 1.5 } }),
  /** Small row inside a drawer list (member role, affiliation, occupant). */
  subRow: css({ display: "flex", alignItems: "center", gap: "2.5", py: "2", borderTop: "1px solid {colors.border.soft}", _first: { borderTop: 0 } }),
  checks: css({ display: "flex", flexDirection: "column", gap: "2", "& label": { display: "flex", alignItems: "center", gap: "2.5", fontSize: "14px", color: "fg", cursor: "pointer" }, "& input": { width: "18px", height: "18px", accentColor: "{colors.action}" }, "& p": { margin: 0, fontSize: "13px", color: "fg.muted" } }),
  grid2: css({ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "3", _narrow: { gridTemplateColumns: "1fr" } }),
  plain: css({ margin: 0, padding: 0, listStyle: "none", display: "flex", flexDirection: "column" }),
  grow: css({ flex: 1, minWidth: 0 }),
  srOnly: css({ srOnly: true }),
  start: css({ alignSelf: "flex-start" }),
  drawerWidth: css({ width: "min(100vw, 512px)" }),
  drawerTitle: css({ display: "block", font: "700 22px/1.25 {fonts.display}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
};
