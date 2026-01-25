//! Channel-based task wrapper that implements CoreTask.
//!
//! This module provides the bridge between Rust's CoreTask trait and Python
//! task execution. When a task needs to run, it sends a request through the
//! channel and waits for Python to execute and respond.

use anyhow::Result;
use bytes::Bytes;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use workflow_core::task::CoreTask;

use crate::channel::{create_request, TaskChannel, TaskResponse};
use crate::codec::SerializerKind;

/// A task wrapper that executes Python tasks via the channel mechanism.
///
/// This implements CoreTask by:
/// 1. Sending a TaskRequest to the channel with the task ID and input
/// 2. Waiting for Python to execute the task and submit a response
/// 3. Returning the result or error from Python
pub struct ChannelTaskWrapper {
    /// The task identifier used to look up the Python callable
    task_id: String,
    /// Shared channel for communication with Python
    channel: Arc<TaskChannel>,
}

impl ChannelTaskWrapper {
    /// Create a new ChannelTaskWrapper.
    pub fn new(task_id: String, channel: Arc<TaskChannel>) -> Self {
        Self { task_id, channel }
    }
}

/// Named future for channel task execution.
///
/// This is a manually implemented Future that avoids Box::pin allocation.
/// The state machine is simple: send request, then poll for response.
pub struct ChannelTaskFuture {
    /// The task identifier for error messages
    task_id: String,
    /// Shared channel for communication
    channel: Arc<TaskChannel>,
    /// Request ID for correlating response
    request_id: u64,
    /// Current state of the future
    state: ChannelTaskState,
}

enum ChannelTaskState {
    /// Initial state - need to send the request
    Init {
        input: Bytes,
        serializer_kind: SerializerKind,
    },
    /// Request sent, waiting for response
    Waiting,
    /// Future has completed (prevents double-polling)
    Complete,
}

impl Future for ChannelTaskFuture {
    type Output = Result<Bytes>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match &mut self.state {
                ChannelTaskState::Init {
                    input,
                    serializer_kind,
                } => {
                    // Take ownership of input bytes
                    let input = std::mem::take(input);
                    let serializer_kind = *serializer_kind;

                    // Create and send the request
                    let request = create_request(
                        self.request_id,
                        self.task_id.clone(),
                        input,
                        serializer_kind,
                    );

                    if let Err(e) = self.channel.send_request(request) {
                        self.state = ChannelTaskState::Complete;
                        return Poll::Ready(Err(e));
                    }

                    // Transition to waiting state
                    self.state = ChannelTaskState::Waiting;
                }
                ChannelTaskState::Waiting => {
                    // Check if response is available
                    if let Some(response) = self.channel.try_recv_response(self.request_id) {
                        self.state = ChannelTaskState::Complete;
                        return Poll::Ready(Self::process_response(&self.task_id, response));
                    }

                    // Register waker and return pending
                    self.channel
                        .register_waker(self.request_id, cx.waker().clone());
                    return Poll::Pending;
                }
                ChannelTaskState::Complete => {
                    panic!("ChannelTaskFuture polled after completion");
                }
            }
        }
    }
}

impl ChannelTaskFuture {
    /// Process the task response into a Result.
    fn process_response(task_id: &str, response: TaskResponse) -> Result<Bytes> {
        response
            .result
            .map_err(|e| anyhow::anyhow!("Task '{}' failed: {}", task_id, e))
    }
}

impl CoreTask for ChannelTaskWrapper {
    type Input = Bytes;
    type Output = Bytes;
    type Future = ChannelTaskFuture;

    fn run(&self, input: Bytes) -> Self::Future {
        let request_id = self.channel.next_request_id();
        let serializer_kind = self.channel.serializer_kind();

        ChannelTaskFuture {
            task_id: self.task_id.clone(),
            channel: self.channel.clone(),
            request_id,
            state: ChannelTaskState::Init {
                input,
                serializer_kind,
            },
        }
    }
}

/// Create an untyped core task from a ChannelTaskWrapper.
#[allow(dead_code)]
pub fn channel_task(task_id: String, channel: Arc<TaskChannel>) -> Box<ChannelTaskWrapper> {
    Box::new(ChannelTaskWrapper::new(task_id, channel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_channel_task_creation() {
        let channel = Arc::new(TaskChannel::default());
        let task = ChannelTaskWrapper::new("test".to_string(), channel);
        assert_eq!(task.task_id, "test");
    }
}
