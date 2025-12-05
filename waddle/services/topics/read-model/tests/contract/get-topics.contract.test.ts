import { describe, expect, it } from "vitest";
import type { D1Database } from "@cloudflare/workers-types";
import { getSchema } from "../../src/schema";

const createStubDb = () => ({
  prepare: () => {
    throw new Error("Not implemented in contract test stub");
  },
}) as unknown as D1Database;

describe("GraphQL contract - getTopics", () => {
  it("exposes getTopics query with connection shape", () => {
    const schema = getSchema({ DB: createStubDb() });
    const queryType = schema.getQueryType();
    expect(queryType).toBeDefined();

    const fields = queryType?.getFields() ?? {};
    expect(fields).toHaveProperty("getTopics");

    const field = fields.getTopics;
    expect(field.type.toString()).toContain("TopicConnection");
  });
});
