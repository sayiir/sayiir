-- Workflow snapshots: current state of each workflow instance (1:1 with instance).
CREATE TABLE IF NOT EXISTS sayiir_workflow_snapshots (
    instance_id          TEXT        PRIMARY KEY,
    status               TEXT        NOT NULL,
    definition_hash      TEXT,
    current_task_id      TEXT,
    completed_task_count INT         NOT NULL DEFAULT 0,
    data                 BYTEA       NOT NULL,
    error                TEXT,
    started_at           TIMESTAMPTZ NOT NULL DEFAULT now(),
    completed_at         TIMESTAMPTZ,
    updated_at           TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_snapshots_status  ON sayiir_workflow_snapshots (status);
CREATE INDEX IF NOT EXISTS idx_snapshots_task    ON sayiir_workflow_snapshots (current_task_id);
CREATE INDEX IF NOT EXISTS idx_snapshots_updated ON sayiir_workflow_snapshots (updated_at);

-- Snapshot history: append-only log of every checkpoint (1:N per instance).
CREATE TABLE IF NOT EXISTS sayiir_workflow_snapshot_history (
    id              BIGSERIAL   PRIMARY KEY,
    instance_id     TEXT        NOT NULL,
    version         INT         NOT NULL,
    status          TEXT        NOT NULL,
    current_task_id TEXT,
    data            BYTEA       NOT NULL,
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_history_instance ON sayiir_workflow_snapshot_history (instance_id, version);

-- Individual task states for observability (1:N per instance).
CREATE TABLE IF NOT EXISTS sayiir_workflow_tasks (
    instance_id  TEXT        NOT NULL,
    task_id      TEXT        NOT NULL,
    status       TEXT        NOT NULL DEFAULT 'pending',
    worker_id    TEXT,
    started_at   TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error        TEXT,
    PRIMARY KEY (instance_id, task_id)
);

CREATE INDEX IF NOT EXISTS idx_tasks_status ON sayiir_workflow_tasks (status);

-- Cancel/pause signals (at most one per kind per instance).
CREATE TABLE IF NOT EXISTS sayiir_workflow_signals (
    instance_id  TEXT        NOT NULL,
    kind         TEXT        NOT NULL,
    reason       TEXT,
    requested_by TEXT,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (instance_id, kind)
);

-- Distributed worker task claims (one active claim per task).
CREATE TABLE IF NOT EXISTS sayiir_task_claims (
    instance_id TEXT        NOT NULL,
    task_id     TEXT        NOT NULL,
    worker_id   TEXT        NOT NULL,
    claimed_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at  TIMESTAMPTZ,
    PRIMARY KEY (instance_id, task_id)
);

CREATE INDEX IF NOT EXISTS idx_claims_expires ON sayiir_task_claims (expires_at);
CREATE INDEX IF NOT EXISTS idx_claims_worker  ON sayiir_task_claims (worker_id);
