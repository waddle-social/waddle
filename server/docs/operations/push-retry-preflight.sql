-- Read-only PostgreSQL inventory. Run with psql -X -v ON_ERROR_STOP=1 -f this-file.sql.
-- Results containing owner/node/job identities are operator-only; do not copy to metrics.
-- No payload, provider credential or raw error text is selected.
-- Detail lists are bounded; aggregate counts are authoritative for this snapshot.
\set ON_ERROR_STOP on
BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY;
SET LOCAL statement_timeout = '15s';
SET LOCAL lock_timeout = '2s';
SET LOCAL idle_in_transaction_session_timeout = '60s';

SELECT transaction_timestamp() AS snapshot_at,
       current_database() AS database_name,
       current_setting('transaction_read_only') AS read_only;

-- Discovery precedes queries; optional lineage/Foundation tables may be absent.
SELECT table_name, column_name, data_type
FROM information_schema.columns
WHERE table_schema = current_schema()
  AND table_name IN ('push_publish_jobs', 'notification_outbox',
                     'notification_outbox_lineage', 'ingress_effect_descendants')
ORDER BY table_name, ordinal_position;

SELECT to_regclass('push_publish_jobs') IS NOT NULL AS has_provider,
       to_regclass('notification_outbox') IS NOT NULL AS has_outbox,
       to_regclass('notification_outbox_lineage') IS NOT NULL AS has_lineage,
       to_regclass('ingress_effect_descendants') IS NOT NULL AS has_foundation
\gset

\if :has_provider
-- JSON field access tolerates optional columns on older deployments. NULL
-- uncertainty is reported separately, never interpreted as known nonacceptance.
WITH jobs AS (
    SELECT p.*, to_jsonb(p) AS record FROM push_publish_jobs AS p
)
SELECT count(*) AS provider_retained,
       count(*) FILTER (WHERE status = 'queued') AS queued,
       count(*) FILTER (WHERE status = 'in-progress') AS in_progress,
       count(*) FILTER (WHERE status = 'queued' AND attempt_count >= 24) AS queued_at_budget,
       count(*) FILTER (WHERE status = 'in-progress' AND attempt_count >= 24) AS in_progress_at_budget,
       count(*) FILTER (WHERE status = 'failed' AND record->>'uncertain_send' = '1'
                        AND record->>'terminal_disposition' IS NULL) AS parked_uncertain,
       count(*) FILTER (WHERE status = 'failed' AND record->>'uncertain_send' = '0') AS failed_known,
       count(*) FILTER (WHERE record->>'uncertain_send' IS NULL) AS uncertainty_marker_missing
FROM jobs;

WITH per_node AS (
    SELECT node, count(*) AS retained FROM push_publish_jobs GROUP BY node
)
SELECT coalesce(max(retained), 0) AS retained_max_per_node,
       count(*) FILTER (WHERE retained >= 9000) AS nodes_near_capacity,
       count(*) FILTER (WHERE retained >= 10000) AS nodes_at_capacity
FROM per_node;

-- Headroom includes parked jobs and terminal retention tails. Identity output
-- is limited to the 100 fullest nodes, independently of the aggregate above.
SELECT owner_bare_jid, node, count(*) AS retained,
       greatest(10000 - count(*), 0) AS admission_headroom,
       count(*) FILTER (WHERE status = 'failed' AND to_jsonb(p)->>'uncertain_send' = '1'
                        AND to_jsonb(p)->>'terminal_disposition' IS NULL) AS parked_uncertain
FROM push_publish_jobs AS p
GROUP BY owner_bare_jid, node
ORDER BY retained DESC, node
LIMIT 100;

-- Exact operator identities: oldest 200 parked or budget-exhausted jobs.
SELECT job_id, owner_bare_jid, node, status, attempt_count,
       to_jsonb(p)->>'uncertain_send' AS uncertain_send,
       to_jsonb(p)->>'terminal_disposition' AS terminal_disposition,
       created_at_ms, updated_at_ms
FROM push_publish_jobs AS p
WHERE (status IN ('queued', 'in-progress') AND attempt_count >= 24)
   OR (status = 'failed' AND to_jsonb(p)->>'uncertain_send' = '1'
       AND to_jsonb(p)->>'terminal_disposition' IS NULL)
ORDER BY created_at_ms, job_id
LIMIT 200;

\if :has_lineage
SELECT count(*) AS provider_failed_known_with_unsettled_lineage
FROM push_publish_jobs AS p
WHERE p.status = 'failed' AND to_jsonb(p)->>'uncertain_send' = '0'
  AND EXISTS (
      SELECT 1 FROM notification_outbox_lineage AS l
      WHERE l.job_id = to_jsonb(p)->>'ancestry_job_id' AND l.settled_at_ms IS NULL
  );
SELECT 'lineage' AS custody_source, p.job_id, p.owner_bare_jid, p.node,
       p.status, p.attempt_count, p.created_at_ms, p.updated_at_ms
FROM push_publish_jobs AS p
WHERE p.status = 'failed' AND to_jsonb(p)->>'uncertain_send' = '0'
  AND EXISTS (
      SELECT 1 FROM notification_outbox_lineage AS l
      WHERE l.job_id = to_jsonb(p)->>'ancestry_job_id' AND l.settled_at_ms IS NULL
  )
ORDER BY p.created_at_ms, p.job_id
LIMIT 200;
\endif
\if :has_foundation
SELECT count(*) AS provider_failed_known_with_unsettled_foundation
FROM push_publish_jobs AS p
WHERE p.status = 'failed' AND to_jsonb(p)->>'uncertain_send' = '0'
  AND EXISTS (
      SELECT 1 FROM ingress_effect_descendants AS d
      WHERE d.descendant_key::text = to_jsonb(p)->>'ancestry_job_id' AND d.settled_at IS NULL
  );
SELECT 'foundation' AS custody_source, p.job_id, p.owner_bare_jid, p.node,
       p.status, p.attempt_count, p.created_at_ms, p.updated_at_ms
FROM push_publish_jobs AS p
WHERE p.status = 'failed' AND to_jsonb(p)->>'uncertain_send' = '0'
  AND EXISTS (
      SELECT 1 FROM ingress_effect_descendants AS d
      WHERE d.descendant_key::text = to_jsonb(p)->>'ancestry_job_id' AND d.settled_at IS NULL
  )
ORDER BY p.created_at_ms, p.job_id
LIMIT 200;
\endif
\endif

\if :has_outbox
WITH jobs AS (
    SELECT o.*, to_jsonb(o) AS record,
           attempt_count + coalesce((to_jsonb(o)->>'policy_error_count')::bigint, 0) AS failures
    FROM notification_outbox AS o
)
SELECT count(*) AS outbox_retained,
       count(*) FILTER (WHERE status = 'queued') AS queued,
       count(*) FILTER (WHERE status = 'in-progress') AS in_progress,
       count(*) FILTER (WHERE status = 'queued' AND failures >= 5) AS queued_at_budget,
       count(*) FILTER (WHERE status = 'in-progress' AND failures >= 5) AS in_progress_at_budget,
       count(*) FILTER (WHERE status IN ('queued', 'in-progress') AND failures >= 5
                        AND record->>'queue_acceptance_may_exist' = '1') AS uncertain_at_budget,
       count(*) FILTER (WHERE status = 'failed'
                        AND record->>'queue_acceptance_may_exist' = '1') AS parked_uncertain,
       count(*) FILTER (WHERE status = 'failed'
                        AND record->>'queue_acceptance_may_exist' = '0') AS failed_known,
       count(*) FILTER (WHERE record->>'queue_acceptance_may_exist' IS NULL) AS acceptance_marker_missing
FROM jobs;

SELECT job_id, recipient_bare_jid AS owner_bare_jid, node, status, attempt_count,
       coalesce((to_jsonb(o)->>'policy_error_count')::bigint, 0) AS policy_error_count,
       to_jsonb(o)->>'queue_acceptance_may_exist' AS queue_acceptance_may_exist,
       created_at_ms, updated_at_ms
FROM notification_outbox AS o
WHERE (status IN ('queued', 'in-progress')
       AND attempt_count + coalesce((to_jsonb(o)->>'policy_error_count')::bigint, 0) >= 5)
   OR (status = 'failed' AND to_jsonb(o)->>'queue_acceptance_may_exist' = '1')
ORDER BY created_at_ms, job_id
LIMIT 200;

\if :has_lineage
SELECT count(*) AS outbox_failed_known_with_unsettled_lineage
FROM notification_outbox AS o
WHERE o.status = 'failed' AND to_jsonb(o)->>'queue_acceptance_may_exist' = '0'
  AND EXISTS (
      SELECT 1 FROM notification_outbox_lineage AS l
      WHERE l.job_id = o.job_id AND l.settled_at_ms IS NULL
  );
SELECT 'lineage' AS custody_source, o.job_id,
       o.recipient_bare_jid AS owner_bare_jid, o.node,
       o.status, o.attempt_count, o.created_at_ms, o.updated_at_ms
FROM notification_outbox AS o
WHERE o.status = 'failed' AND to_jsonb(o)->>'queue_acceptance_may_exist' = '0'
  AND EXISTS (
      SELECT 1 FROM notification_outbox_lineage AS l
      WHERE l.job_id = o.job_id AND l.settled_at_ms IS NULL
  )
ORDER BY o.created_at_ms, o.job_id
LIMIT 200;
\endif
\if :has_foundation
SELECT count(*) AS outbox_failed_known_with_unsettled_foundation
FROM notification_outbox AS o
WHERE o.status = 'failed' AND to_jsonb(o)->>'queue_acceptance_may_exist' = '0'
  AND EXISTS (
      SELECT 1 FROM ingress_effect_descendants AS d
      WHERE d.descendant_key::text = o.job_id AND d.settled_at IS NULL
  );
SELECT 'foundation' AS custody_source, o.job_id,
       o.recipient_bare_jid AS owner_bare_jid, o.node,
       o.status, o.attempt_count, o.created_at_ms, o.updated_at_ms
FROM notification_outbox AS o
WHERE o.status = 'failed' AND to_jsonb(o)->>'queue_acceptance_may_exist' = '0'
  AND EXISTS (
      SELECT 1 FROM ingress_effect_descendants AS d
      WHERE d.descendant_key::text = o.job_id AND d.settled_at IS NULL
  )
ORDER BY o.created_at_ms, o.job_id
LIMIT 200;
\endif
\endif

-- Lineage and Foundation counts can overlap; do not add them together.
COMMIT;
