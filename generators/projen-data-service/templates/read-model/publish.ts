import { printSchemaWithDirectives } from "@graphql-tools/utils";
import type { D1Database } from "@cloudflare/workers-types";
import { lexicographicSortSchema } from "graphql";
import { writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { getSchema } from "./src/schema.ts";

const schemaAsString = printSchemaWithDirectives(
  lexicographicSortSchema(
    getSchema({
      DB: {} as D1Database,
    })
  ),
  {
    pathToDirectivesInExtensions: [""],
  }
);

const __dirname = dirname(fileURLToPath(import.meta.url));
writeFileSync(`${__dirname}/schema.gql`, schemaAsString);

console.log("✅ Schema published to read-model/schema.gql");
