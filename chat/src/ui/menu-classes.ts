import { css } from "styled-system/css";

/**
 * Class names for the Ark Menu anatomy (Polar Huddle account menu board).
 * The preset has no menu recipe, so these are plain `css()` classes.
 * Consumers that render their own `Menu.Item`, `Menu.ItemGroupLabel` or
 * `Menu.Separator` inside `AppMenu` apply these so every menu in the app
 * shares one surface. `data-tone="danger"` on an item turns it red;
 * `data-highlighted` (Ark) and `data-disabled` drive the states.
 */
export const menuClasses = {
  content: css({
    minWidth: "240px",
    maxWidth: "min(340px, calc(100vw - 16px))",
    padding: "1.5",
    display: "flex",
    flexDirection: "column",
    gap: "0.5",
    bg: "surface",
    color: "fg",
    border: "1px solid {colors.border}",
    borderRadius: "panel",
    boxShadow: "overlay",
    zIndex: "dropdown",
    outline: "none",
    "--ring-bg": "{colors.surface}",
    animation: "wd-pop {durations.base} {easings.spring.snappy} backwards",
    transformOrigin: "var(--transform-origin)",
  }),
  item: css({
    display: "flex",
    alignItems: "center",
    gap: "2.5",
    width: "100%",
    minHeight: "40px",
    px: "3",
    border: 0,
    borderRadius: "10px",
    bg: "transparent",
    color: "fg",
    font: "500 14px/1.25 {fonts.body}",
    textAlign: "left",
    textDecoration: "none",
    cursor: "pointer",
    outline: "none",
    transition: "background-color {durations.fast} {easings.out}",
    "& svg": { width: "18px", height: "18px", flex: "none", color: "fg.muted" },
    _highlighted: { bg: "state.hover" },
    _disabled: { cursor: "not-allowed", opacity: 0.5 },
    "&[data-tone=danger], &[data-tone=danger] svg": { color: "danger.text" },
    _coarse: { minHeight: "44px" },
  }),
  itemGroupLabel: css({ px: "3", pt: "2", pb: "1", textStyle: "eyebrow" }),
  separator: css({ height: "1px", bg: "border.soft", mx: "1", my: "1", border: 0 }),
};

/** Item row with a leading icon and a label/hint stack. */
export const menuItemIconClass = css({
  display: "inline-flex",
  width: "18px",
  height: "18px",
  flex: "none",
  alignItems: "center",
  justifyContent: "center",
  color: "fg.muted",
});
export const menuItemStackClass = css({ display: "flex", minWidth: 0, flexDirection: "column", gap: "0.5", textAlign: "left" });
export const menuItemHintClass = css({ textStyle: "caption", color: "fg.muted" });
