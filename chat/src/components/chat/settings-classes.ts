import { css, cx } from "styled-system/css";
import { btn, input } from "styled-system/recipes";

/**
 * Settings-Page board parts shared by UserSettingsPage and VCardEditor:
 * a section card, its heading, labelled fields in a two-column grid and
 * the action row with its live feedback line.
 */
export const settingsClasses = {
  card: css({
    display: "flex",
    flexDirection: "column",
    gap: "4",
    padding: "4",
    md: { padding: "5" },
    borderRadius: "panel",
    bg: "surface",
    border: "1px solid {colors.border.soft}",
    minWidth: 0,
  }),
  cardHead: css({ display: "flex", flexDirection: "column", gap: "0.5" }),
  heading: css({ margin: 0, font: "700 18px/1.25 {fonts.display}", color: "fg" }),
  caption: css({ margin: 0, textStyle: "caption", color: "fg.muted" }),
  grid: css({ display: "grid", gridTemplateColumns: "minmax(0, 1fr)", gap: "3", sm: { gridTemplateColumns: "repeat(2, minmax(0, 1fr))" } }),
  wide: css({ sm: { gridColumn: "1 / -1" } }),
  label: css({ display: "flex", flexDirection: "column", gap: "1.5", minWidth: 0, fontSize: "13px", fontWeight: 600, color: "fg.muted" }),
  input: cx(input(), css({ width: "100%", fontWeight: 400, _disabled: { opacity: 0.5, cursor: "not-allowed" } })),
  textarea: cx(input(), css({ width: "100%", height: "auto", py: "2.5", font: "14px/1.5 {fonts.body}", resize: "vertical", _disabled: { opacity: 0.5, cursor: "not-allowed" } })),
  hint: css({ fontSize: "13px", fontWeight: 400, color: "fg.muted" }),
  hintError: css({ fontSize: "13px", fontWeight: 400, color: "danger.text" }),
  actions: css({ display: "flex", alignItems: "center", flexWrap: "wrap", gap: "2" }),
  primary: btn({ variant: "primary" }),
  secondary: btn(),
};

export type FeedbackTone = "muted" | "success" | "error";

const FEEDBACK: Record<FeedbackTone, string> = {
  muted: css({ margin: 0, textStyle: "caption", color: "fg.muted" }),
  success: css({ margin: 0, textStyle: "caption", color: "action.text" }),
  error: css({ margin: 0, textStyle: "caption", color: "danger.text" }),
};

export function feedbackClass(tone: FeedbackTone): string {
  return FEEDBACK[tone];
}
