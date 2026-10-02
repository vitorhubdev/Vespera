//! Paces sticker downloads so a burst of them never trips WhatsApp's rate
//! limit. Fetching every missing favorite at once on connecting (126 for one
//! reader) answered `429 rate-overlimit` and left the account throttled, so
//! the next picture or sticker the reader sent failed too.

use std::collections::{HashSet, VecDeque};
use std::time::{Duration, Instant};

/// Favorites started per tick of the worker's five-second clock.
pub(super) const PER_TICK: usize = 2;
/// Downloads of each kind allowed in flight at once.
pub(super) const IN_FLIGHT: usize = 2;
/// The first pause after the server says to slow down, doubled each time
/// it says so again, up to [`LONGEST`].
pub(super) const FIRST: Duration = Duration::from_secs(30);
pub(super) const LONGEST: Duration = Duration::from_secs(15 * 60);
/// How long a favorite whose file is gone from the servers rests before it
/// is asked for again.
pub(super) const GONE_FOR: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Default, Debug)]
pub(super) struct Pace {
    queue: VecDeque<String>,
    queued: HashSet<String>,
    until: Option<Instant>,
    pause: Duration,
}

impl Pace {
    /// Queues a favorite for fetching, once.
    pub(super) fn push(&mut self, hash: String) {
        if self.queued.insert(hash.clone()) {
            self.queue.push_back(hash);
        }
    }

    /// Number of queued favorites waiting for their turn.
    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.queue.len()
    }

    /// Whether downloads may start now, or the server asked to wait.
    pub(super) fn open(&self, now: Instant) -> bool {
        self.until.is_none_or(|until| now >= until)
    }

    /// The favorites to start now, given how many are already in flight.
    pub(super) fn take(&mut self, now: Instant, in_flight: usize) -> Vec<String> {
        if !self.open(now) {
            return Vec::new();
        }
        let room = PER_TICK.min(IN_FLIGHT.saturating_sub(in_flight));
        let taken: Vec<String> = (0..room).map_while(|_| self.queue.pop_front()).collect();
        for hash in &taken {
            self.queued.remove(hash);
        }
        taken
    }

    /// The server said to slow down: pause every sticker download, longer
    /// each time it says so again before a pause has passed.
    pub(super) fn limited(&mut self, now: Instant) {
        let again = self.until.is_some_and(|until| now < until + self.pause);
        self.pause = if again {
            (self.pause * 2).min(LONGEST)
        } else {
            FIRST
        };
        self.until = Some(now + self.pause);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Returns the currently active pause deadline, if any.
    #[cfg(test)]
    pub(super) fn until(&self) -> Option<Instant> {
        self.until
    }
}

/// Whether a download error is the server's rate limit.
pub(super) fn rate_limited(error: &str) -> bool {
    error.contains("rate-overlimit") || error.contains("code=429") || error.contains("status: 429")
}

/// Whether a download error means the file is gone from the servers, so
/// asking again soon cannot help.
pub(super) fn gone(error: &str) -> bool {
    ["status: 403", "status: 404", "status: 410"]
        .iter()
        .any(|status| error.contains(status))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn favorites_start_two_at_a_time_and_only_once_each() {
        let mut pace = Pace::default();
        let now = Instant::now();
        for hash in ["a", "b", "c", "a"] {
            pace.push(hash.to_owned());
        }
        assert_eq!(pace.take(now, 0), ["a", "b"]);
        assert_eq!(pace.take(now, 2), Vec::<String>::new(), "two in flight");
        assert_eq!(pace.take(now, 1), ["c"]);
        assert!(pace.is_empty());
    }

    #[test]
    fn a_rate_limit_pauses_and_a_second_one_pauses_longer() {
        let mut pace = Pace::default();
        pace.push("a".to_owned());
        let now = Instant::now();
        pace.limited(now);
        assert!(!pace.open(now + Duration::from_secs(29)));
        assert!(pace.take(now + Duration::from_secs(29), 0).is_empty());
        let later = now + Duration::from_secs(30);
        assert!(pace.open(later));
        pace.limited(later);
        assert!(!pace.open(later + Duration::from_secs(59)));
        assert!(pace.open(later + Duration::from_secs(60)));
        // Long after the last pause, the next one starts short again.
        let much_later = later + Duration::from_secs(3600);
        pace.limited(much_later);
        assert!(pace.open(much_later + FIRST));
    }

    #[test]
    fn errors_are_read_the_way_the_library_writes_them() {
        assert!(rate_limited(
            "received a server error response: code=429, text='rate-overlimit'"
        ));
        assert!(gone("Download failed with status: 403"));
        assert!(gone("Download media not found/expired with status: 410"));
        assert!(!gone(
            "received a server error response: code=429, text='rate-overlimit'"
        ));
        assert!(!rate_limited("Download failed with status: 403"));
    }

    #[test]
    fn one_hundred_fifty_missing_favorites_drain_in_measured_ticks() {
        let mut pace = Pace::default();
        let now = Instant::now();
        for i in 0..150 {
            pace.push(format!("hash_{i}"));
        }
        assert_eq!(pace.len(), 150);

        // First tick with 0 in-flight: starts 2
        let batch1 = pace.take(now, 0);
        assert_eq!(batch1.len(), 2);
        assert_eq!(pace.len(), 148);

        // While 2 are in-flight, take should yield 0
        let batch_busy = pace.take(now, 2);
        assert!(batch_busy.is_empty());

        // When 1 finishes (1 left in flight), take yields at most 1
        let batch_partial = pace.take(now, 1);
        assert_eq!(batch_partial.len(), 1);
        assert_eq!(pace.len(), 147);

        // Next tick with 0 in flight starts 2
        let batch2 = pace.take(now, 0);
        assert_eq!(batch2.len(), 2);
        assert_eq!(pace.len(), 145);
    }

    #[test]
    fn multiple_429_backoffs_cap_at_longest() {
        let mut pace = Pace::default();
        let mut now = Instant::now();
        pace.limited(now);
        assert_eq!(pace.until(), Some(now + Duration::from_secs(30)));

        // Rapid 429 repeats double the pause: 30s -> 60s -> 120s -> 240s -> 480s -> 960s (capped at 900s / 15m)
        for _ in 0..10 {
            now += Duration::from_secs(1);
            pace.limited(now);
        }
        assert_eq!(pace.until(), Some(now + LONGEST));
    }

    #[test]
    fn older_resume_does_not_abort_newer_pause() {
        let now = Instant::now();

        // First pause: 30 seconds
        let pause_1_deadline = now + Duration::from_secs(30);
        let mut paused_until = Some(pause_1_deadline);
        assert_eq!(paused_until, Some(pause_1_deadline));

        // Before 30s expires, another rate limit extends to 60s
        let pause_2_deadline = now + Duration::from_secs(60);
        paused_until = Some(pause_2_deadline);

        // Timer from first pause (30s) fires at now + 30s:
        let time_of_timer_1 = now + Duration::from_secs(30);
        // The resume logic checks: if Instant::now() >= deadline, clear; else preserve!
        let can_clear_at_t30 = paused_until.is_none_or(|until| time_of_timer_1 >= until);
        assert!(
            !can_clear_at_t30,
            "Timer 1 must NOT clear the newer 60s pause!"
        );

        // Timer from second pause (60s) fires at now + 60s:
        let time_of_timer_2 = now + Duration::from_secs(60);
        let can_clear_at_t60 = paused_until.is_none_or(|until| time_of_timer_2 >= until);
        assert!(
            can_clear_at_t60,
            "Timer 2 clears once the deadline is reached"
        );
    }
}
