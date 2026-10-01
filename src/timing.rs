//! Startup and operation marks.
//!
//! One info line each. Milestones fire once per process. Operations log
//! their own duration. Nothing here includes message text, phone numbers,
//! keys, or chat ids.

use std::sync::Mutex;
use std::time::Instant;

static ORIGIN: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
static MILESTONES: Mutex<Vec<&'static str>> = Mutex::new(Vec::new());
static OPERATIONS: Mutex<Vec<(&'static str, Instant)>> = Mutex::new(Vec::new());

fn origin() -> Instant {
    *ORIGIN.get_or_init(Instant::now)
}

fn lock<'a, T>(mutex: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Milliseconds since the process mark, or since the first caller.
pub fn elapsed_ms() -> u128 {
    origin().elapsed().as_millis()
}

/// First line: the process is up and logging.
pub fn process_started() {
    let _ = ORIGIN.set(Instant::now());
    log::info!("timing: process started 0ms");
}

/// A once-per-process point, with milliseconds since process start.
pub fn milestone(name: &'static str) {
    let mut seen = lock(&MILESTONES);
    if seen.contains(&name) {
        return;
    }
    seen.push(name);
    let ms = elapsed_ms();
    log::info!("timing: {name} {ms}ms");
}

/// Starts an operation. A newer start for the same name replaces the older one.
pub fn begin(name: &'static str) {
    let mut open = lock(&OPERATIONS);
    open.retain(|(existing, _)| *existing != name);
    open.push((name, Instant::now()));
}

/// Logs the duration of the matching [`begin`], if one is still open.
pub fn end(name: &'static str) {
    let started = {
        let mut open = lock(&OPERATIONS);
        let position = open.iter().position(|(existing, _)| *existing == name);
        position.map(|position| open.remove(position).1)
    };
    let Some(started) = started else {
        return;
    };
    let ms = started.elapsed().as_millis();
    log::info!("timing: {name} {ms}ms");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_does_not_go_backwards() {
        process_started();
        let first = elapsed_ms();
        let second = elapsed_ms();
        assert!(second >= first);
    }

    #[test]
    fn a_milestone_is_recorded_once() {
        milestone("unit milestone");
        let seen = lock(&MILESTONES);
        assert_eq!(
            seen.iter()
                .filter(|name| **name == "unit milestone")
                .count(),
            1
        );
        drop(seen);
        milestone("unit milestone");
        let seen = lock(&MILESTONES);
        assert_eq!(
            seen.iter()
                .filter(|name| **name == "unit milestone")
                .count(),
            1
        );
    }

    #[test]
    fn ending_without_a_start_is_quiet() {
        end("unit operation that was never started");
    }
}
