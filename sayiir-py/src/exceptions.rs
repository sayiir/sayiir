//! Custom Python exception hierarchy for Sayiir.
//!
//! - `WorkflowError` — base (extends `RuntimeError`)
//! - `TaskError` — task execution failure
//! - `BackendError` — persistence failure

use pyo3::create_exception;
use pyo3::exceptions::PyRuntimeError;

create_exception!(
    _sayiir,
    WorkflowError,
    PyRuntimeError,
    "Base exception for Sayiir workflow errors."
);
create_exception!(
    _sayiir,
    TaskError,
    WorkflowError,
    "A task failed during execution."
);
create_exception!(
    _sayiir,
    BackendError,
    WorkflowError,
    "A persistence backend operation failed."
);
create_exception!(
    _sayiir,
    DeserializationError,
    WorkflowError,
    "Schema mismatch: failed to decode a task input or output."
);
create_exception!(
    _sayiir,
    InstanceAlreadyExistsError,
    WorkflowError,
    "A workflow instance with this ID already exists."
);
