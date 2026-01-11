# Architecture Draft

## Core workflow design choices

### Typed tasks with runtime serialization

- Tasks are strongly typed with `Input` and `Output` associated types via the `CoreTask` trait
- Tasks operate on typed values in memory - no serialization overhead during task execution
- Serialization to/from `Bytes` happens at runtime boundaries (e.g., when tasks are distributed or checkpointed)
- This separation allows tasks to be written with natural Rust types while maintaining flexibility for distributed execution

Using `anyhow::Result` is a relevant choice because:

- Checkpointing errors need serialization — we'll stringify regardless
- User tasks have diverse error types
- Context/backtrace for debugging workflows

### Serialization

Serialization is a runtime execution concern, not a task definition concern. Tasks are defined with typed inputs and outputs, and codecs handle the conversion between typed values and `Bytes` when needed (e.g., for distributed execution, checkpointing, or persistence).

**Runtime codec selection**

Each workflow uses a single codec for all serialization operations. The codec is configured when the workflow is created and is used consistently across all task boundaries (input/output serialization, checkpointing, etc.). This ensures:

- Consistency: All serialized data in a workflow uses the same format
- Simplicity: No need for codec registry or per-task codec selection
- Type safety: All task `Input`/`Output` types must satisfy the workflow's codec trait bounds

Codecs implement the `Encoder` and `Decoder` traits from `workflow-core::codec` to convert between typed values and byte streams. The type parameter is specified at the method call site rather than the trait level, allowing for more flexible usage.

**Built-in codecs:**

The `workflow-runtime` crate provides built-in codec implementations:

- `RkyvCodec` - **Default** (zero-copy deserialization framework) - enabled by default via `rkyv` feature
- `JsonCodec` - Available when `json` feature is enabled (uses serde_json)

The `rkyv` feature is enabled by default. To use JSON instead, disable default features and enable `json`: `--no-default-features --features json`.

**Custom serialization formats**

The architecture is not opinionated about serialization formats. Users can implement custom codecs by implementing the `Encoder` and `Decoder` traits from `workflow-core::codec`. To implement a codec, you need to:
1. Implement the `Encoder` or `Decoder` trait (empty impl is fine)
2. Implement `sealed::EncodeValue<T>` or `sealed::DecodeValue<T>` with your desired type bounds

The runtime can then use these codecs to serialize/deserialize task inputs and outputs as needed.

Available serialization options in the Rust ecosystem that could be integrated:

✅ True Serde-based serializers

- serde_json - **Built-in via `json` feature**
- [postcard](https://crates.io/crates/postcard) - compact binary format
- serde_cbor - CBOR format
- rmp-serde (msgpack) - MessagePack format
- bincode - compact binary format
- flexbuffers (partial)

❌ Non-fully Serde

- prost / protobuf
- avro-rs
- rkyv - **Built-in via `rkyv` feature** (zero-copy deserialization framework)

### Task Registry and Composability

The `TaskRegistry` maps task IDs to their implementations, enabling workflow serialization and extensibility.

**Registry as Code, Not Data**

The registry contains closures/functions and cannot be serialized. Both serializing and deserializing sides must build the same registry from code. This is the standard pattern in workflow engines.

```rust
// Shared function - called on both sides
fn build_registry(codec: Arc<MyCodec>) -> TaskRegistry {
    let mut registry = TaskRegistry::new();
    registry.register("step1", codec.clone(), |i: u32| async move { Ok(i + 1) });
    registry
}

// Serialization side
let workflow = WorkflowBuilder::new(ctx)
    .with_existing_registry(build_registry(codec.clone()))
    .then_registered::<u32>("step1")
    .build()?;
let serialized = serde_json::to_string(&workflow.to_serializable())?;

// Deserialization side (possibly different process)
let registry = build_registry(codec.clone());
let continuation = serde_json::from_str::<SerializableContinuation>(&serialized)?;
let runnable = continuation.to_runnable(&registry)?;
```

**Layered Composition**

Registries enable extension and composition of task libraries:

```rust
// Core library provides base activities
pub fn core_registry(codec: Arc<C>) -> TaskRegistry {
    TaskRegistry::with_codec(codec)
        .register("core::sleep", activities::sleep)
        .register("core::delay", activities::delay)
        .register("core::http_get", activities::http_get)
        .register("core::send_email", activities::send_email)
        .build()
}

// Domain module extends with business logic
pub fn payments_registry(codec: Arc<C>) -> TaskRegistry {
    TaskRegistry::with_codec(codec)
        .register("core::sleep", activities::sleep)
        .register("core::delay", activities::delay)
        .register("core::http_get", activities::http_get)
        .register("core::send_email", activities::send_email)
        .register("payments::charge", charge_card)
        .register("payments::refund", refund)
        .build()
}

// User composes workflow using all available activities
let workflow = WorkflowBuilder::new(ctx)
    .with_existing_registry(payments_registry(codec))
    .then_registered::<Response>("core::http_get")
    .then_registered::<PaymentResult>("payments::charge")
    .then_registered::<()>("core::send_email")
    .then("custom_logic", |r| async move { ... })  // inline custom
    .build()?;
```

This enables:

- **Core libraries** providing standard activities (sleep, HTTP, email, etc.)
- **Domain modules** with business-specific tasks (payments, notifications)
- **Plugin systems** via dynamically loaded registries
- **Testing** by swapping real tasks with mocks via different registries
- **Mixed composition** of pre-registered and inline custom tasks

The registry becomes the extension point - like Temporal/Cadence's activity model but with compile-time type safety.
