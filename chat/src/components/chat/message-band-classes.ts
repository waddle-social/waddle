import { css } from "styled-system/css";

/**
 * Extension event bands (bot notifications, inline event cards) and the
 * tool-action chip strip, shared by MessageSystemBand and MessageBody.
 * Tone comes from `data-tone` (success | danger | warning), payload kind
 * from `data-kind`, chip state from `data-state`.
 */
export const bandClasses = {
  root: css({
    "--band-tone": "{colors.action.text}",
    "&[data-tone=success]": { "--band-tone": "{colors.success}" },
    "&[data-tone=danger]": { "--band-tone": "{colors.danger.text}" },
    "&[data-tone=warning]": { "--band-tone": "{colors.warning}" },
    position: "relative",
    display: "flex",
    flexDirection: "column",
    gap: "1",
    width: "100%",
    padding: "{spacing.2.5} {spacing.4} {spacing.2.5} {spacing.6}",
    mb: "2",
    borderBlock: "1px solid {colors.border.soft}",
    transition: "background-color {durations.fast} {easings.out}",
    animation: "wd-rise {durations.base} {easings.out} backwards",
    _before: {
      content: '""',
      position: "absolute",
      insetBlock: 0,
      left: 0,
      width: "3px",
      bg: "var(--band-tone)",
    },
    _hover: { bg: "color-mix(in srgb, var(--band-tone) 5%, transparent)" },
    _narrow: { paddingInline: "{spacing.6} {spacing.3}" },
    // Inline (inside a human message): an opaque card, the rail inset.
    "&[data-inline]": {
      mb: 0,
      border: "1px solid {colors.border}",
      borderRadius: "control",
      bg: "surface",
      _before: { insetBlock: "6px", borderRadius: "pill" },
    },
  }),
  header: css({ display: "flex", alignItems: "center", gap: "2", textStyle: "caption", fontWeight: 700, color: "fg.muted" }),
  source: css({ display: "inline-flex", alignItems: "center", gap: "1.5", color: "var(--band-tone)", "& svg": { width: "16px", height: "16px" } }),
  stamp: css({ textStyle: "timestamp", color: "fg.muted", fontVariantNumeric: "tabular-nums" }),
  pill: css({ ml: "auto", px: "2", py: "0.5", borderRadius: "pill", color: "var(--band-tone)", bg: "color-mix(in srgb, var(--band-tone) 12%, transparent)" }),
  title: css({ fontFamily: "display", fontWeight: 700, fontSize: "base", lineHeight: 1.35, color: "fg", overflowWrap: "anywhere", _narrow: { fontSize: "md" } }),
  titleLink: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1",
    color: "inherit",
    textDecoration: "none",
    _hover: { color: "var(--band-tone)" },
    "& svg": { width: "14px", height: "14px", opacity: 0.6 },
  }),
  meta: css({ display: "flex", flexWrap: "wrap", alignItems: "baseline", gap: "{spacing.0.5} {spacing.2}", textStyle: "caption", color: "fg.muted" }),
  metaItem: css({
    display: "inline-flex",
    alignItems: "baseline",
    gap: "1",
    minWidth: 0,
    "& + &::before": { content: '"·"', mr: "1.5", color: "fg.soft" },
  }),
  metaLabel: css({
    fontFamily: "mono",
    fontSize: "10px",
    letterSpacing: "0.08em",
    textTransform: "uppercase",
    color: "fg.soft",
    "[data-kind=github-event] &": { display: "none" },
  }),
  metaValue: css({ color: "fg", fontVariantNumeric: "tabular-nums", "&[data-mono]": { fontFamily: "mono", fontSize: "11px" } }),
  actions: css({ display: "flex", flexWrap: "wrap", gap: "1.5" }),
};

export const actionChipClasses = {
  strip: css({ display: "flex", flexWrap: "wrap", alignItems: "center", gap: "1.5", mt: "1" }),
  chip: css({
    display: "inline-flex",
    alignItems: "center",
    gap: "1.5",
    minHeight: "28px",
    px: "2.5",
    borderRadius: "pill",
    border: "1px solid {colors.border}",
    bg: "surface",
    color: "fg",
    fontSize: "2xs",
    fontWeight: 600,
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}, border-color {durations.fast} {easings.out}, color {durations.fast} {easings.out}",
    "& svg": { width: "14px", height: "14px", flex: "none", opacity: 0.75 },
    "&:hover:not(:disabled)": { bg: "action.subtle", borderColor: "action.text", color: "action.text" },
    _focusVisible: { outline: "3px solid {colors.focus}", outlineOffset: "2px" },
    _disabled: { cursor: "not-allowed", opacity: 0.55 },
    "&[data-state=error]": { borderColor: "danger.text", color: "danger.text" },
    "&[data-state=warning]": { borderColor: "warning", color: "warning.text" },
    "&[data-state=success]": { borderColor: "success", color: "success.text" },
    _coarse: { minHeight: "44px" },
  }),
  source: css({ fontFamily: "mono", fontSize: "10px", letterSpacing: "0.08em", textTransform: "uppercase", color: "fg.muted" }),
  detail: css({
    flexBasis: "100%",
    mt: "0.5",
    fontSize: "2xs",
    color: "warning.text",
    "&[data-state=error]": { color: "danger.text" },
  }),
};
