import { menu } from "styled-system/recipes";

/**
 * Class names for the Ark Menu anatomy, from the `menu` slot recipe in
 * `panda.config.ts`. Consumers that render their own `Menu.Item`,
 * `Menu.ItemGroupLabel` or `Menu.Separator` inside `AppMenu` apply these
 * so every menu in the app shares one surface.
 */
export const menuClasses = menu();

/** Item row with a leading icon and a label/hint stack. */
export const menuItemIconClass = "inline-flex h-4 w-4 shrink-0 items-center justify-center text-muted-foreground";
export const menuItemStackClass = "flex min-w-0 flex-col text-left";
export const menuItemHintClass = "type-caption text-muted-foreground";
