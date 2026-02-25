//! [`SnapshotStore`] implementation for Postgres.

use sayiir_core::codec::{self, Decoder, Encoder};
use sayiir_core::snapshot::{SnapshotStatus, WorkflowSnapshot};
use sayiir_persistence::{BackendError, SnapshotStore};
use sqlx::Row;

use crate::backend::PostgresBackend;
use crate::error::PgError;
use crate::helpers::{
    completed_task_count, current_task_id, delay_wake_at, error_message, is_terminal,
    position_kind, status_str,
};

impl<C> SnapshotStore for PostgresBackend<C>
where
    C: Encoder
        + Decoder
        + codec::sealed::EncodeValue<WorkflowSnapshot>
        + codec::sealed::DecodeValue<WorkflowSnapshot>,
{
    #[tracing::instrument(
        name = "db.save_snapshot",
        skip(self, snapshot),
        fields(
            db.system = "postgresql",
            instance_id = %snapshot.instance_id,
            status = %status_str(&snapshot.state),
        ),
        err(level = tracing::Level::ERROR),
    )]
    #[allow(clippy::too_many_lines)]
    async fn save_snapshot(&self, snapshot: &WorkflowSnapshot) -> Result<(), BackendError> {
        tracing::debug!("saving snapshot");
        let data = self.encode(snapshot)?;
        let status = status_str(&snapshot.state);
        let task_id = current_task_id(snapshot).map(ToString::to_string);
        let task_count = completed_task_count(snapshot);
        let error = error_message(snapshot).map(ToString::to_string);
        let terminal = is_terminal(snapshot);
        let pos_kind = position_kind(snapshot);
        let wake_at = delay_wake_at(snapshot);

        let mut tx = self.pool.begin().await.map_err(PgError)?;

        // Upsert current state
        sqlx::query(
            "INSERT INTO sayiir_workflow_snapshots
                (instance_id, status, definition_hash, current_task_id,
                 completed_task_count, data, error, position_kind, delay_wake_at,
                 trace_parent, completed_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $9, $10, $11,
                     CASE WHEN $8 THEN now() ELSE NULL END, now())
             ON CONFLICT (instance_id) DO UPDATE SET
                status = $2,
                definition_hash = $3,
                current_task_id = $4,
                completed_task_count = $5,
                data = $6,
                error = $7,
                position_kind = $9,
                delay_wake_at = $10,
                trace_parent = $11,
                completed_at = CASE WHEN $8 THEN now() ELSE sayiir_workflow_snapshots.completed_at END,
                updated_at = now()",
        )
        .bind(&snapshot.instance_id) // $1
        .bind(status) // $2
        .bind(&snapshot.definition_hash) // $3
        .bind(&task_id) // $4
        .bind(task_count) // $5
        .bind(&data) // $6
        .bind(&error) // $7
        .bind(terminal) // $8
        .bind(pos_kind) // $9
        .bind(wake_at) // $10
        .bind(snapshot.trace_parent.as_deref()) // $11
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        // Append to history
        sqlx::query(
            "INSERT INTO sayiir_workflow_snapshot_history
                (instance_id, version, status, current_task_id, data)
             VALUES (
                $1,
                (SELECT COALESCE(MAX(version), 0) + 1
                 FROM sayiir_workflow_snapshot_history WHERE instance_id = $1),
                $2, $3, $4
             )",
        )
        .bind(&snapshot.instance_id)
        .bind(status)
        .bind(&task_id)
        .bind(&data)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        // --- Maintain sayiir_workflow_tasks lifecycle ---

        // If at a task, mark it as active
        if let Some(ref tid) = task_id {
            sqlx::query(
                "INSERT INTO sayiir_workflow_tasks (instance_id, task_id, status, started_at)
                 VALUES ($1, $2, 'active', now())
                 ON CONFLICT (instance_id, task_id) DO UPDATE SET
                    status = CASE
                        WHEN sayiir_workflow_tasks.status = 'completed' THEN sayiir_workflow_tasks.status
                        ELSE 'active'
                    END,
                    started_at = COALESCE(sayiir_workflow_tasks.started_at, now())",
            )
            .bind(&snapshot.instance_id)
            .bind(tid)
            .execute(&mut *tx)
            .await
            .map_err(PgError)?;
        }

        // On terminal states, mark any still-active task as failed/cancelled
        if terminal {
            let terminal_status = match SnapshotStatus::from(&snapshot.state) {
                SnapshotStatus::Failed => "failed",
                SnapshotStatus::Cancelled => "cancelled",
                _ => "completed",
            };
            sqlx::query(
                "UPDATE sayiir_workflow_tasks SET status = $1, completed_at = now(), error = $2
                 WHERE instance_id = $3 AND status = 'active'",
            )
            .bind(terminal_status)
            .bind(&error)
            .bind(&snapshot.instance_id)
            .execute(&mut *tx)
            .await
            .map_err(PgError)?;
        }

        tx.commit().await.map_err(PgError)?;
        Ok(())
    }

    #[tracing::instrument(
        name = "db.save_task_result",
        skip(self, output),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn save_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
        output: bytes::Bytes,
    ) -> Result<(), BackendError> {
        tracing::debug!("saving task result");
        let mut tx = self.pool.begin().await.map_err(PgError)?;

        // Lock and load the snapshot
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
        snapshot.mark_task_completed(task_id.to_string(), output);

        let data = self.encode(&snapshot)?;
        let status = status_str(&snapshot.state);
        let current = current_task_id(&snapshot).map(ToString::to_string);
        let task_count = completed_task_count(&snapshot);

        sqlx::query(
            "UPDATE sayiir_workflow_snapshots
             SET data = $1, status = $2, current_task_id = $3,
                 completed_task_count = $4, updated_at = now()
             WHERE instance_id = $5",
        )
        .bind(&data)
        .bind(status)
        .bind(&current)
        .bind(task_count)
        .bind(instance_id)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        // Mark task as completed in sayiir_workflow_tasks
        sqlx::query(
            "INSERT INTO sayiir_workflow_tasks (instance_id, task_id, status, completed_at)
             VALUES ($1, $2, 'completed', now())
             ON CONFLICT (instance_id, task_id) DO UPDATE SET
                status = 'completed', completed_at = now(), error = NULL",
        )
        .bind(instance_id)
        .bind(task_id)
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        tx.commit().await.map_err(PgError)?;
        Ok(())
    }

    #[tracing::instrument(
        name = "db.load_snapshot",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        tracing::debug!("loading snapshot");
        let row = sqlx::query(
            "SELECT data, trace_parent FROM sayiir_workflow_snapshots WHERE instance_id = $1",
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?
        .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        let raw: &[u8] = row.get("data");
        let mut snapshot = self.decode(raw)?;
        snapshot.trace_parent = row.get("trace_parent");
        Ok(snapshot)
    }

    #[tracing::instrument(
        name = "db.delete_snapshot",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError> {
        tracing::debug!("deleting snapshot");
        let result = sqlx::query("DELETE FROM sayiir_workflow_snapshots WHERE instance_id = $1")
            .bind(instance_id)
            .execute(&self.pool)
            .await
            .map_err(PgError)?;

        if result.rows_affected() == 0 {
            return Err(BackendError::NotFound(instance_id.to_string()));
        }
        Ok(())
    }

    #[tracing::instrument(
        name = "db.list_snapshots",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        tracing::debug!("listing snapshots");
        let rows = sqlx::query("SELECT instance_id FROM sayiir_workflow_snapshots")
            .fetch_all(&self.pool)
            .await
            .map_err(PgError)?;

        Ok(rows.iter().map(|r| r.get("instance_id")).collect())
    }
}
