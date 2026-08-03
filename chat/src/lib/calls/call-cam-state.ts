import { atom } from "nanostores";

/**
 * Self camera toggle, owned by a leaf module that imports nothing from
 * the call surfaces — mirroring `call-mic-state.ts` — so the control bar
 * (`call-controls.ts`, which imports `use-call-engine`) and the engine's
 * devicechange reconciliation (`use-call-engine.ts`) can both write it
 * without a module cycle. Mirrors the engine: toggles flip it optimistic
 * and roll back on failure; confirmed capture loss forces it off so the
 * media notice's action requests a RE-ENABLE rather than negating a
 * stale on-state into a disable.
 */
export const $callCamEnabled = atom<boolean>(true);
