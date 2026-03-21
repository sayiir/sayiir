//! [`SnapshotStore`] implementation for Cloudflare D1.

use sayiir_core::snapshot::WorkflowSnapshot;
use sayiir_persistence::{BackendError, SnapshotStore};
use sqlx::{Executor, Row};

use crate::backend::SQLiteBackend;
use crate::helpers::dt_to_sqlite;

impl<T> SnapshotStore for SQLiteBackend<T>
where
    for<'c> &'c T: Executor<'c, Database = crate::backend::BackendDB>,
    T: Clone + Send + Sync,
{
    async fn save_snapshot(&self, snapshot: &WorkflowSnapshot) -> Result<(), BackendError> {
        let data = self.encode(snapshot)?;
        let status = snapshot.state.as_ref();
        let task_id = snapshot.current_task_id().map(ToString::to_string);
        let task_count = snapshot.completed_task_count();
        let error = snapshot.error_message().map(ToString::to_string);
        let terminal = snapshot.state.is_terminal();
        let pos_kind = snapshot.position_kind();
        let wake_at = dt_to_sqlite(snapshot.delay_wake_at());

        let now = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";
        let completed_at_expr = if terminal { now } else { "NULL" };

        let upsert_sql = format!(
            "INSERT INTO sayiir_workflow_snapshots
                (instance_id, status, definition_hash, current_task_id,
                 completed_task_count, data, error, position_kind, delay_wake_at,
                 trace_parent, completed_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10,
                     {completed_at_expr}, {now})
             ON CONFLICT (instance_id) DO UPDATE SET
                status = ?2,
                definition_hash = ?3,
                current_task_id = ?4,
                completed_task_count = ?5,
                data = ?6,
                error = ?7,
                position_kind = ?8,
                delay_wake_at = ?9,
                trace_parent = ?10,
                completed_at = CASE WHEN {terminal} THEN {now} ELSE sayiir_workflow_snapshots.completed_at END,
                updated_at = {now}",
            terminal = if terminal { "1" } else { "0" },
        );

        let exec = self.exec();
        sqlx::query(upsert_sql.as_str())
            .bind(&snapshot.instance_id) // ?1
            .bind(status) // ?2
            .bind(&snapshot.definition_hash) // ?3
            .bind(&task_id) // ?4
            .bind(i64::from(task_count)) // ?5
            .bind(&data) // ?6
            .bind(&error) // ?7
            .bind(pos_kind) // ?8
            .bind(&wake_at) // ?9
            .bind(snapshot.trace_parent.as_deref()) // ?10
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        let history_sql = "INSERT INTO sayiir_workflow_snapshot_history
                (instance_id, version, status, current_task_id, data)
             VALUES (
                ?1,
                (SELECT COALESCE(MAX(version), 0) + 1
                 FROM sayiir_workflow_snapshot_history WHERE instance_id = ?1),
                ?2, ?3, ?4
             )";

        sqlx::query(history_sql)
            .bind(&snapshot.instance_id)
            .bind(status)
            .bind(&task_id)
            .bind(&data)
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        Ok(())
    }

    async fn save_task_result(
        &self,
        instance_id: &str,
        task_id: &str,
        output: bytes::Bytes,
    ) -> Result<(), BackendError> {
        // Single-Worker: no concurrency, so sequential load → mutate → save is safe.
        let mut snapshot = self.load_snapshot(instance_id).await?;
        snapshot.mark_task_completed(task_id.to_string(), output);
        self.save_snapshot(&snapshot).await
    }

    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        let sql = "SELECT data, trace_parent FROM sayiir_workflow_snapshots WHERE instance_id = ?1";

        let exec = self.exec();
        let row = sqlx::query(sql)
            .bind(instance_id)
            .fetch_optional(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        let row = row.ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        let data: Vec<u8> = row.get("data");
        let trace_parent: Option<String> = row.get("trace_parent");

        let mut snapshot = self.decode(&data)?;
        snapshot.trace_parent = trace_parent;
        Ok(snapshot)
    }

    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError> {
        let sql = "DELETE FROM sayiir_workflow_snapshots WHERE instance_id = ?1";

        let exec = self.exec();
        let result = sqlx::query(sql)
            .bind(instance_id)
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        #[cfg(feature = "d1")]
        let rows_affected = result.rows_affected;
        #[cfg(not(feature = "d1"))]
        let rows_affected = result.rows_affected();

        if rows_affected < 1 {
            return Err(BackendError::NotFound(instance_id.to_string()));
        }
        Ok(())
    }

    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        let sql = "SELECT instance_id FROM sayiir_workflow_snapshots";

        let exec = self.exec();
        let rows = sqlx::query(sql)
            .fetch_all(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        let ids = rows.into_iter().map(|r| r.get("instance_id")).collect();
        Ok(ids)
    }
}
