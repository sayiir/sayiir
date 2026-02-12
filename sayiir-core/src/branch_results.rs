//! Named branch results — the outputs of parallel fork branches.

use bytes::Bytes;
use std::collections::HashMap;

/// Named branch results — the outputs of parallel fork branches.
///
/// Wraps `Vec<(String, Bytes)>` and implements `serde::Serialize` /
/// `serde::Deserialize`, so any codec that handles serde types can
/// encode and decode it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NamedBranchResults(Vec<(String, Bytes)>);

impl NamedBranchResults {
    /// Create from a vec of `(name, data)` pairs.
    #[must_use]
    pub fn new(results: Vec<(String, Bytes)>) -> Self {
        Self(results)
    }

    /// View the underlying pairs.
    #[must_use]
    pub fn as_slice(&self) -> &[(String, Bytes)] {
        &self.0
    }

    /// Number of branch results.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are no results.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Consume into the inner vec.
    #[must_use]
    pub fn into_vec(self) -> Vec<(String, Bytes)> {
        self.0
    }

    /// Convert to a lookup map. When duplicate names exist, the last value wins.
    #[must_use]
    pub fn into_map(self) -> HashMap<String, Bytes> {
        self.0.into_iter().collect()
    }
}

impl From<Vec<(String, Bytes)>> for NamedBranchResults {
    fn from(results: Vec<(String, Bytes)>) -> Self {
        Self(results)
    }
}
