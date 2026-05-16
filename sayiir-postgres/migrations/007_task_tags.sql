-- Add task tags column to support tag-based worker affinity routing.
ALTER TABLE sayiir_workflow_snapshots
ADD COLUMN task_tags TEXT[] NOT NULL DEFAULT '{}';

CREATE INDEX idx_sayiir_task_tags
ON sayiir_workflow_snapshots USING GIN (task_tags);
