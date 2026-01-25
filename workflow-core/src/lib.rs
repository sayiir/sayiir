#![deny(clippy::pedantic)]
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo,
    clippy::unimplemented,
    clippy::dbg_macro,
    clippy::print_stdout,
    clippy::print_stderr
)]

pub mod codec;
pub mod context;
pub mod error;
pub mod registry;
pub mod snapshot;
pub mod task;
pub mod task_claim;
pub mod workflow;
