import SchemaBuilder from "@pothos/core";
import type { Env } from "./index";

// TODO: Extend this schema with the fields required by your service.
export const getSchema = (_env: Env) => {
  const builder = new SchemaBuilder({});

  builder.queryType({
    fields: (t) => ({
      health: t.boolean({
        resolve: () => true,
      }),
    }),
  });

  return builder.toSchema();
};
