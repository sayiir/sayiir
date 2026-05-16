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
    #[tracing::instrument(
        name = "db.claim_task",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> Result<Option<TaskClaim>, BackendError> {
        tracing::debug!("claiming task");
        let expires_at = ttl.and_then(|d| Utc::now().checked_add_signed(d));

        // Insert claim; on conflict only replace if the existing claim has expired.
        let row = sqlx::query(
            "INSERT INTO sayiir_task_claims (instance_id, task_id, worker_id, expires_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (instance_id, task_id) DO UPDATE
                SET worker_id = $3, claimed_at = now(), expires_at = $4
                WHERE sayiir_task_claims.expires_at IS NOT NULL AND sayiir_task_claims.expires_at < now()
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
            claimed_at: r.get::<i64, _>("claimed_epoch").cast_unsigned(),
            expires_at: r
                .get::<Option<i64>, _>("expires_epoch")
                .map(i64::cast_unsigned),
        }))
    }

    #[tracing::instrument(
        name = "db.release_task_claim",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> Result<(), BackendError> {
        tracing::debug!("releasing task claim");
        // Check ownership first
        let row = sqlx::query(
            "SELECT worker_id FROM sayiir_task_claims WHERE instance_id = $1 AND task_id = $2",
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
            "DELETE FROM sayiir_task_claims
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

    #[tracing::instrument(
        name = "db.extend_task_claim",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    async fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: Duration,
    ) -> Result<(), BackendError> {
        tracing::debug!("extending task claim");
        let row = sqlx::query(
            "SELECT worker_id, expires_at FROM sayiir_task_claims
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
                "UPDATE sayiir_task_claims SET expires_at = $1
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

    #[tracing::instrument(
        name = "db.find_available_tasks",
        skip(self),
        fields(db.system = "postgresql"),
        err(level = tracing::Level::ERROR),
    )]
    #[allow(clippy::cast_precision_loss, clippy::too_many_lines)]
    async fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
        aging_interval: Duration,
        worker_tags: &[String],
    ) -> Result<Vec<AvailableTask>, BackendError> {
        // Step 1: Clean expired claims
        sqlx::query(
            "DELETE FROM sayiir_task_claims WHERE expires_at IS NOT NULL AND expires_at < now()",
        )
        .execute(&self.pool)
        .await
        .map_err(PgError)?;

        // Step 2: Fetch candidate workflows ordered by effective priority with aging.
        // effective_priority = task_priority - (seconds_waiting / aging_interval)
        // Clamp to a minimum of 1s to prevent division by zero in the SQL expression.
        let aging_secs = (aging_interval.num_milliseconds() as f64 / 1000.0).max(1.0);
        let worker_tags_vec: Vec<&str> = worker_tags.iter().map(String::as_str).collect();

        // When worker has tags, add filter: task_tags must be a subset of
        // worker_tags. The `<@` operator checks array containment; an empty
        // array is a subset of every array, so untagged tasks always pass.
        let tag_filter = if worker_tags.is_empty() {
            ""
        } else {
            "AND s.task_tags <@ $3"
        };

        let query = format!(
            "SELECT s.instance_id, s.data, s.trace_parent
             FROM sayiir_workflow_snapshots s
             WHERE s.status = 'InProgress'
               AND NOT EXISTS (
                   SELECT 1 FROM sayiir_task_claims c
                   WHERE c.instance_id = s.instance_id
                     AND c.task_id = s.current_task_id
                     AND (c.expires_at IS NULL OR c.expires_at > now())
               )
               AND NOT EXISTS (
                   SELECT 1 FROM sayiir_workflow_signals sig
                   WHERE sig.instance_id = s.instance_id
               )
               {tag_filter}
             ORDER BY
               (s.task_priority - EXTRACT(EPOCH FROM (now() - s.updated_at)) / $2) ASC,
               s.updated_at ASC
             LIMIT $1"
        );

        let mut q = sqlx::query(&query)
            .bind(i64::try_from(limit).unwrap_or(i64::MAX))
            .bind(aging_secs);
        if !worker_tags.is_empty() {
            q = q.bind(&worker_tags_vec);
        }
        let rows = q.fetch_all(&self.pool).await.map_err(PgError)?;

        // Step 3: App-level evaluation per candidate.
        // Collect (worker_failed_here, task) pairs so we can stable-sort by
        // worker bias afterwards without re-decoding.
        let mut available: Vec<(bool, AvailableTask)> = Vec::with_capacity(rows.len());
        for row in &rows {
            let raw: &[u8] = row.get("data");
            let mut snapshot = self.decode(raw)?;
            snapshot.trace_parent = row.get("trace_parent");

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
                            let bias = snapshot.has_failed_on_worker(task_id, worker_id);
                            available.push((bias, task));
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
                        let bias = snapshot.has_failed_on_worker(task_id, worker_id);
                        available.push((bias, task));
                    }
                }

                _ => continue,
            }

            if available.len() >= limit {
                break;
            }
        }

        // Step 4: Stable-sort by worker bias so tasks whose last failure was on
        // this worker sink to the bottom, while preserving the effective-priority
        // order from the SQL query for everything else.
        available.sort_by_key(|(bias, _)| *bias);

        tracing::debug!(count = available.len(), "available tasks found");
        Ok(available.into_iter().map(|(_, task)| task).collect())
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
        trace_parent: snapshot.trace_parent.clone(),
    })
}
