import { createInsertSchema, createSelectSchema } from "drizzle-zod";
import { z } from "zod";
import * as dataSchema from "./schema.ts";

const visibilityValues = dataSchema.waddle.visibility.enumValues ?? [];

if (visibilityValues.length === 0) {
  throw new Error("Waddle visibility enum must define at least one value");
}

const visibilityEnumValues = visibilityValues as [string, ...string[]];

export const waddleVisibilityEnum = z.enum(visibilityEnumValues);

export const WADDLE_VISIBILITY_VALUES = visibilityEnumValues;

export const defaultWaddleVisibility =
  (dataSchema.waddle.visibility.default ?? visibilityEnumValues[0]) as
    (typeof visibilityEnumValues)[number];

export const selectWaddleSchema = createSelectSchema(dataSchema.waddle);
export type Waddle = z.infer<typeof selectWaddleSchema>;

export const insertWaddleSchema = createInsertSchema(dataSchema.waddle, {
  visibility: (schema) => schema.visibility.default(defaultWaddleVisibility),
});
export type NewWaddle = z.infer<typeof insertWaddleSchema>;
