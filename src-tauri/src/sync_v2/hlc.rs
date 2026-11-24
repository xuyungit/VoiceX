//! Hybrid Logical Clock (HLC) for total ordering of sync operations.
//!
//! Serializes to fixed-width `"{wall_ms:016}-{counter:06}-{device_id}"` so
//! standard lexicographic SQLite `ORDER BY hlc` matches structural `Ord`.

use std::cmp::Ordering;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;

/// Maximum forward clock skew (24 hours) a remote HLC may pull the local
/// physical clock ahead; beyond this we log a warning and clamp `observe()`
/// advancement while still preserving the remote HLC for comparison.
pub const MAX_CLOCK_DRIFT_MS: i64 = 24 * 60 * 60 * 1000;

/// Maximum valid counter fitting in the 6-digit fixed-width encoding.
pub const MAX_HLC_COUNTER: u32 = 999_999;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HlcError {
    #[error("invalid HLC format: {0}")]
    InvalidFormat(String),
    #[error("HLC device_id must not be empty")]
    EmptyDeviceId,
    #[error("HLC wall_ms must be non-negative, got {0}")]
    NegativeWallMs(i64),
    #[error("HLC counter overflowed {MAX_HLC_COUNTER}")]
    CounterOverflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Hlc {
    pub wall_ms: i64,
    pub counter: u32,
    pub device_id: String,
}

impl Hlc {
    pub fn new(wall_ms: i64, counter: u32, device_id: impl Into<String>) -> Result<Self, HlcError> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() {
            return Err(HlcError::EmptyDeviceId);
        }
        if wall_ms < 0 {
            return Err(HlcError::NegativeWallMs(wall_ms));
        }
        if counter > MAX_HLC_COUNTER {
            return Err(HlcError::CounterOverflow);
        }
        Ok(Self {
            wall_ms,
            counter,
            device_id,
        })
    }

    pub fn to_wire_string(&self) -> String {
        format!("{:016}-{:06}-{}", self.wall_ms, self.counter, self.device_id)
    }
}

impl Ord for Hlc {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then_with(|| self.counter.cmp(&other.counter))
            .then_with(|| self.device_id.cmp(&other.device_id))
    }
}

impl PartialOrd for Hlc {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Hlc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:016}-{:06}-{}",
            self.wall_ms, self.counter, self.device_id
        )
    }
}

impl FromStr for Hlc {
    type Err = HlcError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let mut parts = s.splitn(3, '-');
        let wall_str = parts
            .next()
            .ok_or_else(|| HlcError::InvalidFormat(s.to_string()))?;
        let counter_str = parts
            .next()
            .ok_or_else(|| HlcError::InvalidFormat(s.to_string()))?;
        let device_id = parts
            .next()
            .ok_or_else(|| HlcError::InvalidFormat(s.to_string()))?;

        if wall_str.len() != 16
            || !wall_str.bytes().all(|b| b.is_ascii_digit())
            || counter_str.len() != 6
            || !counter_str.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(HlcError::InvalidFormat(s.to_string()));
        }

        let wall_ms: i64 = wall_str
            .parse()
            .map_err(|_| HlcError::InvalidFormat(s.to_string()))?;
        let counter: u32 = counter_str
            .parse()
            .map_err(|_| HlcError::InvalidFormat(s.to_string()))?;

        Self::new(wall_ms, counter, device_id)
    }
}

/// Stateful HLC generator persisted per sync space.
#[derive(Debug, Clone)]
pub struct HlcClock {
    device_id: String,
    last_wall_ms: i64,
    last_counter: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObserveOutcome {
    Accepted,
    /// Remote timestamp exceeded `now_ms + MAX_CLOCK_DRIFT_MS`; local clock was
    /// clamped instead of jumping arbitrarily far into the future.
    DriftClamped,
}

impl HlcClock {
    pub fn new(device_id: impl Into<String>, persisted_last: Option<&Hlc>) -> Result<Self, HlcError> {
        let device_id = device_id.into();
        if device_id.trim().is_empty() {
            return Err(HlcError::EmptyDeviceId);
        }
        let (last_wall_ms, last_counter) = match persisted_last {
            Some(hlc) => (hlc.wall_ms, hlc.counter),
            None => (0, 0),
        };
        Ok(Self {
            device_id,
            last_wall_ms,
            last_counter,
        })
    }

    /// Generate the next strictly monotonic HLC for a local event at `now_ms`.
    pub fn tick(&mut self, now_ms: i64) -> Result<Hlc, HlcError> {
        let now_ms = now_ms.max(0);
        if now_ms > self.last_wall_ms {
            self.last_wall_ms = now_ms;
            self.last_counter = 0;
        } else {
            self.last_counter = self
                .last_counter
                .checked_add(1)
                .filter(|&c| c <= MAX_HLC_COUNTER)
                .ok_or(HlcError::CounterOverflow)?;
        }
        Hlc::new(self.last_wall_ms, self.last_counter, self.device_id.clone())
    }

    /// Advance local clock upon receiving a remote HLC.
    pub fn observe(&mut self, remote: &Hlc, now_ms: i64) -> Result<ObserveOutcome, HlcError> {
        let now_ms = now_ms.max(0);
        let max_allowed = now_ms.saturating_add(MAX_CLOCK_DRIFT_MS);
        let (effective_remote_wall, effective_remote_counter, outcome) =
            if remote.wall_ms > max_allowed {
                (max_allowed, 0, ObserveOutcome::DriftClamped)
            } else {
                (remote.wall_ms, remote.counter, ObserveOutcome::Accepted)
            };

        let next_wall = now_ms.max(self.last_wall_ms).max(effective_remote_wall);
        let next_counter = if next_wall == self.last_wall_ms && next_wall == effective_remote_wall {
            self.last_counter
                .max(effective_remote_counter)
                .checked_add(1)
                .filter(|&c| c <= MAX_HLC_COUNTER)
                .ok_or(HlcError::CounterOverflow)?
        } else if next_wall == self.last_wall_ms {
            self.last_counter
                .checked_add(1)
                .filter(|&c| c <= MAX_HLC_COUNTER)
                .ok_or(HlcError::CounterOverflow)?
        } else if next_wall == effective_remote_wall {
            effective_remote_counter
                .checked_add(1)
                .filter(|&c| c <= MAX_HLC_COUNTER)
                .ok_or(HlcError::CounterOverflow)?
        } else {
            0
        };

        self.last_wall_ms = next_wall;
        self.last_counter = next_counter;
        Ok(outcome)
    }

    pub fn last_hlc(&self) -> Hlc {
        Hlc {
            wall_ms: self.last_wall_ms,
            counter: self.last_counter,
            device_id: self.device_id.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_string_round_trip_and_lexicographic_order() {
        let a = Hlc::new(1_795_438_700_000, 0, "dev-a").unwrap();
        let b = Hlc::new(1_795_438_700_000, 1, "dev-a").unwrap();
        let c = Hlc::new(1_795_438_700_000, 1, "dev-b").unwrap();
        let d = Hlc::new(1_795_438_700_001, 0, "dev-a").unwrap();

        let items = [a, b, c, d];
        for item in &items {
            let wire = item.to_wire_string();
            let parsed: Hlc = wire.parse().unwrap();
            assert_eq!(&parsed, item);
        }
        for pair in items.windows(2) {
            assert!(pair[0] < pair[1]);
            assert!(pair[0].to_wire_string() < pair[1].to_wire_string());
        }
    }

    #[test]
    fn parses_uuid_with_hyphens_as_device_id() {
        let hlc = Hlc::new(1_700_000_000_000, 42, "7a99d063-dcab-4062-97f1-f3795d038001").unwrap();
        let parsed: Hlc = hlc.to_wire_string().parse().unwrap();
        assert_eq!(parsed, hlc);
    }

    #[test]
    fn tick_stays_monotonic_when_wall_clock_moves_backward() {
        let mut clock = HlcClock::new("mac", None).unwrap();
        let t1 = clock.tick(10_000).unwrap();
        let t2 = clock.tick(10_000).unwrap();
        let t3 = clock.tick(9_500).unwrap();
        let t4 = clock.tick(10_001).unwrap();

        assert_eq!((t1.wall_ms, t1.counter), (10_000, 0));
        assert_eq!((t2.wall_ms, t2.counter), (10_000, 1));
        assert_eq!((t3.wall_ms, t3.counter), (10_000, 2));
        assert_eq!((t4.wall_ms, t4.counter), (10_001, 0));
        assert!(t1 < t2 && t2 < t3 && t3 < t4);
    }

    #[test]
    fn observe_advances_local_clock_past_remote() {
        let mut clock = HlcClock::new("mac", None).unwrap();
        let _ = clock.tick(10_000).unwrap();

        let remote = Hlc::new(10_500, 7, "win").unwrap();
        assert_eq!(
            clock.observe(&remote, 10_000).unwrap(),
            ObserveOutcome::Accepted
        );
        let next = clock.tick(10_000).unwrap();
        assert!(next > remote);
        assert_eq!(next.wall_ms, 10_500);
        assert_eq!(next.counter, 9);
    }

    #[test]
    fn observe_clamps_excessive_future_drift() {
        let mut clock = HlcClock::new("mac", None).unwrap();
        let _ = clock.tick(10_000).unwrap();
        let far_future = Hlc::new(10_000 + MAX_CLOCK_DRIFT_MS * 10, 3, "broken-clock").unwrap();

        assert_eq!(
            clock.observe(&far_future, 10_000).unwrap(),
            ObserveOutcome::DriftClamped
        );
        let next = clock.tick(10_000).unwrap();
        assert_eq!(next.wall_ms, 10_000 + MAX_CLOCK_DRIFT_MS);
    }

    #[test]
    fn rejects_malformed_wire_strings() {
        assert!(Hlc::from_str("123-0-dev").is_err());
        assert!(Hlc::from_str("0000000000000001-000000-").is_err());
        assert!(Hlc::from_str("not-an-hlc").is_err());
    }
}
