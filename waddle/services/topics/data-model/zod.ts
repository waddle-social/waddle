import { createInsertSchema, createSelectSchema } from "drizzle-zod";
import { z } from "zod";
import * as dataSchema from "./schema";

const scopeValues = dataSchema.topics.scope.enumValues ?? [];

if (scopeValues.length === 0) {
  throw new Error("Topics scope enum must define at least one value");
}

const scopeEnumValues = scopeValues as [string, ...string[]];

export const topicScopeEnum = z.enum(scopeEnumValues);
export const TOPIC_SCOPE_VALUES = scopeEnumValues;

const titleSchema = z.string().trim().min(1, "Title is required").max(120);
const descriptionSchema = z
  .string()
  .max(512, "Description must be 512 characters or fewer")
  .optional()
  .or(z.literal(null));

export const selectTopicSchema = createSelectSchema(dataSchema.topics, {
  title: () => titleSchema,
  description: () => descriptionSchema,
  scope: () => topicScopeEnum,
  ownerId: (schema) => schema.nullish(),
  waddleSlug: (schema) => schema.nullish(),
  createdAt: (schema) => schema.transform((value) => Number(value)),
  updatedAt: (schema) => schema.transform((value) => Number(value)),
});
export type Topic = z.infer<typeof selectTopicSchema>;

export const insertTopicSchema = createInsertSchema(dataSchema.topics, {
  title: () => titleSchema,
  description: () => descriptionSchema,
  scope: () => topicScopeEnum,
  ownerId: (schema) => schema.nullish(),
  waddleSlug: (schema) => schema.nullish(),
  createdAt: (schema) => schema.optional(),
  updatedAt: (schema) => schema.optional(),
}).superRefine((topic, ctx) => {
  const { scope, ownerId, waddleSlug } = topic;

  const ownerPresent = ownerId != null && ownerId !== "";
  const waddlePresent = waddleSlug != null && waddleSlug !== "";

  if (scope === "owner") {
    if (!ownerPresent) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["ownerId"],
        message: "ownerId is required when scope is 'owner'",
      });
    }

    if (waddlePresent) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["waddleSlug"],
        message: "waddleSlug must be null when scope is 'owner'",
      });
    }
  }

  if (scope === "waddle") {
    if (!waddlePresent) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["waddleSlug"],
        message: "waddleSlug is required when scope is 'waddle'",
      });
    }

    if (ownerPresent) {
      ctx.addIssue({
        code: z.ZodIssueCode.custom,
        path: ["ownerId"],
        message: "ownerId must be null when scope is 'waddle'",
      });
    }
  }

  if (scope === "global" && (ownerPresent || waddlePresent)) {
    ctx.addIssue({
      code: z.ZodIssueCode.custom,
      path: ["scope"],
      message: "Global topics cannot be associated with an owner or waddle",
    });
  }
});
export type NewTopic = z.infer<typeof insertTopicSchema>;
