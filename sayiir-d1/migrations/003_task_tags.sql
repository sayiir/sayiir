-- Reserve task_tags column for forward-compatibility with the Postgres schema.
-- This column is NOT maintained by the D1 snapshot store — it always keeps
-- its default value. The authoritative tags live inside the serialised
-- snapshot blob (`data`). Worker affinity filtering is not supported on D1.
ALTER TABLE sayiir_workflow_snapshots
ADD COLUMN task_tags TEXT NOT NULL DEFAULT '[]';
