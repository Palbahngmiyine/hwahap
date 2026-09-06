//! Time, injected rather than read.
//!
//! Timestamps land in the plan, in answers, and in the journal's hash chain, so "what time is it"
//! is an input to digests the user retypes. Tests pin it; production reads the system clock.

/// Source of the RFC 3339 UTC timestamps written into durable state.
pub trait Clock: Send + Sync {
    /// The current instant, as `YYYY-MM-DDTHH:MM:SSZ`.
    ///
    /// Second resolution: sub-second digits would add entropy to digests without adding ordering
    /// that the journal's sequence number does not already provide.
    fn now(&self) -> String;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> String {
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
    }
}

/// A clock that returns one fixed instant, for tests.
#[derive(Debug, Clone)]
pub struct FixedClock(String);

impl FixedClock {
    pub fn new(ts: impl Into<String>) -> Self {
        FixedClock(ts.into())
    }
}

impl Clock for FixedClock {
    fn now(&self) -> String {
        self.0.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_clock_emits_second_resolution_rfc3339_utc() {
        let now = SystemClock.now();
        assert_eq!(now.len(), 20, "unexpected shape: {now}");
        assert!(now.ends_with('Z'), "{now}");
        assert_eq!(now.as_bytes()[10], b'T', "{now}");
        chrono::DateTime::parse_from_rfc3339(&now).expect("must parse as RFC 3339");
    }

    #[test]
    fn a_fixed_clock_never_moves() {
        let clock = FixedClock::new("2026-09-04T00:00:00Z");
        assert_eq!(clock.now(), "2026-09-04T00:00:00Z");
        assert_eq!(clock.now(), clock.now());
    }

    #[test]
    fn clocks_are_usable_as_trait_objects() {
        let clocks: Vec<Box<dyn Clock>> =
            vec![Box::new(SystemClock), Box::new(FixedClock::new("t"))];
        assert_eq!(clocks.len(), 2);
        assert_eq!(clocks[1].now(), "t");
    }
}
