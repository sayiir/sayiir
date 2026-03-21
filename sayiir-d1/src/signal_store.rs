//! [`SignalStore`] implementation for Cloudflare D1.
//!
//! Implements the 5 primitive methods. Composite methods (`check_and_cancel`,
//! `check_and_pause`, `unpause`) use the default implementations from
//! `sayiir-persistence` — safe because a single Cloudflare Worker has no
//! concurrent access to the same D1 instance.

use sayiir_core::snapshot::{SignalKind, SignalRequest};
use sayiir_persistence::{BackendError, SignalStore};
use sqlx::{Executor, Row};

use crate::backend::SQLiteBackend;
use sayiir_persistence::validation::validate_signal_allowed;

impl<T> SignalStore for SQLiteBackend<T>
where
    for<'c> &'c T: Executor<'c, Database = crate::backend::BackendDB>,
    T: Clone + Send + Sync,
{
    async fn store_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
        request: SignalRequest,
    ) -> Result<(), BackendError> {
        // Validate workflow state first.
        let status_sql = "SELECT status FROM sayiir_workflow_snapshots WHERE instance_id = ?1";

        let exec = self.exec();
        let row = sqlx::query(status_sql)
            .bind(instance_id)
            .fetch_optional(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        let row = row.ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;
        let status: String = row.get("status");
        validate_signal_allowed(&status, kind)?;

        // Upsert the signal.
        let sql = "INSERT INTO sayiir_workflow_signals (instance_id, kind, reason, requested_by)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (instance_id, kind) DO UPDATE SET
                reason = ?3, requested_by = ?4,
                created_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')";

        sqlx::query(sql)
            .bind(instance_id)
            .bind(kind.as_ref())
            .bind(&request.reason)
            .bind(&request.requested_by)
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        Ok(())
    }

    async fn get_signal(
        &self,
        instance_id: &str,
        kind: SignalKind,
    ) -> Result<Option<SignalRequest>, BackendError> {
        let sql = "SELECT reason, requested_by, created_at
             FROM sayiir_workflow_signals
             WHERE instance_id = ?1 AND kind = ?2";

        let exec = self.exec();
        let row = sqlx::query(sql)
            .bind(instance_id)
            .bind(kind.as_ref())
            .fetch_optional(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let reason: Option<String> = row.get("reason");
        let requested_by: Option<String> = row.get("requested_by");
        let created_at_str: String = row.get("created_at");

        let requested_at = chrono::DateTime::parse_from_rfc3339(&created_at_str)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|e| {
                BackendError::Backend(format!(
                    "invalid created_at timestamp {created_at_str:?}: {e}"
                ))
            })?;

        Ok(Some(SignalRequest {
            reason,
            requested_by,
            requested_at,
        }))
    }

    async fn clear_signal(&self, instance_id: &str, kind: SignalKind) -> Result<(), BackendError> {
        let sql = "DELETE FROM sayiir_workflow_signals WHERE instance_id = ?1 AND kind = ?2";

        let exec = self.exec();
        sqlx::query(sql)
            .bind(instance_id)
            .bind(kind.as_ref())
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        Ok(())
    }

    async fn send_event(
        &self,
        instance_id: &str,
        signal_name: &str,
        payload: bytes::Bytes,
    ) -> Result<(), BackendError> {
        let sql = "INSERT INTO sayiir_workflow_events (instance_id, signal_name, payload)
             VALUES (?1, ?2, ?3)";

        let exec = self.exec();
        sqlx::query(sql)
            .bind(instance_id)
            .bind(signal_name)
            .bind(payload.as_ref())
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        Ok(())
    }

    async fn consume_event(
        &self,
        instance_id: &str,
        signal_name: &str,
    ) -> Result<Option<bytes::Bytes>, BackendError> {
        // single-Worker access means we can safely do SELECT then DELETE.

        let exec = self.exec();

        // 1. Find the oldest event.
        let select_sql = "SELECT id, payload FROM sayiir_workflow_events
             WHERE instance_id = ?1 AND signal_name = ?2
             ORDER BY id ASC LIMIT 1";

        let row = sqlx::query(select_sql)
            .bind(instance_id)
            .bind(signal_name)
            .fetch_optional(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        let Some(row) = row else {
            return Ok(None);
        };

        let id: i64 = row.get("id");
        let payload: Vec<u8> = row.get("payload");

        let delete_sql = "DELETE FROM sayiir_workflow_events WHERE id = ?1";
        sqlx::query(delete_sql)
            .bind(id)
            .execute(&exec)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;

        Ok(Some(bytes::Bytes::from(payload)))
    }
}
