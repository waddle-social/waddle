import { css } from "../../styled-system/css";

export const kicker = css({ textStyle: "eyebrow", fontWeight: 500, lineHeight: 1.2 });
export const heading = css({ m: 0, textStyle: "headline", fontWeight: 700, lineHeight: 1.2, letterSpacing: "-0.01em" });
export const lead = css({ m: 0, fontSize: "md", lineHeight: 1.5, color: "fg.muted" });
export const note = css({ m: 0, fontSize: "xs", lineHeight: 1.5, color: "fg.muted" });
export const mono = css({ fontFamily: "mono" });

// Bordered list shared by the account details and the consent scopes.
export const sheet = css({
  m: 0,
  p: 0,
  listStyle: "none",
  border: "1px solid {colors.border.soft}",
  borderRadius: "control",
  overflow: "hidden",
});
