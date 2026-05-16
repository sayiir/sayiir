//! Task priority levels for execution ordering.
//!
//! Priority determines the order in which tasks are picked up by workers.
//! Lower numeric values indicate higher priority. The default is [`Normal`](Priority::Normal) (3).

use serde::{Deserialize, Serialize};

/// Execution priority for a task.
///
/// Lower numeric values are executed first. Workers use this value
/// (combined with aging) to decide which available task to claim next.
///
/// # Examples
///
/// ```
/// use sayiir_core::priority::Priority;
///
/// assert_eq!(Priority::default(), Priority::Normal);
/// assert_eq!(Priority::Critical.as_u8(), 1);
/// assert!(Priority::Critical < Priority::Low);
/// ```
#[repr(u8)]
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub enum Priority {
    /// Highest priority (1). Use sparingly for time-critical work.
    Critical = 1,
    /// High priority (2).
    High = 2,
    /// Default priority (3).
    #[default]
    Normal = 3,
    /// Low priority (4). Background work.
    Low = 4,
    /// Lowest priority (5). Best-effort, may be starved without aging.
    Minimal = 5,
}

impl Priority {
    /// Convert a `u8` to a `Priority`, returning `None` for out-of-range values.
    #[must_use]
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            1 => Some(Self::Critical),
            2 => Some(Self::High),
            3 => Some(Self::Normal),
            4 => Some(Self::Low),
            5 => Some(Self::Minimal),
            _ => None,
        }
    }

    /// Return the numeric value of this priority (1–5).
    #[must_use]
    pub fn as_u8(self) -> u8 {
        self as u8
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn default_is_normal() {
        assert_eq!(Priority::default(), Priority::Normal);
        assert_eq!(Priority::default().as_u8(), 3);
    }

    #[test]
    fn round_trip() {
        for v in 1..=5 {
            let p = Priority::from_u8(v).unwrap();
            assert_eq!(p.as_u8(), v);
        }
    }

    #[test]
    fn out_of_range() {
        assert!(Priority::from_u8(0).is_none());
        assert!(Priority::from_u8(6).is_none());
    }

    #[test]
    fn ordering() {
        assert!(Priority::Critical < Priority::High);
        assert!(Priority::High < Priority::Normal);
        assert!(Priority::Normal < Priority::Low);
        assert!(Priority::Low < Priority::Minimal);
    }
}
