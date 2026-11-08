-- This separate pass bounds untrusted catalog vectors before the shape/ACL
-- query expands them. The operator pins are independent of this connection.
WITH input AS MATERIALIZED (
  SELECT $1::pg_catalog.oid AS messages, $2::pg_catalog.oid AS sequences,
         $3::pg_catalog.oid AS schema_oid, $4::pg_catalog.oid AS owner_oid,
         $5::pg_catalog.oid AS runtime_oid, $6::pg_catalog.oid AS index_oid,
         $7::pg_catalog.text AS schema_name
)
SELECT
  (SELECT pg_catalog.count(*) FROM
     (SELECT 1 FROM pg_catalog.pg_attribute a, input p
      WHERE a.attrelid IN (p.messages,p.sequences) LIMIT 41) q) <= 40
  AND (SELECT pg_catalog.count(*) FROM
     (SELECT 1 FROM pg_catalog.pg_index i, input p
      WHERE i.indrelid IN (p.messages,p.sequences) LIMIT 12) q) = 10
  AND (SELECT pg_catalog.count(*) FROM
     (SELECT 1 FROM pg_catalog.pg_constraint c, input p
      WHERE c.conrelid IN (p.messages,p.sequences)
         OR c.confrelid IN (p.messages,p.sequences) LIMIT 4) q) = 2
  AND (SELECT pg_catalog.count(*) FROM
     (SELECT 1 FROM pg_catalog.pg_attrdef d, input p
      WHERE d.adrelid IN (p.messages,p.sequences) LIMIT 4) q) = 2
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_class c, input p
    WHERE c.oid IN (p.messages,p.sequences)
      AND ((c.relacl IS NOT NULL AND
            (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL OR pg_catalog.pg_column_size(c.relacl)>4096))
        OR (c.reloptions IS NOT NULL AND
            (pg_catalog.pg_column_compression(c.reloptions) IS NOT NULL OR pg_catalog.pg_column_size(c.reloptions)>4096)))
  )
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_attribute a, input p
    WHERE a.attrelid IN (p.messages,p.sequences)
      AND ((a.attacl IS NOT NULL AND
            (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL OR pg_catalog.pg_column_size(a.attacl)>4096))
        OR (a.attoptions IS NOT NULL AND
            (pg_catalog.pg_column_compression(a.attoptions) IS NOT NULL OR pg_catalog.pg_column_size(a.attoptions)>4096))
        OR (a.attfdwoptions IS NOT NULL AND
            (pg_catalog.pg_column_compression(a.attfdwoptions) IS NOT NULL OR pg_catalog.pg_column_size(a.attfdwoptions)>4096)))
  )
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_attrdef d, input p
    WHERE d.adrelid IN (p.messages,p.sequences)
      AND (pg_catalog.pg_column_compression(d.adbin) IS NOT NULL OR pg_catalog.pg_column_size(d.adbin)>4096)
  )
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_constraint c, input p
    WHERE c.conrelid IN (p.messages,p.sequences)
      AND ((c.conkey IS NOT NULL AND
            (pg_catalog.pg_column_compression(c.conkey) IS NOT NULL OR pg_catalog.pg_column_size(c.conkey)>4096))
        OR (c.conbin IS NOT NULL AND
            (pg_catalog.pg_column_compression(c.conbin) IS NOT NULL OR pg_catalog.pg_column_size(c.conbin)>4096)))
  )
  AND NOT EXISTS (
    SELECT 1 FROM pg_catalog.pg_index i
    JOIN pg_catalog.pg_class c ON c.oid=i.indexrelid, input p
    WHERE i.indrelid IN (p.messages,p.sequences)
      AND (i.indnatts>3 OR i.indnkeyatts>3
        OR pg_catalog.pg_column_compression(i.indkey) IS NOT NULL OR pg_catalog.pg_column_size(i.indkey)>4096
        OR pg_catalog.pg_column_compression(i.indclass) IS NOT NULL OR pg_catalog.pg_column_size(i.indclass)>4096
        OR pg_catalog.pg_column_compression(i.indcollation) IS NOT NULL OR pg_catalog.pg_column_size(i.indcollation)>4096
        OR pg_catalog.pg_column_compression(i.indoption) IS NOT NULL OR pg_catalog.pg_column_size(i.indoption)>4096
        OR (c.reloptions IS NOT NULL AND
            (pg_catalog.pg_column_compression(c.reloptions) IS NOT NULL OR pg_catalog.pg_column_size(c.reloptions)>4096)))
  )
