# Waddle Data Model

Shared Drizzle schema and validation helpers for the data service.

## Included

- `schema.ts` – Drizzle ORM table definitions.
- `zod.ts` – Zod schemas derived from the Drizzle definitions.
- `drizzle.config.ts` – Configuration used by Drizzle Kit when generating migrations.
- `migrations/` – Generated migration files.

## Usage

Import the schema or helpers from the workspace package:

```ts
import * as schema from "@waddlesocial/waddle-service-waddle-data-model/schema";
import { WADDLE_VISIBILITY_VALUES } from "@waddlesocial/waddle-service-waddle-data-model/zod";
```