//! [`TaskClaimStore`] implementation for Postgres.

use chrono::{Duration, Utc};
use sayiir_core::codec::{self, Decoder, Encoder};
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};
use sayiir_persistence::{BackendError, SnapshotStore, TaskClaimStore};
use sqlx::Row;

use crate::backend::PostgresBackend;
use crate::error::PgError;

impl<C> TaskClaimStore for PostgresBackend<C>
where
    C: Encoder
        + Decoder
        + codec::sealed::EncodeValue<WorkflowSnapshot>
        + codec::sealed::DecodeValue<WorkflowSnapshot>,
{
    async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> Result<Option<TaskClaim>, BackendError> {
        let expires_at = ttl.and_then(|d| Utc::now().checked_add_signed(d));

        // Insert claim; on conflict only replace if the existing claim has expired.
        let row = sqlx::query(
            "INSERT INTO task_claims (instance_id, task_id, worker_id, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (instance_id, task_id) DO UPDATE
                SET worker_id = $3, claimed_at = now(), expires_at = $4
                WHERE task_claims.expires_at IS NOT NULL AND task_claims.expires_at < now()
             RETURNING instance_id, task_id, worker_id,
                       EXTRACT(EPOCH FROM claimed_at)::BIGINT AS claimed_epoch,
                       EXTRACT(EPOCH FROM expires_at)::BIGINT AS expires_epoch",
        )
        .bind(instance_id)
        .bind(task_id)
        .bind(worker_id)
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?;

        Ok(row.map(|r| TaskClaim {
            instance_id: r.get("instance_id"),
            task_id: r.get("task_id"),
            worker_id: r.get("worker_id"),
            claimed_at: r.get::<i64, _>("claimed_epoch") as u64,
            expires_at: r.get::<Option<i64>, _>("expires_epoch").map(|e| e as u64),
        }))
    }

    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> Result<(), BackendError> {
        // Check ownership first
        let row = sqlx::query(
            "SELECT worker_id FROM task_claims WHERE instance_id = $1 AND task_id = $2",
        )
        .bind(instance_id)
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?
        .ok_or_else(|| BackendError::NotFound(format!("{instance_id}:{task_id}")))?;

        let owner: String = row.get("worker_id");
        if owner != worker_id {
            return Err(BackendError::Backend(format!(
                "Claim owned by different worker: {owner}"
            )));
        }

        sqlx::query(
            "DELETE FROM task_claims
             WHERE instance_id = $1 AND task_id = $2 AND worker_id = $3",
        )
        .bind(instance_id)
        .bind(task_id)
        .bind(worker_id)
        .execute(&self.pool)
        .await
        .map_err(PgError)?;

        Ok(())
    }

    async fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: Duration,
    ) -> Result<(), BackendError> {
        let row = sqlx::query(
            "SELECT worker_id, expires_at FROM task_claims
             WHERE instance_id = $1 AND task_id = $2",
        )
        .bind(instance_id)
        .bind(task_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?
        .ok_or_else(|| BackendError::NotFound(format!("{instance_id}:{task_id}")))?;

        let owner: String = row.get("worker_id");
        if owner != worker_id {
            return Err(BackendError::Backend(format!(
                "Claim owned by different worker: {owner}"
            )));
        }

        // Only extend if there's an expiration set
        let expires_at: Option<chrono::DateTime<Utc>> = row.get("expires_at");
        if let Some(exp) = expires_at {
            let new_exp = exp
                .checked_add_signed(additional_duration)
                .ok_or_else(|| BackendError::Backend("Time overflow".to_string()))?;

            sqlx::query(
                "UPDATE task_claims SET expires_at = $1
                 WHERE instance_id = $2 AND task_id = $3",
            )
            .bind(new_exp)
            .bind(instance_id)
            .bind(task_id)
            .execute(&self.pool)
            .await
            .map_err(PgError)?;
        }

        Ok(())
    }

    async fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<AvailableTask>, BackendError> {
        // Step 1: Clean expired claims
        sqlx::query("DELETE FROM task_claims WHERE expires_at IS NOT NULL AND expires_at < now()")
            .execute(&self.pool)
            .await
            .map_err(PgError)?;

        // Step 2: Fetch candidate workflows via SQL bulk filter
        let rows = sqlx::query(
            "SELECT s.instance_id, s.data
             FROM workflow_snapshots s
             WHERE s.status = 'InProgress'
               AND NOT EXISTS (
                   SELECT 1 FROM task_claims c
                   WHERE c.instance_id = s.instance_id
                     AND c.task_id = s.current_task_id
                     AND (c.expires_at IS NULL OR c.expires_at > now())
               )
               AND NOT EXISTS (
                   SELECT 1 FROM workflow_signals sig
                   WHERE sig.instance_id = s.instance_id
               )
             ORDER BY s.updated_at ASC
             LIMIT $1",
        )
        .bind(limit as i64)
        .fetch_all(&self.pool)
        .await
        .map_err(PgError)?;

        // Step 3: App-level evaluation per candidate
        let mut available = Vec::new();
        for row in &rows {
            let raw: &[u8] = row.get("data");
            let mut snapshot = self.decode(raw)?;

            match &snapshot.state {
                // Delay: if expired, advance past it
                WorkflowSnapshotState::InProgress {
                    position:
                        ExecutionPosition::AtDelay {
                            wake_at,
                            next_task_id,
                            delay_id,
                            ..
                        },
                    ..
                } if Utc::now() >= *wake_at => {
                    if let Some(next_id) = next_task_id.clone() {
                        snapshot.update_position(ExecutionPosition::AtTask { task_id: next_id });
                        self.save_snapshot(&snapshot).await?;

                        if let WorkflowSnapshotState::InProgress {
                            position: ExecutionPosition::AtTask { task_id },
                            completed_tasks,
                            ..
                        } = &snapshot.state
                            && let Some(task) =
                                build_available_task(&snapshot, task_id, completed_tasks, worker_id)
                        {
                            available.push(task);
                        }
                    } else {
                        // Delay is the last node — complete the workflow
                        let output = snapshot.get_task_result_bytes(delay_id).unwrap_or_default();
                        snapshot.mark_completed(output);
                        self.save_snapshot(&snapshot).await?;
                    }
                }

                // Task: check retry backoff, then add to available
                WorkflowSnapshotState::InProgress {
                    position: ExecutionPosition::AtTask { task_id },
                    completed_tasks,
                    ..
                } => {
                    // Skip if task is already completed
                    if completed_tasks.contains_key(task_id) {
                        continue;
                    }

                    // Skip if retry backoff hasn't elapsed
                    if let Some(rs) = snapshot.task_retries.get(task_id)
                        && Utc::now() < rs.next_retry_at
                    {
                        continue;
                    }

                    if let Some(task) =
                        build_available_task(&snapshot, task_id, completed_tasks, worker_id)
                    {
                        available.push(task);
                    }
                }

                _ => continue,
            }

            if available.len() >= limit {
                break;
            }
        }

        // Ordering is already by updated_at from SQL.
        // For full worker bias, snapshots would need to be retained in the loop
        // above — this can be added when needed.

        Ok(available)
    }
}

/// Build an [`AvailableTask`] from a snapshot at a task position.
fn build_available_task(
    snapshot: &WorkflowSnapshot,
    task_id: &str,
    completed_tasks: &std::collections::HashMap<String, sayiir_core::snapshot::TaskResult>,
    _worker_id: &str,
) -> Option<AvailableTask> {
    let input = if completed_tasks.is_empty() {
        snapshot.initial_input_bytes()
    } else {
        snapshot.get_last_task_output()
    };

    input.map(|input_bytes| AvailableTask {
        instance_id: snapshot.instance_id.clone(),
        task_id: task_id.to_string(),
        input: input_bytes,
        workflow_definition_hash: snapshot.definition_hash.clone(),
    })
}
