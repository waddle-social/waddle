import SchemaBuilder from "@pothos/core";
import directivesPlugin from "@pothos/plugin-directives";
import drizzlePlugin from "@pothos/plugin-drizzle";
import federationPlugin from "@pothos/plugin-federation";
import { drizzle } from "drizzle-orm/d1";
import type { GraphQLSchema } from "graphql";
import * as dataSchema from "../../data-model/schema";
import type { Env } from "./index";
import type { GraphQLContext } from "./context";
import {
  createTopicsRepository,
  type TopicRecord,
  type TopicsConnection,
  type TopicsFilter,
  type TopicsPagination,
} from "./repositories/topics.repository";
import { authorizeTopics } from "./guards/authorize-topics";
import { authorizeTopicsAdmin } from "./guards/authorize-topics-admin";
import { recordTopicsQuery } from "./metrics/topics.metrics";

interface SchemaBuilderTypes {
  Context: GraphQLContext;
  DrizzleSchema: typeof dataSchema;
}

const buildSchema = (env: Env) => {
  const db = drizzle(env.DB, { schema: dataSchema });
  const repository = createTopicsRepository(env.DB);

  const builder = new SchemaBuilder<SchemaBuilderTypes>({
    plugins: [directivesPlugin, drizzlePlugin, federationPlugin],
    drizzle: {
      client: db,
    },
  });

  const topicScopeEnum = builder.enumType("TopicScope", {
    values: {
      GLOBAL: { value: "global" },
      OWNER: { value: "owner" },
      WADDLE: { value: "waddle" },
    },
  });

  const topicRef = builder.objectRef<TopicRecord>("Topic").implement({
    directives: {
      key: {
        fields: "id",
      },
    },
    fields: (t) => ({
      id: t.exposeID("id"),
      title: t.exposeString("title"),
      description: t.exposeString("description", { nullable: true }),
      scope: t.field({
        type: topicScopeEnum,
        resolve: (topic) => topic.scope,
      }),
      ownerId: t.exposeID("ownerId", { nullable: true }),
      waddleSlug: t.exposeID("waddleSlug", { nullable: true }),
      createdAt: t.field({
        type: "String",
        resolve: (topic) => new Date(topic.createdAt).toISOString(),
      }),
      updatedAt: t.field({
        type: "String",
        resolve: (topic) => new Date(topic.updatedAt).toISOString(),
      }),
    }),
  });

  const pageInfoRef = builder.objectRef<TopicsConnection["pageInfo"]>("PageInfo").implement({
    fields: (t) => ({
      hasNextPage: t.exposeBoolean("hasNextPage"),
      endCursor: t.exposeString("endCursor", { nullable: true }),
    }),
  });

  const topicEdgeRef = builder.objectRef<
    TopicsConnection["edges"][number]
  >("TopicEdge").implement({
    fields: (t) => ({
      cursor: t.exposeString("cursor"),
      node: t.field({
        type: topicRef,
        resolve: (edge) => edge.node,
      }),
    }),
  });

  const topicConnectionRef = builder.objectRef<TopicsConnection>("TopicConnection").implement({
    fields: (t) => ({
      edges: t.field({
        type: [topicEdgeRef],
        resolve: (connection) => connection.edges,
      }),
      pageInfo: t.field({
        type: pageInfoRef,
        resolve: (connection) => connection.pageInfo,
      }),
      totalCount: t.exposeInt("totalCount"),
    }),
  });

  const topicFilterInput = builder.inputType("TopicFilterInput", {
    fields: (t) => ({
      ownerId: t.id({ required: false }),
      waddleSlug: t.id({ required: false }),
      first: t.int({ required: false }),
      after: t.string({ required: false }),
    }),
  });

  const topicsPaginationInput = builder.inputType("TopicsPaginationInput", {
    fields: (t) => ({
      first: t.int({ required: false }),
      after: t.string({ required: false }),
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
      getTopics: t.field({
        type: topicConnectionRef,
        args: {
          filter: t.arg({ type: topicFilterInput, required: true }),
        },
        resolve: async (_root, args, ctx) => {
          const filter: TopicsFilter = {
            ownerId: args.filter.ownerId ?? undefined,
            waddleSlug: args.filter.waddleSlug ?? undefined,
          };

          const pagination: TopicsPagination = {
            first: args.filter.first ?? undefined,
            after: args.filter.after ?? undefined,
          };

          authorizeTopics();

          const result = await repository.getTopics(filter, pagination);
          recordTopicsQuery("getTopics", result.totalCount);

          return result;
        },
      }),
      getAllTopics: t.field({
        type: topicConnectionRef,
        args: {
          pagination: t.arg({
            type: topicsPaginationInput,
            required: false,
            defaultValue: { first: 50 },
          }),
        },
        resolve: async (_root, args, ctx) => {
          authorizeTopicsAdmin();

          const pagination: TopicsPagination = {
            first: args.pagination?.first ?? undefined,
            after: args.pagination?.after ?? undefined,
          };

          const result = await repository.getAllTopics(pagination);
          recordTopicsQuery("getAllTopics", result.totalCount);

          return result;
        },
      }),
    }),
  });

  return builder;
};

export const getSchema = (env: Env): GraphQLSchema => {
  const builder = buildSchema(env);

  return builder.toSubGraphSchema({
    linkUrl: "https://specs.apollo.dev/federation/v2.6",
    federationDirectives: ["@extends", "@external", "@key", "@shareable"],
  });
};
