-- Add position_kind for queryable workflow position without blob deserialization.
ALTER TABLE sayiir_workflow_snapshots ADD COLUMN IF NOT EXISTS position_kind TEXT;

CREATE INDEX IF NOT EXISTS idx_snapshots_position ON sayiir_workflow_snapshots (position_kind);

-- Add delay_wake_at so dashboards can show when parked workflows will resume.
ALTER TABLE sayiir_workflow_snapshots ADD COLUMN IF NOT EXISTS delay_wake_at TIMESTAMPTZ;
