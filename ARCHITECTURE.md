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

Codecs implement the `Encoder<T>` and `Decoder<T>` traits from `workflow-core::codec` to convert between typed values and byte streams.

**Built-in codecs:**

The `workflow-runtime` crate provides built-in codec implementations:

- `RkyvCodec` - **Default** (zero-copy deserialization framework) - enabled by default via `rkyv` feature
- `JsonCodec` - Available when `json` feature is enabled (uses serde_json)

The `rkyv` feature is enabled by default. To use JSON instead, disable default features and enable `json`: `--no-default-features --features json`.

**Custom serialization formats**

The architecture is not opinionated about serialization formats. Users can implement custom codecs by implementing the `Encoder<T>` and `Decoder<T>` traits from `workflow-core::codec`. The runtime can then use these codecs to serialize/deserialize task inputs and outputs as needed.

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
