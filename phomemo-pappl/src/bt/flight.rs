//! A query that runs on a helper thread, at most one at a time.
//!
//! Callers wait for the answer only as long as they choose to, while the
//! query runs to its end on its own thread. A caller arriving while a query
//! is in flight waits for that query's answer instead of starting another,
//! so a query that hangs costs one thread however many callers give up on
//! it.

use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::panic;
use std::sync::{Condvar, Mutex, PoisonError};
use std::thread;
use std::time::Duration;

use super::lock;

/// Why [`SingleFlight::ask`] has no answer.
#[derive(Debug)]
pub enum FlightError {
    /// The query thread could not be started.
    Spawn(io::Error),
    /// No answer within the caller's timeout.
    TimedOut(Duration),
    /// The query panicked.
    Panicked,
}

impl fmt::Display for FlightError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(err) => write!(f, "unable to start a thread: {err}"),
            Self::TimedOut(timeout) => {
                write!(f, "no answer within {} s", timeout.as_secs_f32())
            }
            Self::Panicked => f.write_str("the query failed unexpectedly"),
        }
    }
}

impl StdError for FlightError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Spawn(err) => Some(err),
            Self::TimedOut(_) | Self::Panicked => None,
        }
    }
}

/// A query run at most once at a time; see the module documentation.
#[derive(Debug)]
pub struct SingleFlight<T> {
    flight: Mutex<Flight<T>>,
    /// Signalled when a query has answered.
    answered: Condvar,
}

#[derive(Debug)]
struct Flight<T> {
    /// A query is running.
    running: bool,
    /// How many queries have answered.
    answers: u64,
    /// The latest answer; `None` if that query panicked.
    answer: Option<T>,
}

impl<T: Clone + Send + 'static> SingleFlight<T> {
    /// Nothing asked yet.
    pub const fn new() -> Self {
        Self {
            flight: Mutex::new(Flight {
                running: false,
                answers: 0,
                answer: None,
            }),
            answered: Condvar::new(),
        }
    }

    /// The answer of the query in flight, or else of a new run of `query`,
    /// waiting at most `timeout` for it.
    ///
    /// # Errors
    ///
    /// Fails if no answer comes in time, the query panics, or its thread
    /// cannot be started.
    pub fn ask(&'static self, query: fn() -> T, timeout: Duration) -> Result<T, FlightError> {
        let awaited = self.launch(query)?;
        let (flight, _) = self
            .answered
            .wait_timeout_while(lock(&self.flight), timeout, |flight| {
                flight.answers < awaited
            })
            .unwrap_or_else(PoisonError::into_inner);
        let answer = (flight.answers >= awaited).then(|| flight.answer.clone());
        drop(flight);
        match answer {
            Some(Some(answer)) => Ok(answer),
            Some(None) => Err(FlightError::Panicked),
            None => Err(FlightError::TimedOut(timeout)),
        }
    }

    /// Run `query` unless a query is in flight; how many queries will have
    /// answered once the one in flight has.
    fn launch(&'static self, query: fn() -> T) -> Result<u64, FlightError> {
        let mut flight = lock(&self.flight);
        if !flight.running {
            thread::Builder::new()
                .name("bt-query".to_owned())
                .spawn(move || self.land(panic::catch_unwind(query).ok()))
                .map_err(FlightError::Spawn)?;
            flight.running = true;
        }
        Ok(flight.answers + 1)
    }

    /// Publish a query's answer and end its flight.
    fn land(&self, answer: Option<T>) {
        let mut flight = lock(&self.flight);
        flight.answer = answer;
        flight.answers += 1;
        flight.running = false;
        drop(flight);
        self.answered.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier};

    use super::*;

    const LONG: Duration = Duration::from_secs(10);

    #[test]
    fn a_query_answers() {
        static FLIGHT: SingleFlight<u32> = SingleFlight::new();
        assert_eq!(FLIGHT.ask(|| 42, LONG).expect("answer"), 42);
    }

    #[test]
    fn callers_share_the_query_in_flight() {
        static FLIGHT: SingleFlight<usize> = SingleFlight::new();
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        static RELEASED: AtomicBool = AtomicBool::new(false);
        fn blocked() -> usize {
            while !RELEASED.load(Ordering::SeqCst) {
                thread::sleep(Duration::from_millis(1));
            }
            RUNS.fetch_add(1, Ordering::SeqCst) + 1
        }

        let started = Arc::new(Barrier::new(3));
        let callers: Vec<_> = (0..2)
            .map(|_| {
                let started = Arc::clone(&started);
                thread::spawn(move || {
                    started.wait();
                    FLIGHT.ask(blocked, LONG).expect("answer")
                })
            })
            .collect();
        started.wait();
        thread::sleep(Duration::from_millis(50));
        RELEASED.store(true, Ordering::SeqCst);

        for caller in callers {
            assert_eq!(caller.join().expect("caller"), 1);
        }
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_slow_query_times_out_and_is_joined_later() {
        static FLIGHT: SingleFlight<u32> = SingleFlight::new();
        static RUNS: AtomicUsize = AtomicUsize::new(0);
        fn slow() -> u32 {
            RUNS.fetch_add(1, Ordering::SeqCst);
            thread::sleep(Duration::from_millis(200));
            7
        }

        let err = FLIGHT
            .ask(slow, Duration::from_millis(10))
            .expect_err("too slow");
        assert!(matches!(err, FlightError::TimedOut(_)));
        // The second caller gets the first query's answer.
        assert_eq!(FLIGHT.ask(slow, LONG).expect("answer"), 7);
        assert_eq!(RUNS.load(Ordering::SeqCst), 1);
        // Once it has landed, the next caller starts a new query.
        assert_eq!(FLIGHT.ask(slow, LONG).expect("answer"), 7);
        assert_eq!(RUNS.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_panicking_query_does_not_block_the_next() {
        static FLIGHT: SingleFlight<u32> = SingleFlight::new();
        static CALLS: AtomicUsize = AtomicUsize::new(0);
        fn panics_once() -> u32 {
            assert!(CALLS.fetch_add(1, Ordering::SeqCst) > 0, "first call");
            3
        }

        let err = FLIGHT.ask(panics_once, LONG).expect_err("panicked");
        assert!(matches!(err, FlightError::Panicked));
        assert_eq!(FLIGHT.ask(panics_once, LONG).expect("answer"), 3);
    }
}
