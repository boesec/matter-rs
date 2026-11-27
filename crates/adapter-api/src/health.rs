//! The health of an adapter instance.

use alloc::string::String;
use core::fmt;
use core::time::Duration;

/// The health of an adapter instance, as the adapter reports it and as the host observes it.
///
/// The adapter reports [`Starting`](Self::Starting), [`Healthy`](Self::Healthy),
/// [`Degraded`](Self::Degraded) and [`Failed`](Self::Failed) through its context. The host adds
/// what it sees from outside: [`Restarting`](Self::Restarting) between a failure and the next
/// attempt, [`Failed`](Self::Failed) when it gives up, [`Stopped`](Self::Stopped) when the
/// adapter ended. The texts are for the operator and carry no secret.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Health {
    /// The adapter runs but has not reported yet; the state of every new run.
    Starting,
    /// The adapter serves its devices.
    Healthy,
    /// The adapter runs with a problem it can describe, for example a lost connection it is
    /// trying to restore.
    Degraded(String),
    /// The adapter does not work and will not recover on its own.
    Failed(String),
    /// The adapter failed and the host restarts it after a delay.
    Restarting {
        /// The number of this restart attempt, starting at 1 after the first failure.
        attempt: u32,
        /// The delay before the attempt.
        next_in: Duration,
    },
    /// The adapter ended and is not restarted: it returned on its own, or the host shut it down.
    Stopped,
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => f.write_str("starting"),
            Self::Healthy => f.write_str("healthy"),
            Self::Degraded(reason) => write!(f, "degraded: {reason}"),
            Self::Failed(reason) => write!(f, "failed: {reason}"),
            Self::Restarting { attempt, next_in } => {
                write!(f, "restarting (attempt {attempt} in {next_in:?})")
            }
            Self::Stopped => f.write_str("stopped"),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::string::{String, ToString};

    use super::Health;

    #[test]
    fn health_displays_for_the_operator() {
        assert_eq!(Health::Starting.to_string(), "starting");
        assert_eq!(Health::Healthy.to_string(), "healthy");
        assert_eq!(
            Health::Degraded(String::from("broker unreachable")).to_string(),
            "degraded: broker unreachable"
        );
        assert_eq!(
            Health::Failed(String::from("bad topic")).to_string(),
            "failed: bad topic"
        );
        assert_eq!(
            Health::Restarting {
                attempt: 2,
                next_in: core::time::Duration::from_secs(2),
            }
            .to_string(),
            "restarting (attempt 2 in 2s)"
        );
        assert_eq!(Health::Stopped.to_string(), "stopped");
    }
}
