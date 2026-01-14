# workflow-persistence

Persistence layer for distributed workflow execution with checkpoint/restore capabilities.

## Overview

This crate provides traits and implementations for persisting workflow execution state, enabling:

- **Distributed execution**: Multiple worker nodes can execute tasks from the same workflow
- **Fault tolerance**: Workflows can be resumed after crashes
- **Task claiming**: Atomic task claiming prevents duplicate execution
- **Flexible backends**: Implement custom storage (Redis, PostgreSQL, etc.)

## Quick Start

```rust
use workflow_persistence::{InMemoryBackend, PersistentBackend};
use workflow_core::snapshot::WorkflowSnapshot;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create a backend
    let backend = InMemoryBackend::new();
    
    // Save a workflow snapshot
    let snapshot = WorkflowSnapshot::new(
        "instance-123".to_string(),
        "workflow-hash".to_string()
    );
    backend.save_snapshot(snapshot).await?;
    
    // Load it back
    let loaded = backend.load_snapshot("instance-123").await?;
    println!("Loaded workflow: {}", loaded.instance_id);
    
    Ok(())
}
```

## Implementing Custom Backends

To create a custom persistence backend (e.g., for Redis, PostgreSQL, or any other storage):

### 1. Add Dependencies

```toml
[dependencies]
workflow-persistence = { .. }
workflow-core = { .. }
async-trait = { .. }
```

### 2. Implement `PersistentBackend`

```rust
use workflow_persistence::{PersistentBackend, BackendError};
use workflow_core::snapshot::WorkflowSnapshot;
use workflow_core::task_claim::{TaskClaim, AvailableTask};
use async_trait::async_trait;
use chrono::Duration;

pub struct RedisBackend {
    client: redis::Client,
}

#[async_trait]
impl PersistentBackend for RedisBackend {
    async fn save_snapshot(&self, snapshot: WorkflowSnapshot) -> Result<(), BackendError> {
        // Serialize and save to Redis
        let serialized = serde_json::to_vec(&snapshot)
            .map_err(|e| BackendError::Serialization(e.to_string()))?;
        
        self.client
            .set(&format!("workflow:{}", snapshot.instance_id), serialized)
            .await
            .map_err(|e| BackendError::Backend(e.to_string()))?;
        
        Ok(())
    }
    
    async fn load_snapshot(&self, instance_id: &str) -> Result<WorkflowSnapshot, BackendError> {
        let data: Vec<u8> = self.client
            .get(&format!("workflow:{}", instance_id))
            .await
            .map_err(|_| BackendError::NotFound(instance_id.to_string()))?;
        
        serde_json::from_slice(&data)
            .map_err(|e| BackendError::Serialization(e.to_string()))
    }
    
    // Implement other methods...
    async fn delete_snapshot(&self, instance_id: &str) -> Result<(), BackendError> {
        // ...
    }
    
    async fn list_snapshots(&self) -> Result<Vec<String>, BackendError> {
        // ...
    }
    
    async fn claim_task(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        ttl: Option<Duration>,
    ) -> Result<Option<TaskClaim>, BackendError> {
        // Use Redis SETNX for atomic claiming
        // ...
    }
    
    async fn release_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
    ) -> Result<(), BackendError> {
        // ...
    }
    
    async fn extend_task_claim(
        &self,
        instance_id: &str,
        task_id: &str,
        worker_id: &str,
        additional_duration: Duration,
    ) -> Result<(), BackendError> {
        // ...
    }
    
    async fn find_available_tasks(
        &self,
        worker_id: &str,
        limit: usize,
    ) -> Result<Vec<AvailableTask>, BackendError> {
        // Query for unclaimed, ready-to-execute tasks
        // ...
    }
}
```

## Key Concepts

### Snapshots

Snapshots capture the complete execution state of a workflow:

- Which tasks have completed
- Task outputs
- Current execution position
- Initial workflow input

### Task Claims

Task claims enable distributed execution:

- **Atomic claiming**: Only one worker can claim a task
- **Expiration**: Claims can have TTLs to prevent stuck tasks
- **Extension**: Long-running tasks can extend their claims
- **Release**: Workers release claims when done

### Backend Requirements

A production backend should provide:

- **Durability**: Snapshots survive process crashes
- **Atomicity**: Task claims must be atomic (use database transactions, Redis SETNX, etc.)
- **Consistency**: Multiple workers see consistent state
- **Performance**: Efficient querying for available tasks

## Built-in Backends

### InMemoryBackend

Provided for testing and development:

- No persistence across restarts
- Good for unit tests and prototyping

## Architecture

```
┌─────────────────────┐
│  Workflow Runtime   │
└──────────┬──────────┘
           │
           ▼
┌─────────────────────┐
│ PersistentBackend   │ ◄─── Trait you implement
│      (trait)        │
└──────────┬──────────┘
           │
     ┌─────┴─────┬───────────┬──────────────┐
     ▼           ▼           ▼              ▼
┌─────────┐ ┌─────────┐ ┌─────────┐   ┌─────────┐
│ Memory  │ │  Redis  │ │Postgres │   │  Your   │
│ Backend │ │ Backend │ │ Backend │   │ Backend │
└─────────┘ └─────────┘ └─────────┘   └─────────┘
```
