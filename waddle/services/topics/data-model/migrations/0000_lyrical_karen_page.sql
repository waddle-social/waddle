CREATE TABLE `topics` (
	`id` text PRIMARY KEY NOT NULL,
	`title` text NOT NULL,
	`description` text,
	`scope` text NOT NULL,
	`owner_id` text,
	`waddle_slug` text,
	`created_at` integer DEFAULT (strftime('%s', 'now') * 1000) NOT NULL,
	`updated_at` integer DEFAULT (strftime('%s', 'now') * 1000) NOT NULL,
	CONSTRAINT "topics_owner_scope_check" CHECK("topics"."scope" != 'owner' OR ("topics"."owner_id" IS NOT NULL AND "topics"."waddle_slug" IS NULL)),
	CONSTRAINT "topics_waddle_scope_check" CHECK("topics"."scope" != 'waddle' OR ("topics"."waddle_slug" IS NOT NULL AND "topics"."owner_id" IS NULL)),
	CONSTRAINT "topics_global_scope_check" CHECK("topics"."scope" != 'global' OR ("topics"."owner_id" IS NULL AND "topics"."waddle_slug" IS NULL))
);
--> statement-breakpoint
CREATE UNIQUE INDEX `topics_owner_title_unique` ON `topics` (`owner_id`,`title`) WHERE "topics"."scope" = 'owner';--> statement-breakpoint
CREATE UNIQUE INDEX `topics_waddle_title_unique` ON `topics` (`waddle_slug`,`title`) WHERE "topics"."scope" = 'waddle';