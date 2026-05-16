-- Add task priority column for forward-compatibility with priority-based scheduling.
-- Default 3 = Normal priority (1 = Critical, 5 = Minimal).
ALTER TABLE sayiir_workflow_snapshots ADD COLUMN task_priority INTEGER NOT NULL DEFAULT 3;
