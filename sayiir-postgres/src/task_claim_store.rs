//! [`TaskClaimStore`] implementation for Postgres.
//!
//! Claim ownership lives in `sayiir_workflow_claims` — one row per
//! in-flight workflow, PK on `instance_id`. Off the snapshot row so
//! `find_available_tasks`' `FOR UPDATE OF s SKIP LOCKED` doesn't
//! lock-fight with claim/release. Acquisition is gated by a
//! fast-fail `pg_try_advisory_xact_lock(hashtextextended(instance_id, 0))`
//! so racing workers short-circuit without blocking on a row lock.

use chrono::{Duration, Utc};
use sayiir_core::codec;
use sayiir_core::snapshot::{ExecutionPosition, WorkflowSnapshot, WorkflowSnapshotState};
use sayiir_core::task_claim::{AvailableTask, TaskClaim};
use sayiir_persistence::{BackendError, SnapshotStore, TaskClaimStore};
use sqlx::Row;

use crate::backend::PostgresBackend;
use crate::error::PgError;

/// SQL fragment that gates a snapshot row to "claimable right now":
/// no active claim on the instance and no pending signal. Shared
/// between `find_available_tasks` (polling scan) and `find_hinted_task`
/// (single-row lookup) so a future change to the eligibility rules
/// can't silently desynchronise the two paths.
///
/// `c.expires_at IS NULL` is treated as "live forever" — matches the
/// in-memory backend's never-expires semantic. Operationally this means
/// a crashed worker that held a no-TTL claim pins the workflow
/// undispatchable until the row is released manually. Runtime callers
/// (PooledWorker) always set a TTL; only out-of-band ad-hoc callers
/// should pass `ttl=None`, and only when they're certain a manual
/// release path exists.
const ELIGIBILITY_PREDICATE: &str = "NOT EXISTS (
            SELECT 1 FROM sayiir_workflow_claims c
            WHERE c.instance_id = s.instance_id
              AND (c.expires_at IS NULL OR c.expires_at > now())
        )
        AND NOT EXISTS (
            SELECT 1 FROM sayiir_workflow_signals sig
            WHERE sig.instance_id = s.instance_id
        )";

impl<C> TaskClaimStore for PostgresBackend<C>
where
    C: codec::SnapshotCodec,
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
        task_id: &sayiir_core::TaskId,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> Result<Option<TaskClaim>, BackendError> {
        let expires_at = ttl.and_then(|d| Utc::now().checked_add_signed(d));

        // One round-trip: the conditional INSERT and the
        // `snapshot_exists` probe share a single statement so a
        // concurrent insert/delete can't interleave between them. A
        // missing snapshot is reported as `NotFound` (almost always a
        // stale caller); the other no-update reasons — non-InProgress,
        // task advanced, lost advisory lock, slot held and unexpired —
        // collapse to `Ok(None)` for the caller to retry.
        //
        // Epoch fields use `FLOOR(EXTRACT(EPOCH ...))::BIGINT`: a bare
        // cast to BIGINT rounds, so a `.5+` fractional second would
        // return an epoch one ahead of the stored timestamp and break
        // round-trips against `chrono::DateTime::timestamp()` (floor).
        let query = "
            WITH probe AS (
                SELECT instance_id, current_task_id, status
                FROM sayiir_workflow_snapshots
                WHERE instance_id = $1
            ),
            lock AS (
                SELECT pg_try_advisory_xact_lock(hashtextextended($1::text, 0)) AS got
            ),
            upsert AS (
                INSERT INTO sayiir_workflow_claims
                    (instance_id, task_id, worker_id, expires_at)
                SELECT probe.instance_id, probe.current_task_id, $3, $4
                FROM probe, lock
                WHERE lock.got
                  AND probe.status = 'InProgress'
                  AND probe.current_task_id = $2
                  AND NOT EXISTS (
                      SELECT 1 FROM sayiir_workflow_claims c
                      WHERE c.instance_id = probe.instance_id
                        AND (c.expires_at IS NULL OR c.expires_at > now())
                  )
                  -- Re-execution race protection lives in the
                  -- worker''s validate_task_preconditions path, not
                  -- here: gating claim_task on a completed row in
                  -- workflow_tasks would permanently lock out
                  -- recovery when a worker dies BETWEEN
                  -- save_task_result (which writes the workflow_tasks
                  -- row) and save_snapshot (which advances
                  -- current_task_id). After that partial-completion,
                  -- every subsequent claim_task would reject and the
                  -- workflow would stall forever. The sleeping-giants
                  -- storm hits exactly this pattern under load.
                -- Gate ON CONFLICT DO UPDATE on the existing claim
                -- being expired. Without this, two transactions that
                -- both pass the SELECT-side NOT EXISTS check (e.g.
                -- because the prior claim's row just got deleted by
                -- release, or because both see the same expired row)
                -- can both INSERT, and ON CONFLICT silently picks the
                -- last writer — leaving the earlier worker thinking
                -- it owns a claim that the row actually attributes
                -- to a different worker. The WHERE here turns the
                -- upsert into 'steal expired only'; concurrent
                -- inserts on a still-live claim now produce zero
                -- upsert rows for the loser (caller sees Ok(None)
                -- and retries), instead of misreporting success.
                ON CONFLICT (instance_id) DO UPDATE
                    SET task_id    = EXCLUDED.task_id,
                        worker_id  = EXCLUDED.worker_id,
                        claimed_at = now(),
                        expires_at = EXCLUDED.expires_at
                    WHERE sayiir_workflow_claims.expires_at IS NOT NULL
                      AND sayiir_workflow_claims.expires_at <= now()
                RETURNING
                    FLOOR(EXTRACT(EPOCH FROM claimed_at))::BIGINT AS claimed_epoch,
                    FLOOR(EXTRACT(EPOCH FROM expires_at))::BIGINT AS expires_epoch
            )
            SELECT claimed_epoch, expires_epoch, TRUE AS snapshot_exists
            FROM upsert
            UNION ALL
            SELECT
                NULL::bigint, NULL::bigint,
                EXISTS (SELECT 1 FROM probe)
            WHERE NOT EXISTS (SELECT 1 FROM upsert)
        ";
        let row = sqlx::query(query)
            .bind(instance_id)
            .bind(task_id.as_bytes().as_slice())
            .bind(worker_id)
            .bind(expires_at)
            .fetch_one(&self.pool)
            .await
            .map_err(PgError)?;

        if let Some(claimed_epoch) = row.get::<Option<i64>, _>("claimed_epoch") {
            return Ok(Some(TaskClaim {
                instance_id: std::sync::Arc::from(instance_id),
                task_id: *task_id,
                worker_id: worker_id.to_string(),
                claimed_at: claimed_epoch.cast_unsigned(),
                expires_at: row
                    .get::<Option<i64>, _>("expires_epoch")
                    .map(i64::cast_unsigned),
            }));
        }

        if !row.get::<bool, _>("snapshot_exists") {
            return Err(BackendError::NotFound(format!("{instance_id}:{task_id}")));
        }
        Ok(None)
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
        task_id: &sayiir_core::TaskId,
        worker_id: &str,
    ) -> Result<(), BackendError> {
        // DELETE + owner disambiguation in one statement closes the
        // inter-statement window where a concurrent release/re-claim
        // could otherwise make the error path misreport the owner.
        // The owner subselect is gated on `NOT EXISTS (del)` so the
        // hot success path skips it.
        //
        // Match on `task_id` too — the API is task-scoped and the
        // in-memory backend keys claims by task_id, so a stale caller
        // (e.g. a worker whose claim was taken over by an expired-
        // claim steal and replaced with a different (worker, task))
        // must not be allowed to delete the new owner's claim just
        // because the worker_id happens to coincide.
        //
        // The probe returns the live row's worker_id AND task_id (no
        // filtering on either column). `disambiguate_writeback` then
        // verifies BOTH match in Rust before treating self-ownership as
        // success — keeping the contract check out of the SQL where a
        // future query refactor could silently drop it.
        let row = sqlx::query(
            "WITH del AS (
                 DELETE FROM sayiir_workflow_claims
                 WHERE instance_id = $1 AND worker_id = $2 AND task_id = $3
                 RETURNING 1
             ),
             probe AS (
                 SELECT worker_id, task_id
                 FROM sayiir_workflow_claims
                 WHERE instance_id = $1 AND NOT EXISTS (SELECT 1 FROM del)
             )
             SELECT
                 EXISTS (SELECT 1 FROM del) AS done,
                 (SELECT worker_id FROM probe) AS owner,
                 (SELECT task_id   FROM probe) AS current_task_id",
        )
        .bind(instance_id)
        .bind(worker_id)
        .bind(task_id.as_bytes().as_slice())
        .fetch_one(&self.pool)
        .await
        .map_err(PgError)?;

        disambiguate_writeback(&row, instance_id, task_id, worker_id)
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
        task_id: &sayiir_core::TaskId,
        worker_id: &str,
        ttl: Duration,
    ) -> Result<(), BackendError> {
        // Heartbeat. Renewal is absolute (`now() + ttl`), not additive:
        // adding to `expires_at` would let the lease drift ahead of the
        // wall clock by ~ttl per tick, so an orphaned claim from a
        // crashed worker would block reclaim for far longer than one
        // TTL. A no-TTL claim is eternal-until-released; mirror
        // the in-memory backend by silently no-op'ing the extension in
        // that case (no `COALESCE` — that would *introduce* a TTL).
        // When the row exists with our worker AND our task_id but
        // `expires_at IS NULL`, the UPDATE returns 0 rows and the
        // probe returns (worker=self, task=task_id) — recognised by
        // `disambiguate_writeback` as the legitimate no-TTL no-op.
        //
        // The UPDATE matches on (instance_id, worker_id, task_id) so a
        // stale heartbeat for a different task is a structural miss.
        // The probe returns the LIVE row's worker_id AND task_id (no
        // column-side filtering); `disambiguate_writeback` verifies
        // BOTH match in Rust before treating self-ownership as success,
        // keeping the task-scoped contract out of the SQL where a
        // future refactor could silently drop it.
        let row = sqlx::query(
            "WITH upd AS (
                 UPDATE sayiir_workflow_claims
                 SET expires_at = now() + $3
                 WHERE instance_id = $1
                   AND worker_id = $2
                   AND task_id = $4
                   AND expires_at IS NOT NULL
                 RETURNING 1
             ),
             probe AS (
                 SELECT worker_id, task_id
                 FROM sayiir_workflow_claims
                 WHERE instance_id = $1 AND NOT EXISTS (SELECT 1 FROM upd)
             )
             SELECT
                 EXISTS (SELECT 1 FROM upd) AS done,
                 (SELECT worker_id FROM probe) AS owner,
                 (SELECT task_id   FROM probe) AS current_task_id",
        )
        .bind(instance_id)
        .bind(worker_id)
        .bind(ttl)
        .bind(task_id.as_bytes().as_slice())
        .fetch_one(&self.pool)
        .await
        .map_err(PgError)?;

        disambiguate_writeback(&row, instance_id, task_id, worker_id)
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
        // Expired claims are reclaimable via the eligibility predicate's
        // `c.expires_at > now()` check; dead rows linger until the next
        // successful claim on the instance overwrites them (steal path).

        let aging_secs = (aging_interval.num_milliseconds() as f64 / 1000.0).max(1.0);
        let worker_tags_vec: Vec<&str> = worker_tags.iter().map(String::as_str).collect();
        let tag_filter = if worker_tags.is_empty() {
            ""
        } else {
            "AND s.task_tags <@ $2"
        };

        // Two LIMIT-bounded index-order arms instead of one full-set sort.
        // The old single query ordered by the aging expression
        // `(task_priority - age/interval)`, which is non-SARGable: every
        // poll tick sorted ALL InProgress rows (~190ms at 200k rows).
        // Each arm below is satisfied by an index in order — arm 1 by
        // idx_snapshots_inprogress (task_priority, updated_at), arm 2 by
        // idx_snapshots_inprogress_updated (updated_at) — so PG stops
        // after `limit` rows per arm. The exact aging score is then
        // computed in Rust over <= 2*limit candidates.
        //
        // Aging becomes approximate: the true score-optimal row is almost
        // always in one of the two arms (best-priority or oldest), and the
        // oldest-first arm guarantees starved rows are eventually offered,
        // which is what aging exists for.
        //
        // `FOR UPDATE OF s SKIP LOCKED` keeps statement-scope dedup —
        // two pollers running simultaneously skip each other's rows
        // instead of returning the full set in duplicate. Both arms run
        // in the same transaction, so arm 2 sees arm 1's own locks as its
        // own and may return the same row; DISTINCT ON dedups. The actual
        // claim race is resolved by the conditional UPDATE in
        // `claim_task` plus its advisory lock. The history JOIN stays
        // outside the locking subqueries so only candidate blob rows are
        // materialised.
        let dispatchable = "s.status = 'InProgress'
                   AND (
                       s.position_kind = 'AtTask'
                       OR (s.position_kind = 'AtDelay'
                           AND s.delay_wake_at IS NOT NULL
                           AND s.delay_wake_at <= now())
                   )";
        let arm = |order_by: &str, alias: &str| {
            format!(
                "SELECT * FROM
                     (SELECT s.instance_id, s.history_version, s.trace_parent,
                             s.task_priority, s.updated_at
                      FROM sayiir_workflow_snapshots s
                      WHERE {dispatchable}
                        AND {ELIGIBILITY_PREDICATE}
                        {tag_filter}
                      ORDER BY {order_by}
                      LIMIT $1
                      FOR UPDATE OF s SKIP LOCKED) {alias}"
            )
        };
        let arm_priority = arm("s.task_priority ASC, s.updated_at ASC", "arm_priority");
        let arm_oldest = arm("s.updated_at ASC", "arm_oldest");
        let query = format!(
            "SELECT r.instance_id, r.task_priority, r.updated_at, h.data, r.trace_parent
             FROM (
                 SELECT DISTINCT ON (u.instance_id)
                        u.instance_id, u.history_version, u.trace_parent,
                        u.task_priority, u.updated_at
                 FROM ({arm_priority} UNION ALL {arm_oldest}) u
                 ORDER BY u.instance_id
             ) r
             JOIN sayiir_workflow_snapshot_history h
               ON h.instance_id = r.instance_id AND h.version = r.history_version"
        );

        let mut q =
            sqlx::query(sqlx::AssertSqlSafe(query)).bind(i64::try_from(limit).unwrap_or(i64::MAX));
        if !worker_tags.is_empty() {
            q = q.bind(&worker_tags_vec);
        }
        let rows = q.fetch_all(&self.pool).await.map_err(PgError)?;

        // Score on the narrow columns first and truncate to `limit` —
        // the two arms can return up to 2*limit candidates, and only the
        // survivors are worth the blob decode + outputs hydration below.
        let now = Utc::now();
        let mut scored: Vec<(f64, &sqlx::postgres::PgRow)> = rows
            .iter()
            .map(|row| {
                let priority: i16 = row.get("task_priority");
                let updated_at: chrono::DateTime<Utc> = row.get("updated_at");
                let age_secs = (now - updated_at).num_milliseconds() as f64 / 1000.0;
                (f64::from(priority) - age_secs / aging_secs, row)
            })
            .collect();
        scored.sort_by(|(a, _), (b, _)| a.total_cmp(b));
        scored.truncate(limit);

        // Decode-then-hydrate: outputs no longer live in the snapshot
        // blob, so batch-fetch them from `sayiir_workflow_tasks` once for
        // every surviving candidate before the match loop touches
        // `completed_tasks`.
        let mut decoded: Vec<WorkflowSnapshot> = Vec::with_capacity(scored.len());
        for (_, row) in &scored {
            let raw: &[u8] = row.get("data");
            let mut snapshot = self.decode(raw)?;
            snapshot.trace_parent = row.get("trace_parent");
            decoded.push(snapshot);
        }
        let instance_id_strs: Vec<&str> = decoded.iter().map(|s| s.instance_id.as_ref()).collect();
        let mut outputs_by_instance =
            crate::history::fetch_task_outputs_batched(&self.pool, &instance_id_strs).await?;
        for snapshot in &mut decoded {
            if let Some(outputs) = outputs_by_instance.remove(snapshot.instance_id.as_ref()) {
                snapshot.hydrate_task_outputs(outputs);
            }
        }

        let mut available: Vec<(bool, AvailableTask)> = Vec::with_capacity(decoded.len());
        for mut snapshot in decoded {
            // Examine state, possibly advance position, extract the
            // task_id we want to dispatch. All field captures from the
            // match pattern are `Copy` so the borrow on `snapshot.state`
            // is released before the move into build_available_task.
            let task_id_to_dispatch: sayiir_core::TaskId = match &snapshot.state {
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
                    let next_id_opt = *next_task_id;
                    let delay_id_owned = *delay_id;
                    if let Some(next_id) = next_id_opt {
                        snapshot.update_position(ExecutionPosition::AtTask { task_id: next_id });
                        self.save_snapshot(&mut snapshot).await?;
                        next_id
                    } else {
                        // Delay is the last node — complete the workflow.
                        let output = snapshot
                            .get_task_result_bytes(&delay_id_owned)
                            .unwrap_or_default();
                        snapshot.mark_completed(output);
                        self.save_snapshot(&mut snapshot).await?;
                        continue;
                    }
                }
                WorkflowSnapshotState::InProgress {
                    position: ExecutionPosition::AtTask { task_id },
                    completed_tasks,
                    ..
                } => {
                    if completed_tasks.contains_key(task_id) {
                        continue;
                    }
                    let task_id_copy = *task_id;
                    if let Some(rs) = snapshot.task_retries.get(&task_id_copy)
                        && Utc::now() < rs.next_retry_at
                    {
                        continue;
                    }
                    task_id_copy
                }
                _ => continue,
            };

            let bias = snapshot.has_failed_on_worker(&task_id_to_dispatch, worker_id);
            if let Some(task) = build_available_task(snapshot, task_id_to_dispatch) {
                available.push((bias, task));
            }

            if available.len() >= limit {
                break;
            }
        }

        available.sort_by_key(|(bias, _)| *bias);
        tracing::debug!(count = available.len(), "available tasks found");
        Ok(available.into_iter().map(|(_, task)| task).collect())
    }

    async fn wait_for_wakeup(
        &self,
        timeout: std::time::Duration,
    ) -> Result<Option<sayiir_persistence::TaskWakeupHint>, BackendError> {
        Ok(self.wakeup.wait(timeout).await)
    }

    #[tracing::instrument(
        name = "db.find_hinted_task",
        skip(self, hint),
        fields(
            db.system = "postgresql",
            instance_id = %hint.instance_id,
            task_id = %sayiir_core::TaskId::from_bytes(hint.task_id),
        ),
        err(level = tracing::Level::ERROR),
    )]
    async fn find_hinted_task(
        &self,
        hint: &sayiir_persistence::TaskWakeupHint,
    ) -> Result<Option<AvailableTask>, BackendError> {
        // Single round-trip on the NOTIFY hot path: eligibility check,
        // history-blob fetch, and per-task outputs all land in one
        // statement. Pre-collapse this was three sequential RTTs
        // (eligibility probe + fetch_blob + fetch_task_outputs); at
        // 1000-fanout signal-driven workloads that's thousands of
        // sequential round-trips on the headline path this branch is
        // built to optimise.
        //
        // No FOR UPDATE: the snapshot row needs no row-lock — concurrent
        // mutators (save_task_result, check_and_*) serialise on the
        // claim slot and the `lock_snapshot_for_mutation` FOR UPDATE in
        // their own paths.
        let query = format!(
            "SELECT
                 s.trace_parent,
                 h.data,
                 (SELECT array_agg(t.task_id ORDER BY t.task_id)
                  FROM sayiir_workflow_tasks t
                  WHERE t.instance_id = s.instance_id
                    AND t.status = 'completed'
                    AND t.output IS NOT NULL) AS task_ids,
                 (SELECT array_agg(t.output ORDER BY t.task_id)
                  FROM sayiir_workflow_tasks t
                  WHERE t.instance_id = s.instance_id
                    AND t.status = 'completed'
                    AND t.output IS NOT NULL) AS task_outputs
             FROM sayiir_workflow_snapshots s
             JOIN sayiir_workflow_snapshot_history h
               ON h.instance_id = s.instance_id AND h.version = s.history_version
             WHERE s.instance_id = $1
               AND s.current_task_id = $2
               AND s.status = 'InProgress'
               AND {ELIGIBILITY_PREDICATE}"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(&hint.instance_id)
            .bind(hint.task_id.as_slice())
            .fetch_optional(&self.pool)
            .await
            .map_err(PgError)?;

        let Some(row) = row else { return Ok(None) };

        let data: Vec<u8> = row.get("data");
        let mut snapshot = self.decode(&data)?;
        snapshot.trace_parent = row.get("trace_parent");

        // Hydrate outputs from the paired arrays. Both arrays come from
        // the same subquery with the same ORDER BY, so positional
        // pairing is safe.
        let task_ids: Option<Vec<Vec<u8>>> = row.get("task_ids");
        let task_outputs: Option<Vec<Vec<u8>>> = row.get("task_outputs");
        if let (Some(ids), Some(outs)) = (task_ids, task_outputs) {
            // `Vec`, not `HashMap`: `hydrate_task_outputs` only iterates these.
            let mut outputs = Vec::with_capacity(ids.len());
            for (id_bytes, out_bytes) in ids.into_iter().zip(outs.into_iter()) {
                let tid = sayiir_core::TaskId::from_slice(&id_bytes).map_err(|_| {
                    BackendError::Backend(format!(
                        "sayiir_workflow_tasks.task_id for instance {} is {} bytes (expected 32)",
                        hint.instance_id,
                        id_bytes.len()
                    ))
                })?;
                outputs.push((tid, bytes::Bytes::from(out_bytes)));
            }
            snapshot.hydrate_task_outputs(outputs);
        }

        let hint_task_id = sayiir_core::TaskId::from_bytes(hint.task_id);
        let matches = match &snapshot.state {
            WorkflowSnapshotState::InProgress {
                position: ExecutionPosition::AtTask { task_id },
                completed_tasks,
                ..
            } => *task_id == hint_task_id && !completed_tasks.contains_key(task_id),
            _ => false,
        };
        if !matches {
            return Ok(None);
        }

        Ok(build_available_task(snapshot, hint_task_id))
    }

    #[tracing::instrument(
        name = "db.save_snapshot_fenced",
        skip(self, snapshot),
        fields(
            db.system = "postgresql",
            instance_id = %snapshot.instance_id,
            status = %snapshot.state.as_ref(),
        ),
        err(level = tracing::Level::ERROR),
    )]
    async fn save_snapshot_fenced(
        &self,
        snapshot: &mut WorkflowSnapshot,
        task_id: &sayiir_core::TaskId,
        worker_id: &str,
    ) -> Result<bool, BackendError> {
        // Fencing: lock our claim row first. FOR UPDATE makes a concurrent
        // expired-claim steal (claim_task's ON CONFLICT UPDATE) wait until
        // this commit, so "claim verified live" holds for the whole save.
        // No live claim row for (instance, task, us) → the lease expired or
        // was stolen; refuse the write so a slow worker can't clobber the
        // new claimant's progress.
        let mut tx = self.pool.begin().await.map_err(PgError)?;
        let owned = sqlx::query(
            "SELECT 1 FROM sayiir_workflow_claims
             WHERE instance_id = $1 AND worker_id = $2 AND task_id = $3
               AND (expires_at IS NULL OR expires_at > now())
             FOR UPDATE",
        )
        .bind(&*snapshot.instance_id)
        .bind(worker_id)
        .bind(task_id.as_bytes().as_slice())
        .fetch_optional(&mut *tx)
        .await
        .map_err(PgError)?;

        if owned.is_none() {
            tx.rollback().await.map_err(PgError)?;
            tracing::warn!(
                instance_id = %snapshot.instance_id,
                %task_id,
                worker_id,
                "claim no longer held; snapshot save fenced off"
            );
            return Ok(false);
        }

        self.exec_save_snapshot(&mut *tx, snapshot).await?;
        tx.commit().await.map_err(PgError)?;
        Ok(true)
    }
}

/// Decode the `(done, owner, current_task_id)` shape returned by the
/// release/extend writable-CTE queries.
///
/// `done` is the hot-path success signal. When false, the probe returns
/// the LIVE row's `(worker_id, task_id)` — the SQL applies no column-
/// side filter, so the disambiguation is performed here in Rust where
/// the task-scoped contract is explicit and testable:
///
/// - row absent (`owner=NULL`) → `NotFound`.
/// - row's worker_id differs from ours → owner-mismatch `Backend` error.
/// - row's worker_id matches but task_id differs → `NotFound` (the API
///   is task-scoped; a worker that owns a DIFFERENT task on the same
///   instance is not the owner of OUR task). The in-memory backend
///   keys claims by (instance_id, task_id) and returns the same shape.
/// - row's worker_id AND task_id both match → `Ok`. For extend this is
///   the no-TTL no-op (UPDATE's `expires_at IS NOT NULL` gate filtered
///   out the row); for release it is unreachable because the DELETE
///   has already matched on all three columns when this state holds.
fn disambiguate_writeback(
    row: &sqlx::postgres::PgRow,
    instance_id: &str,
    task_id: &sayiir_core::TaskId,
    worker_id: &str,
) -> Result<(), BackendError> {
    if row.get::<bool, _>("done") {
        return Ok(());
    }
    let owner: Option<String> = row.get("owner");
    let Some(owner) = owner else {
        return Err(BackendError::NotFound(format!("{instance_id}:{task_id}")));
    };
    if owner != worker_id {
        return Err(BackendError::ClaimLost(format!(
            "claim on {instance_id}:{task_id} owned by {owner}"
        )));
    }
    // owner == self: verify the row's task_id matches the request.
    let current: Option<Vec<u8>> = row.get("current_task_id");
    match current {
        Some(bytes) if bytes.as_slice() == task_id.as_bytes().as_slice() => Ok(()),
        _ => Err(BackendError::NotFound(format!("{instance_id}:{task_id}"))),
    }
}

/// Build an [`AvailableTask`] from a snapshot at a task position.
///
/// Takes the snapshot by VALUE and wraps it in `Arc` for the
/// [`AvailableTask::snapshot`] field — no deep clone. Callers must
/// extract `task_id` and any decision fields (bias, retry timing) from
/// the snapshot BEFORE handing it off here, since we move on the way
/// in. Returns `None` when the snapshot isn't `InProgress` at a task
/// position with a resolvable input.
fn build_available_task(
    snapshot: WorkflowSnapshot,
    task_id: sayiir_core::TaskId,
) -> Option<AvailableTask> {
    let completed_empty = match &snapshot.state {
        sayiir_core::snapshot::WorkflowSnapshotState::InProgress {
            completed_tasks, ..
        } => completed_tasks.is_empty(),
        _ => return None,
    };
    let input = if completed_empty {
        snapshot.initial_input_bytes()
    } else {
        snapshot.get_last_task_output()
    };

    input.map(|input_bytes| AvailableTask {
        instance_id: snapshot.instance_id.clone(),
        task_id,
        input: input_bytes,
        workflow_definition_hash: snapshot.definition_hash,
        trace_parent: snapshot.trace_parent.clone(),
        snapshot: std::sync::Arc::new(snapshot),
    })
}
