-- Prototype-only schema, never a Waddle migration.
CREATE TABLE scope_guard (
    scope UUID PRIMARY KEY,
    incarnation UUID NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    operation UUID NOT NULL,
    graph_digest TEXT NOT NULL,
    desired BOOLEAN NOT NULL,
    active BOOLEAN NOT NULL
);
CREATE TABLE admitted_effect (
    attempt UUID PRIMARY KEY,
    scope UUID NOT NULL,
    incarnation UUID NOT NULL,
    generation BIGINT NOT NULL
);
CREATE TABLE revocation (
    operation UUID PRIMARY KEY,
    scope UUID NOT NULL,
    incarnation UUID NOT NULL,
    previous_generation BIGINT NOT NULL,
    closed_generation BIGINT NOT NULL
);
