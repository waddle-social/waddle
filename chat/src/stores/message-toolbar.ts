import { atom } from "nanostores";

// Desktop reaction toolbars are mutually exclusive while an emoji picker is open.
export const $desktopToolbarOwnerId = atom<string | null>(null);
