import SchemaBuilder from "@pothos/core";
import directivesPlugin from "@pothos/plugin-directives";
import drizzlePlugin from "@pothos/plugin-drizzle";
import federationPlugin from "@pothos/plugin-federation";
import { asc, eq } from "drizzle-orm";
import { drizzle } from "drizzle-orm/d1";
import type { GraphQLSchema } from "graphql";
import * as dataSchema from "../../data-model/schema.ts";
import { WADDLE_VISIBILITY_VALUES } from "../../data-model/zod.ts";
import type { Env } from "./index.ts";

interface SchemaBuilderTypes {
  DrizzleSchema: typeof dataSchema;
}

const buildSchema = (env: Env) => {
  const db = drizzle(env.DB, { schema: dataSchema });

  const builder = new SchemaBuilder<SchemaBuilderTypes>({
    plugins: [directivesPlugin, drizzlePlugin, federationPlugin],
    drizzle: {
      client: db,
    },
  });

  const visibilityEnum = builder.enumType("WaddleVisibility", {
    values: Object.fromEntries(
      WADDLE_VISIBILITY_VALUES.map((value) => [value.toUpperCase(), { value }]),
    ),
  });

  type WaddleRecord = typeof dataSchema.waddle.$inferSelect;

  const waddleRef = builder.objectRef<WaddleRecord>("Waddle").implement({
    directives: {
      key: {
        fields: "slug",
      },
    },
    fields: (t) => ({
      slug: t.exposeID("slug"),
      name: t.exposeString("name"),
      visibility: t.field({
        type: visibilityEnum,
        resolve: (waddle) => waddle.visibility,
      }),
    }),
  });

  builder.queryType({
    fields: (t) => ({
      health: t.boolean({
        directives: {
          shareable: true,
        },
        resolve: () => true,
      }),
      getWaddle: t.field({
        type: waddleRef,
        nullable: true,
        args: {
          slug: t.arg.string({ required: true }),
        },
        resolve: async (_root, args) =>
          db.query.waddle.findFirst({
            where: eq(dataSchema.waddle.slug, args.slug),
          }),
      }),
      getWaddles: t.field({
        type: [waddleRef],
        resolve: () =>
          db
            .select()
            .from(dataSchema.waddle)
            .orderBy(asc(dataSchema.waddle.slug)),
      }),
    }),
  });

  return builder;
};

export const getSchema = (env: Env): GraphQLSchema => {
  const builder = buildSchema(env);

  return builder.toSubGraphSchema({
    linkUrl: "https://specs.apollo.dev/federation/v2.6",
    federationDirectives: ["@extends", "@external", "@key"],
  });
};
