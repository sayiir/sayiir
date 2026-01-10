# Architecture Draft

## Core workflow design choices

### Minimal typing overhead

- in/out data for tasks are just `Bytes`. Computations and IO are all about processing bytes!
- Errors cross task boundaries, types won't align anyway
- Traits associated types add significant overhead and gymanastics.

Using `anyhow::Result` is a relevant choice because:

- Checkpointing errors need serialization — we'll stringify regardless
- User tasks have diverse error types
- Context/backtrace for debugging workflows

### Serialization

As workflows are distributed, serializing/deserializing activities in/out is needed.

**Default: JSON (serde_json)**

The `task` macro defaults to JSON serialization via `Json<T>` wrapper, providing a clean API where users write functions with plain types that are automatically wrapped/unwrapped. This makes JSON the default choice for most use cases.

**Flexible: Any serialization format**

The architecture is not opinionated - nothing prevents using any other serialization format. Users can:

- Use custom wrappers that implement `TaskInput`/`TaskOutput` (e.g., `Bincode<T>`, `Postcard<T>`)
- The macro automatically detects these wrappers and won't double-wrap them
- Opt out of automatic wrapping entirely with `#[task(custom_serialization)]`

Available serialization options in the Rust ecosystem:

✅ True Serde-based serializers

- serde_json (default)
- [postcard](https://crates.io/crates/postcard) - compact binary format
- serde_cbor - CBOR format
- rmp-serde (msgpack) - MessagePack format
- bincode - compact binary format
- flexbuffers (partial)

❌ Non-fully Serde

- prost / protobuf
- avro-rs
- rkyv

All of these can be used by implementing `TaskInput` and `TaskOutput` traits on custom wrapper types.
