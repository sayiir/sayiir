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
