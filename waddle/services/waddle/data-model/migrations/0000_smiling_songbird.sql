CREATE TABLE `waddles` (
	`slug` text PRIMARY KEY NOT NULL,
	`name` text NOT NULL,
	`visibility` text DEFAULT 'public' NOT NULL
);
