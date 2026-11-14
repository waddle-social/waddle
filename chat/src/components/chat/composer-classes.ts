import { css } from "styled-system/css";

/**
 * Composer popovers (mention, emoji, slash, GIF) and their pieces: the
 * same floating surface as the menus (menuClasses.content), anchored
 * above the composer field (below it when the timeline is top-pinned).
 */
export const composerPopover = {
  root: css({
    position: "absolute",
    left: "6",
    right: "6",
    zIndex: "dropdown",
    maxHeight: "224px",
    overflow: "auto",
    p: "1.5",
    display: "flex",
    flexDirection: "column",
    gap: "0.5",
    bg: "surface",
    color: "fg",
    border: "1px solid {colors.border}",
    borderRadius: "panel",
    boxShadow: "overlay",
    animation: "wd-pop {durations.base} {easings.spring.snappy} backwards",
    _narrow: { left: "3", right: "3" },
  }),
  above: css({ bottom: "100%", mb: "2" }),
  below: css({ top: "100%", mt: "2" }),
  option: css({
    display: "flex",
    alignItems: "center",
    gap: "2",
    width: "100%",
    minHeight: "36px",
    px: "3",
    border: 0,
    borderRadius: "10px",
    bg: "transparent",
    color: "fg",
    font: "500 14px/1.25 {fonts.body}",
    textAlign: "left",
    cursor: "pointer",
    transition: "background-color {durations.fast} {easings.out}",
    _hover: { bg: "state.hover" },
    "&[data-selected]": { bg: "state.selected" },
    _coarse: { minHeight: "44px" },
  }),
  hint: css({ textStyle: "caption", color: "fg.muted", minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }),
  glyph: css({
    display: "inline-flex",
    alignItems: "center",
    justifyContent: "center",
    width: "20px",
    height: "20px",
    flex: "none",
    borderRadius: "xs",
    bg: "action.subtle",
    color: "action.text",
    fontSize: "xs",
    objectFit: "cover",
    "& svg": { width: "12px", height: "12px" },
  }),
};

/** Search field at the top of the emoji and GIF pickers. */
export const pickerSearch = {
  bar: css({ p: "2", borderBottom: "1px solid {colors.border.soft}" }),
  box: css({
    position: "relative",
    minWidth: 0,
    borderRadius: "control",
    bg: "surface.2",
    _focusWithin: { outline: "2px solid {colors.focus}", outlineOffset: "0" },
  }),
  icon: css({ position: "absolute", left: "3", top: "50%", translate: "0 -50%", width: "14px", height: "14px", color: "fg.muted", pointerEvents: "none" }),
  input: css({
    width: "100%",
    minHeight: "40px",
    py: "2.5",
    pl: "36px",
    pr: "44px",
    border: 0,
    bg: "transparent",
    color: "fg",
    textStyle: "input",
    outline: "none",
    _placeholder: { color: "fg.soft" },
  }),
  close: css({ position: "absolute", right: "1.5", top: "50%", translate: "0 -50%" }),
  state: css({ display: "flex", flexDirection: "column", alignItems: "center", justifyContent: "center", gap: "1.5", height: "96px", px: "4", textAlign: "center", textStyle: "caption", color: "fg.muted", "& svg": { width: "20px", height: "20px" } }),
};

/** A selected formatting tool without `aria-pressed` (bubble toolbar, link input open). */
export const toolActive = css({ bg: "state.selected", color: "fg" });

/** The link URL field in the formatting bar and bubble toolbar. */
export const linkInput = css({
  height: "32px",
  width: "192px",
  minWidth: 0,
  px: "2",
  borderRadius: "sm",
  border: "1px solid {colors.border.strong}",
  bg: "bg",
  color: "fg",
  textStyle: "caption",
  outline: "none",
  _focus: { borderColor: "focus" },
  _placeholder: { color: "fg.soft" },
});

/** Loader glyph spin (still under reduced motion). */
export const spin = css({ _motionSafe: { animation: "spin 1s linear infinite" } });
