//! Workflow status result exposed to JavaScript.

use bytes::Bytes;
use wasm_bindgen::prelude::*;

use sayiir_core::workflow::{FlatWorkflowStatus, WorkflowStatus};

/// Workflow status result from durable execution.
#[wasm_bindgen]
pub struct WasmWorkflowStatus {
    status: String,
    error: Option<String>,
    reason: Option<String>,
    cancelled_by: Option<String>,
    paused_by: Option<String>,
    output_json: Option<String>,
    wake_at: Option<String>,
    delay_id: Option<String>,
    signal_id: Option<String>,
    signal_name: Option<String>,
}

#[wasm_bindgen]
#[allow(clippy::must_use_candidate)]
impl WasmWorkflowStatus {
    #[wasm_bindgen(getter)]
    pub fn status(&self) -> String {
        self.status.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    #[wasm_bindgen(getter)]
    pub fn reason(&self) -> Option<String> {
        self.reason.clone()
    }

    #[wasm_bindgen(getter, js_name = "cancelledBy")]
    pub fn cancelled_by(&self) -> Option<String> {
        self.cancelled_by.clone()
    }

    #[wasm_bindgen(getter, js_name = "pausedBy")]
    pub fn paused_by(&self) -> Option<String> {
        self.paused_by.clone()
    }

    /// JSON-serialized output (decoded in TypeScript layer).
    #[wasm_bindgen(getter, js_name = "outputJson")]
    pub fn output_json(&self) -> Option<String> {
        self.output_json.clone()
    }

    /// ISO-8601 wake-up timestamp for `waiting` and `awaiting_signal` statuses.
    #[wasm_bindgen(getter, js_name = "wakeAt")]
    pub fn wake_at(&self) -> Option<String> {
        self.wake_at.clone()
    }

    /// Delay step identifier (present when status is `waiting`).
    #[wasm_bindgen(getter, js_name = "delayId")]
    pub fn delay_id(&self) -> Option<String> {
        self.delay_id.clone()
    }

    /// Signal step identifier (present when status is `awaiting_signal`).
    #[wasm_bindgen(getter, js_name = "signalId")]
    pub fn signal_id(&self) -> Option<String> {
        self.signal_id.clone()
    }

    /// Signal name (present when status is `awaiting_signal`).
    #[wasm_bindgen(getter, js_name = "signalName")]
    pub fn signal_name(&self) -> Option<String> {
        self.signal_name.clone()
    }
}

impl WasmWorkflowStatus {
    pub(crate) fn from_core(status: WorkflowStatus, output: Option<Bytes>) -> Self {
        let output_json = output.and_then(|bytes| {
            std::str::from_utf8(&bytes)
                .ok()
                .map(std::string::ToString::to_string)
        });
        let flat = FlatWorkflowStatus::from(status);
        Self {
            status: flat.status,
            error: flat.error,
            reason: flat.reason,
            cancelled_by: flat.cancelled_by,
            paused_by: flat.paused_by,
            output_json,
            wake_at: flat.wake_at,
            delay_id: flat.delay_id,
            signal_id: flat.signal_id,
            signal_name: flat.signal_name,
        }
    }
}
