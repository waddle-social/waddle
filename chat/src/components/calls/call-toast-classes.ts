import { css, cx } from "styled-system/css";
import { btn, iconbtn } from "styled-system/recipes";

/**
 * Ringing / ended / error call toasts (Huddle-DM board). They render with
 * `data-theme="night"` so they stay ink in both themes, like other toasts.
 */
const card = {
  position: "fixed",
  zIndex: "toast",
  right: "24px",
  width: "320px",
  maxWidth: "calc(100vw - 32px)",
  boxSizing: "border-box",
  padding: "4",
  borderRadius: "16px",
  bg: "surface",
  border: "1px solid {colors.border}",
  boxShadow: "overlay",
  color: "fg",
  animation: "wd-toast {durations.slow} {easings.spring.snappy} backwards",
  _narrow: { right: "12px", left: "12px", width: "auto", maxWidth: "none" },
} as const;

export const callToastClasses = {
  root: css(card, { bottom: "24px", display: "flex", flexDirection: "column", gap: "3.5" }),
  error: css(card, { top: "80px", width: "340px", display: "flex", alignItems: "flex-start", gap: "3", padding: "14px 16px", borderColor: "danger" }),
  head: css({ display: "flex", alignItems: "center", gap: "3" }),
  icon: css({
    display: "inline-flex",
    flex: "none",
    width: "40px",
    height: "40px",
    alignItems: "center",
    justifyContent: "center",
    borderRadius: "pill",
    bg: "action.subtle",
    color: "action.text",
    "& svg": { width: "20px", height: "20px" },
    "&[data-pulse=true]": { _motionSafe: { animation: "wd-ripple 1.6s {easings.out} infinite" } },
    "&[data-tone=quiet]": { bg: "surface.2", color: "fg.muted" },
  }),
  errorIcon: css({ flex: "none", width: "20px", height: "20px", marginTop: "1px", color: "danger.text" }),
  copy: css({ flex: 1, minWidth: 0, display: "flex", flexDirection: "column", gap: "0.5", lineHeight: "1.35" }),
  title: css({ font: "700 15px/1.35 {fonts.body}", overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  errorTitle: css({ font: "700 14px/1.35 {fonts.body}" }),
  status: css({ textStyle: "caption", color: "fg.muted" }),
  alert: css({ padding: "6px 10px", borderRadius: "control", border: "1px solid {colors.danger}", bg: "danger.subtle", color: "danger.text", textStyle: "caption" }),
  actions: css({ display: "grid", gridTemplateColumns: "1fr 1fr", gap: "2" }),
  decline: cx(btn(), css({ height: "40px", justifyContent: "center", color: "danger.text", _coarse: { height: "44px" } })),
  accept: cx(btn({ variant: "primary" }), css({ height: "40px", justifyContent: "center", _coarse: { height: "44px" } })),
  cancel: cx(btn(), css({ height: "40px", justifyContent: "center", _coarse: { height: "44px" } })),
  dismiss: iconbtn({ size: "sm" }),
};
