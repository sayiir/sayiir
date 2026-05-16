-- Add task priority column to support priority-based task scheduling.
-- Default 3 = Normal priority (1 = Critical, 5 = Minimal).
ALTER TABLE sayiir_workflow_snapshots
ADD COLUMN task_priority SMALLINT NOT NULL DEFAULT 3;
