//! [`SignalStore`] implementation for Postgres.
//!
//! Overrides the 3 default composite methods with single-transaction
//! implementations for true ACID atomicity.

use sayiir_core::codec::{self, Decoder, Encoder};
use sayiir_core::snapshot::{
    PauseRequest, SignalKind, SignalRequest, SnapshotStatus, WorkflowSnapshot,
};
use sayiir_persistence::{BackendError, SignalStore};
use sqlx::Row;

use crate::backend::PostgresBackend;
use crate::error::PgError;
use crate::helpers::{
    completed_task_count, current_task_id, delay_wake_at, error_message, position_kind, status_str,
};

impl<C> SignalStore for PostgresBackend<C>
where
    C: Encoder
        + Decoder
        + codec::sealed::EncodeValue<WorkflowSnapshot>
        + codec::sealed::DecodeValue<WorkflowSnapshot>,
{
    async fn store_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
        request: SignalRequest,
    ) -> Result<(), BackendError> {
        tracing::debug!(instance_id, kind = %kind.as_ref(), "storing signal");
        // Validate workflow state first
        let row =
            sqlx::query("SELECT status FROM sayiir_workflow_snapshots WHERE instance_id = $1")
                .bind(instance_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(PgError)?
                .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        let status: String = row.get("status");
        validate_signal_allowed(&status, kind)?;

        sqlx::query(
            "INSERT INTO sayiir_workflow_signals (instance_id, kind, reason, requested_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (instance_id, kind) DO UPDATE SET
                reason = $3, requested_by = $4, created_at = now()",
        )
        .bind(instance_id)
        .bind(kind.as_ref())
        .bind(&request.reason)
        .bind(&request.requested_by)
        .execute(&self.pool)
        .await
        .map_err(PgError)?;

        Ok(())
    }

    async fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> Result<Option<SignalRequest>, BackendError> {
        tracing::debug!(instance_id, kind = %kind.as_ref(), "getting signal");
        let row = sqlx::query(
            "SELECT reason, requested_by, created_at
             FROM sayiir_workflow_signals
             WHERE instance_id = $1 AND kind = $2",
        )
        .bind(instance_id)
        .bind(kind.as_ref())
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?;

        Ok(row.map(|r| SignalRequest {
            reason: r.get("reason"),
            requested_by: r.get("requested_by"),
            requested_at: r.get("created_at"),
        }))
    }

    async fn clear_signal(&self, instance_id: &str, kind: SignalKind) -> Result<(), BackendError> {
        tracing::debug!(instance_id, kind = %kind.as_ref(), "clearing signal");
        sqlx::query("DELETE FROM sayiir_workflow_signals WHERE instance_id = $1 AND kind = $2")
            .bind(instance_id)
            .bind(kind.as_ref())
            .execute(&self.pool)
            .await
            .map_err(PgError)?;
        Ok(())
    }

    async fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: bytes::Bytes,
    ) -> Result<(), BackendError> {
        tracing::debug!(instance_id, signal_name, "buffering external event");
        sqlx::query(
            "INSERT INTO sayiir_workflow_events (instance_id, signal_name, payload)
             VALUES ($1, $2, $3)",
        )
        .bind(instance_id)
        .bind(signal_name)
        .bind(payload.as_ref())
        .execute(&self.pool)
        .await
        .map_err(PgError)?;
        Ok(())
    }

    async fn consume_event(
        &self,
        instance_id: &str,
        signal_name: &str,
    ) -> Result<Option<bytes::Bytes>, BackendError> {
        tracing::debug!(instance_id, signal_name, "consuming oldest buffered event");
        // Atomically delete-and-return the oldest event for this (instance, signal).
        let row = sqlx::query(
            "DELETE FROM sayiir_workflow_events
             WHERE id = (
                 SELECT id FROM sayiir_workflow_events
                 WHERE instance_id = $1 AND signal_name = $2
                 ORDER BY id ASC
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING payload",
        )
        .bind(instance_id)
        .bind(signal_name)
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?;

        Ok(row.map(|r| {
            let raw: Vec<u8> = r.get("payload");
            bytes::Bytes::from(raw)
        }))
    }

    // --- Overridden composites: single ACID transactions ---

    async fn check_and_cancel(
        &self,
        instance_id: &str,
        interrupted_at_task: Option<&str>,
    ) -> Result<bool, BackendError> {
        tracing::debug!(instance_id, "checking for cancel signal");
        let mut tx = self.pool.begin().await.map_err(PgError)?;

        // Check for cancel signal (lock the row)
        let signal_row = sqlx::query(
            "SELECT reason, requested_by
             FROM sayiir_workflow_signals
             WHERE instance_id = $1 AND kind = $2
             FOR UPDATE",
        )
        .bind(instance_id)
        .bind(SignalKind::Cancel.as_ref())
        .fetch_optional(&mut *tx)
        .await
        .map_err(PgError)?;

        let Some(signal_row) = signal_row else {
            tx.rollback().await.map_err(PgError)?;
            return Ok(false);
        };

        // Lock and load the snapshot
        let snap_row = sqlx::query(
            "SELECT data FROM sayiir_workflow_snapshots WHERE instance_id = $1 FOR UPDATE",
        )
        .bind(instance_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(PgError)?;

        let raw: &[u8] = snap_row.get("data");
        let mut snapshot = self.decode(raw)?;

        if !snapshot.state.is_in_progress() {
            tx.rollback().await.map_err(PgError)?;
            return Ok(false);
        }

        let reason: Option<String> = signal_row.get("reason");
        let requested_by: Option<String> = signal_row.get("requested_by");
        snapshot.mark_cancelled(reason, requested_by, interrupted_at_task.map(String::from));

        let data = self.encode(&snapshot)?;
        let status = status_str(&snapshot.state);
        let error = error_message(&snapshot).map(ToString::to_string);
        let pos_kind = position_kind(&snapshot);
        let wake_at = delay_wake_at(&snapshot);

        sqlx::query(
            "UPDATE sayiir_workflow_snapshots
             SET data = $1, status = $2, error = $3,
                 position_kind = $4, delay_wake_at = $5,
                 completed_at = now(), updated_at = now()
             WHERE instance_id = $6",
        )
        .bind(&data)
        .bind(status)
        .bind(&error)
        .bind(pos_kind)
        .bind(wake_at)
        .bind(instance_id)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        // Mark any still-active tasks as cancelled
        sqlx::query(
            "UPDATE sayiir_workflow_tasks SET status = 'cancelled', completed_at = now()
             WHERE instance_id = $1 AND status = 'active'",
        )
        .bind(instance_id)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        // Clear the signal
        sqlx::query("DELETE FROM sayiir_workflow_signals WHERE instance_id = $1 AND kind = $2")
            .bind(instance_id)
            .bind(SignalKind::Cancel.as_ref())
            .execute(&mut *tx)
            .await
            .map_err(PgError)?;

        tx.commit().await.map_err(PgError)?;
        tracing::info!(instance_id, "workflow cancelled");
        Ok(true)
    }

    async fn check_and_pause(&self, instance_id: &str) -> Result<bool, BackendError> {
        tracing::debug!(instance_id, "checking for pause signal");
        let mut tx = self.pool.begin().await.map_err(PgError)?;

        // Check for pause signal (lock the row)
        let signal_row = sqlx::query(
            "SELECT reason, requested_by
             FROM sayiir_workflow_signals
             WHERE instance_id = $1 AND kind = $2
             FOR UPDATE",
        )
        .bind(instance_id)
        .bind(SignalKind::Pause.as_ref())
        .fetch_optional(&mut *tx)
        .await
        .map_err(PgError)?;

        let Some(signal_row) = signal_row else {
            tx.rollback().await.map_err(PgError)?;
            return Ok(false);
        };

        // Lock and load the snapshot
        let snap_row = sqlx::query(
            "SELECT data FROM sayiir_workflow_snapshots WHERE instance_id = $1 FOR UPDATE",
        )
        .bind(instance_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(PgError)?;

        let raw: &[u8] = snap_row.get("data");
        let mut snapshot = self.decode(raw)?;

        if !snapshot.state.is_in_progress() {
            tx.rollback().await.map_err(PgError)?;
            return Ok(false);
        }

        let reason: Option<String> = signal_row.get("reason");
        let requested_by: Option<String> = signal_row.get("requested_by");
        let pause_request = PauseRequest::new(reason, requested_by);
        snapshot.mark_paused(&pause_request);

        let data = self.encode(&snapshot)?;
        let status = status_str(&snapshot.state);
        let task_id = current_task_id(&snapshot).map(ToString::to_string);
        let task_count = completed_task_count(&snapshot);
        let pos_kind = position_kind(&snapshot);
        let wake_at = delay_wake_at(&snapshot);

        sqlx::query(
            "UPDATE sayiir_workflow_snapshots
             SET data = $1, status = $2, current_task_id = $3,
                 completed_task_count = $4, position_kind = $5,
                 delay_wake_at = $6, updated_at = now()
             WHERE instance_id = $7",
        )
        .bind(&data)
        .bind(status)
        .bind(&task_id)
        .bind(task_count)
        .bind(pos_kind)
        .bind(wake_at)
        .bind(instance_id)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        // Clear the signal
        sqlx::query("DELETE FROM sayiir_workflow_signals WHERE instance_id = $1 AND kind = $2")
            .bind(instance_id)
            .bind(SignalKind::Pause.as_ref())
            .execute(&mut *tx)
            .await
            .map_err(PgError)?;

        tx.commit().await.map_err(PgError)?;
        tracing::info!(instance_id, "workflow paused");
        Ok(true)
    }

    async fn unpause(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        tracing::debug!(instance_id, "unpausing workflow");
        let mut tx = self.pool.begin().await.map_err(PgError)?;

        let row = sqlx::query(
            "SELECT data FROM sayiir_workflow_snapshots WHERE instance_id = $1 FOR UPDATE",
        )
        .bind(instance_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(PgError)?
        .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        let raw: &[u8] = row.get("data");
        let mut snapshot = self.decode(raw)?;

        if !snapshot.state.is_paused() {
            let state_name = status_str(&snapshot.state);
            return Err(BackendError::CannotPause(format!(
                "Workflow is not paused (current state: {state_name:?})"
            )));
        }

        snapshot.mark_unpaused();

        let data = self.encode(&snapshot)?;
        let status = status_str(&snapshot.state);
        let task_id = current_task_id(&snapshot).map(ToString::to_string);
        let task_count = completed_task_count(&snapshot);
        let pos_kind = position_kind(&snapshot);
        let wake_at = delay_wake_at(&snapshot);

        sqlx::query(
            "UPDATE sayiir_workflow_snapshots
             SET data = $1, status = $2, current_task_id = $3,
                 completed_task_count = $4, position_kind = $5,
                 delay_wake_at = $6, updated_at = now()
             WHERE instance_id = $7",
        )
        .bind(&data)
        .bind(status)
        .bind(&task_id)
        .bind(task_count)
        .bind(pos_kind)
        .bind(wake_at)
        .bind(instance_id)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        tx.commit().await.map_err(PgError)?;
        tracing::info!(instance_id, "workflow unpaused");
        Ok(snapshot)
    }
}

/// Validate that a signal can be sent to a workflow in the given state.
fn validate_signal_allowed(status: &str, kind: SignalKind) -> Result<(), BackendError> {
    use std::str::FromStr;

    let Ok(status) = SnapshotStatus::from_str(status) else {
        // Unknown status from DB — be permissive (forward compatibility).
        return Ok(());
    };

    match kind {
        SignalKind::Cancel => match status {
            SnapshotStatus::Completed | SnapshotStatus::Failed => {
                Err(BackendError::CannotCancel(status.as_ref().to_string()))
            }
            _ => Ok(()),
        },
        SignalKind::Pause => match status {
            SnapshotStatus::Completed | SnapshotStatus::Failed | SnapshotStatus::Cancelled => {
                Err(BackendError::CannotPause(status.as_ref().to_string()))
            }
            _ => Ok(()),
        },
    }
}
