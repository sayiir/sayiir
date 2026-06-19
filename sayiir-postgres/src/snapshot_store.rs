//! [`SnapshotStore`] implementation for Postgres.

use sayiir_core::codec;
use sayiir_core::snapshot::{SnapshotStatus, WorkflowSnapshot};
use sayiir_persistence::{BackendError, SnapshotStore};
use sqlx::Row;

use crate::backend::PostgresBackend;
use crate::error::{PgError, with_transient_retry};
use crate::wakeup::{TASK_READY_CHANNEL, build_task_ready_payload};

impl<C> PostgresBackend<C>
where
    C: codec::SnapshotCodec,
{
    /// The full save-snapshot CTE, generic over the executor so it can run
    /// either directly on the pool or inside a fencing transaction (see
    /// `save_snapshot_fenced`).
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn exec_save_snapshot<'e, E>(
        &self,
        executor: E,
        snapshot: &mut WorkflowSnapshot,
    ) -> Result<(), BackendError>
    where
        E: sqlx::PgExecutor<'e>,
    {
        tracing::debug!("saving snapshot");
        let (data, data_hash) = self.encode_blob_preserving(snapshot)?;
        let status = snapshot.state.as_ref();
        let task_id_bytes: Option<[u8; 32]> = snapshot.current_task_id().map(|t| *t.as_bytes());
        let task_id: Option<&[u8]> = task_id_bytes.as_ref().map(<[u8; 32]>::as_slice);
        let task_count = snapshot.completed_task_count();
        let error = snapshot.error_message().map(ToString::to_string);
        let terminal = snapshot.state.is_terminal();
        let pos_kind = snapshot.position_kind();
        let wake_at = snapshot.delay_wake_at();
        let task_priority = i16::from(snapshot.current_task_priority());
        // Bind the `&[String]` slice directly — sqlx encodes it as `text[]`,
        // so the intermediate `Vec<&str>` collect (one alloc per save) is
        // unnecessary.
        let task_tags = snapshot.current_task_tags();
        let terminal_status = match SnapshotStatus::from(&snapshot.state) {
            SnapshotStatus::Failed => "failed",
            SnapshotStatus::Cancelled => "cancelled",
            _ => "completed",
        };
        let notify_payload = build_task_ready_payload(snapshot);

        // Ship the just-completed output to the sidecar ONLY when flagged
        // unflushed (set by `mark_task_completed`, cleared post-write). On
        // position-only saves (pause/cancel/delay/signal parking) the flag is
        // false, so both binds are NULL and the `task_output` CTE no-ops via
        // `WHERE $18 IS NOT NULL` — no re-shipping. Safe: an unflagged output
        // is already in `workflow_tasks`.
        let output_unflushed = snapshot.output_unflushed;
        let last_completed_task_id: Option<[u8; 32]> = output_unflushed
            .then(|| snapshot.last_completed_task_id().map(|t| *t.as_bytes()))
            .flatten();
        let last_completed_slice: Option<&[u8]> =
            last_completed_task_id.as_ref().map(<[u8; 32]>::as_slice);
        let last_completed_output: Option<bytes::Bytes> = output_unflushed
            .then(|| {
                snapshot
                    .last_completed_task_id()
                    .and_then(|id| snapshot.get_task_result(&id).map(|r| r.output.clone()))
            })
            .flatten();
        let last_completed_output_slice: Option<&[u8]> = last_completed_output.as_deref();

        // One round-trip via a CTE chain: bump the snapshot version,
        // append the history row at that version, refresh the
        // workflow_tasks lifecycle, and queue the wakeup NOTIFY — all
        // in one statement. Sibling DML CTEs in PG always run; the
        // gates (`WHERE $task_id IS NOT NULL`, `WHERE $terminal`,
        // `WHERE $notify IS NOT NULL`) make zero-row branches cheap
        // no-ops without conditional Rust string building.
        //
        // The trailing `SELECT history_version FROM snap` is needed for
        // the CTE chain to be a valid statement, but the value is not
        // read on the caller side. `.execute()` skips PgRow allocation
        // on the Rust side while still driving the full CTE on PG.
        let query = "
            WITH snap AS (
                INSERT INTO sayiir_workflow_snapshots
                    (instance_id, status, definition_hash, current_task_id,
                     completed_task_count, error, position_kind, delay_wake_at,
                     trace_parent, task_priority, task_tags, data_hash,
                     completed_at, updated_at)
                VALUES ($1, $2, $3, $4, $5, $6, $8, $9, $10, $11, $12, $13,
                        CASE WHEN $7 THEN now() ELSE NULL END, now())
                ON CONFLICT (instance_id) DO UPDATE SET
                    status = $2,
                    definition_hash = $3,
                    current_task_id = $4,
                    completed_task_count = $5,
                    error = $6,
                    position_kind = $8,
                    delay_wake_at = $9,
                    trace_parent = $10,
                    task_priority = $11,
                    task_tags = $12,
                    data_hash = $13,
                    history_version = sayiir_workflow_snapshots.history_version + 1,
                    completed_at = CASE WHEN $7 THEN now() ELSE sayiir_workflow_snapshots.completed_at END,
                    updated_at = now()
                WHERE sayiir_workflow_snapshots.status NOT IN ('Completed', 'Failed', 'Cancelled')
                RETURNING history_version
            ),
            hist AS (
                INSERT INTO sayiir_workflow_snapshot_history
                    (instance_id, version, status, current_task_id, data, data_hash)
                SELECT $1, snap.history_version, $2, $4, $14, $13
                FROM snap
                RETURNING 1
            ),
            task_active AS (
                INSERT INTO sayiir_workflow_tasks (instance_id, task_id, status, started_at)
                SELECT $1, $4, 'active', now()
                WHERE $4 IS NOT NULL AND NOT $7
                  -- Skip when the just-completed task IS the current task
                  -- (final task in a chain has next=None, so position stays
                  -- on it and current_task_id == last_completed_task_id).
                  -- The task_output CTE below already UPSERTs that row;
                  -- this CTE doing it too trips PG`s rule that ON CONFLICT
                  -- DO UPDATE cannot affect a row twice in one statement.
                  AND $4 IS DISTINCT FROM $18
                ON CONFLICT (instance_id, task_id) DO UPDATE SET
                    status = CASE
                        WHEN sayiir_workflow_tasks.status = 'completed' THEN sayiir_workflow_tasks.status
                        ELSE 'active'
                    END,
                    started_at = COALESCE(sayiir_workflow_tasks.started_at, now())
                RETURNING 1
            ),
            task_terminal AS (
                UPDATE sayiir_workflow_tasks
                SET status = $15, completed_at = now(), error = $6
                WHERE instance_id = $1 AND status = 'active' AND $7
                RETURNING 1
            ),
            task_output AS (
                INSERT INTO sayiir_workflow_tasks
                    (instance_id, task_id, status, completed_at, output)
                SELECT $1, $18, 'completed', now(), $19
                WHERE $18 IS NOT NULL
                ON CONFLICT (instance_id, task_id) DO UPDATE SET
                    status = 'completed',
                    completed_at = now(),
                    error = NULL,
                    output = EXCLUDED.output
                WHERE sayiir_workflow_tasks.output IS NULL
                   OR sayiir_workflow_tasks.output IS DISTINCT FROM EXCLUDED.output
                RETURNING 1
            )
            -- pg_notify lives in the outer SELECT (not a sibling CTE)
            -- because PG's optimizer prunes unreferenced non-DML CTEs.
            -- `WITH notify AS (SELECT pg_notify(...) WHERE ...)` without
            -- a reference from the outer query is silently dropped and
            -- the wake never fires; the `wait_for_wakeup_*` integration
            -- tests catch that regression. CASE keeps the conditional
            -- behaviour — `pg_notify` is only evaluated when the
            -- payload is non-NULL because CASE short-circuits its arms.
            SELECT history_version,
                   CASE WHEN $17 IS NOT NULL THEN pg_notify($16, $17) END
            FROM snap
        ";

        sqlx::query(query)
            .bind(&*snapshot.instance_id) // $1
            .bind(status) // $2
            .bind(snapshot.definition_hash.as_bytes().as_slice()) // $3
            .bind(task_id) // $4
            .bind(task_count) // $5
            .bind(&error) // $6
            .bind(terminal) // $7
            .bind(pos_kind) // $8
            .bind(wake_at) // $9
            .bind(snapshot.trace_parent.as_deref()) // $10
            .bind(task_priority) // $11
            .bind(task_tags) // $12
            .bind(data_hash.as_slice()) // $13
            .bind(&data) // $14
            .bind(terminal_status) // $15
            .bind(TASK_READY_CHANNEL) // $16
            .bind(notify_payload.as_deref()) // $17
            .bind(last_completed_slice) // $18
            .bind(last_completed_output_slice) // $19
            .execute(executor)
            .await
            .map_err(PgError)?;
        // The last completed output (if any) is now durable in
        // `workflow_tasks`; clear the marker so a later save of this same
        // in-memory snapshot binds NULL instead of re-shipping the bytes.
        snapshot.output_unflushed = false;
        Ok(())
    }

    /// One attempt of `save_task_result`; retried on transient errors by
    /// the trait method.
    async fn save_task_result_once(
        &self,
        instance_id: &str,
        task_id: &sayiir_core::TaskId,
        output: bytes::Bytes,
    ) -> Result<(), crate::error::TxError> {
        let mut tx = self.pool.begin().await.map_err(PgError)?;

        // FOR UPDATE on s serialises read-modify-write across this path
        // and signal_store's mutators — TaskClaim only protects against
        // peer workers, not against concurrent check_and_cancel.
        let (mut snapshot, prev_history_version) = self
            .lock_snapshot_for_mutation(&mut tx, instance_id)
            .await?
            .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        let output_bytes = output.clone();
        snapshot.mark_task_completed(*task_id, output);

        let (data, data_hash) = self.encode_blob(&mut snapshot)?;
        let status = snapshot.state.as_ref();
        let current_bytes: Option<[u8; 32]> = snapshot.current_task_id().map(|t| *t.as_bytes());
        let current: Option<&[u8]> = current_bytes.as_ref().map(<[u8; 32]>::as_slice);
        let task_count = snapshot.completed_task_count();
        let next_history_version = prev_history_version + 1;

        // UPDATE snapshots + INSERT history + UPSERT workflow_tasks in
        // one CTE — the lock acquired above by
        // lock_snapshot_for_mutation is still held in this same tx, so
        // all three writes commit together.
        //
        // NOTIFY is deliberately NOT emitted here: `current_task_id`
        // has just been marked completed in `completed_tasks` but the
        // snapshot row's `current_task_id` still points at it (the
        // position advance happens in the immediately-following
        // `save_snapshot` from `commit_task_result`). A NOTIFY here
        // would carry a stale hint — recipients would call
        // `find_hinted_task`, hydrate workflow_tasks, see the task
        // already completed in `completed_tasks`, and bail. On the
        // linear bench this stale half doubled NOTIFY pressure and
        // blew through the 16 384-slot mpmc channel (~9 600 drops on
        // CI), forcing workers onto the slower polling fallback.
        sqlx::query(
            "WITH upd AS (
                 UPDATE sayiir_workflow_snapshots
                 SET status = $1, current_task_id = $2,
                     completed_task_count = $3, history_version = $4,
                     data_hash = $5, updated_at = now()
                 WHERE instance_id = $6
                 RETURNING 1
             ),
             hist AS (
                 INSERT INTO sayiir_workflow_snapshot_history
                     (instance_id, version, status, current_task_id, data, data_hash)
                 VALUES ($6, $4, $1, $2, $7, $5)
                 RETURNING 1
             ),
             task AS (
                 INSERT INTO sayiir_workflow_tasks
                     (instance_id, task_id, status, completed_at, output)
                 VALUES ($6, $8, 'completed', now(), $9)
                 ON CONFLICT (instance_id, task_id) DO UPDATE SET
                     status = 'completed', completed_at = now(), error = NULL,
                     output = EXCLUDED.output
                 RETURNING 1
             )
             -- Anchor the outer SELECT to `upd` so the structural
             -- defence against unreferenced-CTE pruning matches
             -- save_snapshot. upd/hist/task are all DML so PG executes
             -- them unconditionally regardless of references, but any
             -- future refactor that converts one to a non-DML CTE
             -- would silently drop the write without this anchor.
             SELECT 1 FROM upd",
        )
        .bind(status) // $1
        .bind(current) // $2
        .bind(task_count) // $3
        .bind(next_history_version) // $4
        .bind(data_hash.as_slice()) // $5
        .bind(instance_id) // $6
        .bind(&data) // $7
        .bind(task_id.as_bytes().as_slice()) // $8
        .bind(output_bytes.as_ref()) // $9
        .execute(&mut *tx)
        .await
        .map_err(PgError)?;

        tx.commit().await.map_err(PgError)?;
        Ok(())
    }
}

impl<C> SnapshotStore for PostgresBackend<C>
where
    C: codec::SnapshotCodec,
{
    #[tracing::instrument(
        name = "db.save_snapshot",
        skip(self, snapshot),
        fields(
            db.system = "postgresql",
            instance_id = %snapshot.instance_id,
            status = %snapshot.state.as_ref(),
        ),
        err(level = tracing::Level::ERROR),
    )]
    async fn save_snapshot(&self, snapshot: &mut WorkflowSnapshot) -> Result<(), BackendError> {
        self.exec_save_snapshot(&self.pool, snapshot).await
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
        task_id: &sayiir_core::TaskId,
        output: bytes::Bytes,
    ) -> Result<(), BackendError> {
        tracing::debug!("saving task result");
        // The transaction rolls back on failure, so re-running on a
        // serialization failure / deadlock / connection drop is safe.
        with_transient_retry("save_task_result", || {
            self.save_task_result_once(instance_id, task_id, output.clone())
        })
        .await
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
            "SELECT history_version, trace_parent
             FROM sayiir_workflow_snapshots
             WHERE instance_id = $1",
        )
        .bind(instance_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(PgError)?
        .ok_or_else(|| BackendError::NotFound(instance_id.to_string()))?;

        let history_version: i32 = row.get("history_version");
        let data = self
            .fetch_blob(&self.pool, instance_id, history_version)
            .await?;
        let mut snapshot = self.decode(&data)?;
        snapshot.trace_parent = row.get("trace_parent");
        let outputs = crate::history::fetch_task_outputs(&self.pool, instance_id).await?;
        snapshot.hydrate_task_outputs(outputs);
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

        // One round-trip: child-table DELETEs and the snapshot DELETE are
        // sibling DML CTEs (always executed), atomic without an explicit tx.
        let child_ctes = crate::child_delete_ctes("instance_id = $1");
        let query = format!(
            "WITH {child_ctes}
             del AS (DELETE FROM sayiir_workflow_snapshots WHERE instance_id = $1 RETURNING 1)
             SELECT EXISTS (SELECT 1 FROM del) AS deleted"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(query))
            .bind(instance_id)
            .fetch_one(&self.pool)
            .await
            .map_err(PgError)?;

        if !row.get::<bool, _>("deleted") {
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
        let rows =
            sqlx::query("SELECT instance_id FROM sayiir_workflow_snapshots ORDER BY instance_id")
                .fetch_all(&self.pool)
                .await
                .map_err(PgError)?;

        Ok(rows.iter().map(|r| r.get("instance_id")).collect())
    }
}

impl<C> PostgresBackend<C> {
    /// Keyset-paginated variant of `list_snapshots` for large tables:
    /// returns up to `limit` instance ids strictly after `after` (pass the
    /// last id of the previous page; `None` starts from the beginning).
    ///
    /// # Errors
    ///
    /// Returns an error if the query fails.
    pub async fn list_snapshots_page(
        &self,
        after: Option<&str>,
        limit: i64,
    ) -> Result<Vec<String>, BackendError> {
        let rows = sqlx::query(
            "SELECT instance_id FROM sayiir_workflow_snapshots
             WHERE ($1::text IS NULL OR instance_id > $1)
             ORDER BY instance_id
             LIMIT $2",
        )
        .bind(after)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(PgError)?;
        Ok(rows.iter().map(|r| r.get("instance_id")).collect())
    }
}
