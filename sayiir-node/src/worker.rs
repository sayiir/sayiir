//! Node.js-exposed distributed worker.
//!
//! The worker wraps the Rust `PooledWorker` to provide distributed task
//! execution from Node.js. Unlike the simple engine which runs synchronously,
//! the worker spawns a background task that polls for available work.
//!
//! **Note:** The distributed worker requires a backend that supports task
//! claiming (currently `PostgresBackend`). The `InMemoryBackend` can be used
//! for testing but doesn't support cross-process coordination.

// Phase 5 implementation will be added here.
// The distributed worker is complex and requires careful bridging between
// the Rust PooledWorker's typed system and the JS callback-based approach.
//
// Key challenges:
// - PooledWorker requires Codec + TaskRegistry generics
// - Task execution needs ThreadsafeFunction for cross-thread JS calls
// - Worker lifecycle (spawn/shutdown/join) needs async bridging
//
// For now, users can use the durable engine directly for single-process
// execution with checkpointing, which covers most use cases.
