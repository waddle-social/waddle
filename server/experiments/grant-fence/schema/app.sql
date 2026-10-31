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

-- Second-stage leaf recovery model; still prototype-only, never a migration.
CREATE TABLE recovery_guard (
    candidate TEXT NOT NULL CHECK (candidate IN ('postgres','spicedb')),
    scope UUID NOT NULL,
    incarnation UUID NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    operation UUID NOT NULL,
    digest TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    PRIMARY KEY(candidate, scope)
);
CREATE TABLE recovery_intent (
    candidate TEXT NOT NULL CHECK (candidate = 'postgres'),
    scope UUID NOT NULL,
    incarnation UUID NOT NULL,
    generation BIGINT NOT NULL,
    operation UUID NOT NULL,
    digest TEXT NOT NULL,
    subject UUID NOT NULL,
    relation TEXT NOT NULL CHECK (relation = 'reader'),
    polarity TEXT NOT NULL CHECK (polarity IN ('grant','revoke')),
    PRIMARY KEY(candidate, scope, incarnation, generation, operation, digest)
);
CREATE TABLE recovery_fact (
    candidate TEXT NOT NULL,
    scope UUID NOT NULL,
    incarnation UUID NOT NULL,
    generation BIGINT NOT NULL,
    operation UUID NOT NULL,
    digest TEXT NOT NULL,
    fact TEXT NOT NULL CHECK (fact IN ('admission_closed','canonical_committed','projection_verified')),
    PRIMARY KEY(candidate, scope, incarnation, generation, operation, digest, fact)
);
CREATE TABLE recovery_admission (
    attempt UUID PRIMARY KEY,
    candidate TEXT NOT NULL,
    scope UUID NOT NULL,
    incarnation UUID NOT NULL,
    generation BIGINT NOT NULL,
    operation UUID NOT NULL,
    digest TEXT NOT NULL
);

-- Finite synthetic multi-edge experiment, not a production policy or migration.
CREATE TABLE multi_guard (
    kind SMALLINT NOT NULL CHECK (kind BETWEEN 0 AND 3),
    id UUID NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    operation UUID NOT NULL,
    payload TEXT NOT NULL,
    active BOOLEAN NOT NULL,
    PRIMARY KEY(kind,id)
);
CREATE TABLE multi_effect (
    effect UUID PRIMARY KEY,
    descriptor TEXT NOT NULL
);
