-- External events (signals) buffered per (instance_id, signal_name) in FIFO order.
CREATE TABLE IF NOT EXISTS sayiir_workflow_events (
    id           BIGSERIAL   PRIMARY KEY,
    instance_id  TEXT        NOT NULL,
    signal_name  TEXT        NOT NULL,
    payload      BYTEA       NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_events_instance_signal
    ON sayiir_workflow_events (instance_id, signal_name, id);
