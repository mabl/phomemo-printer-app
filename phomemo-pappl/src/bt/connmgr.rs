//! Persistent connections, one per printer, leased to one user at a time.
//!
//! PAPPL opens and closes a device for every job and every status poll.
//! Connecting over RFCOMM takes a second or two, and the printers' Jieli
//! chipsets tear a link down slowly enough that connecting again right
//! after closing fails. So a [`Pool`] keeps each printer's connection open
//! between uses and hands it out as a [`Lease`]; a printer's lease is
//! exclusive, while different printers are independent.
//!
//! A connection nobody has used for [`Config::idle_timeout`] is closed by
//! [`Pool::reap`], so a printer that is no longer used gets its link - and
//! its battery - back. [`Pool::reap_forever`] runs it periodically on a
//! thread of its own; the connection is closed on the next lease
//! otherwise.

use std::collections::HashMap;
use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::mem::{self, ManuallyDrop};
use std::ops::Deref;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use super::address::{BdAddr, Channel};
use super::lock;

/// A connection the pool can keep.
pub trait Connection: Send {
    /// Whether the connection can be leased (again).
    ///
    /// Called whenever a lease ends and before an idle connection is
    /// leased, so it may also discard input that nobody is waiting for.
    fn is_usable(&self) -> bool;
}

/// How the pool connects to a printer.
pub trait Connector {
    /// The connections it makes.
    type Connection: Connection;

    /// Connect to `channel` of `address`, giving up after `timeout`.
    ///
    /// # Errors
    ///
    /// Fails if the printer cannot be reached on that channel.
    fn connect(
        &self,
        address: BdAddr,
        channel: Channel,
        timeout: Duration,
    ) -> io::Result<Self::Connection>;
}

/// How a pool connects and how long it keeps unused connections.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// The channels to try, in order, after the one a printer's URI names
    /// and the one that worked last.
    pub channels: Vec<Channel>,
    /// How long a connection may go unused before it is closed.
    pub idle_timeout: Duration,
}

/// The channels in a comma-separated list such as `1,3`, in order and
/// without repeats, skipping anything that is not a channel.
#[must_use]
pub fn parse_channels(list: &str) -> Vec<Channel> {
    let mut channels = Vec::new();
    for channel in list.split(',').filter_map(|item| item.trim().parse().ok()) {
        if !channels.contains(&channel) {
            channels.push(channel);
        }
    }
    channels
}

/// Why a printer could not be leased.
#[derive(Debug)]
pub enum AcquireError {
    /// Someone else held the printer's connection for the whole timeout.
    Busy(Duration),
    /// No channel to try.
    NoChannels,
    /// Every channel failed; the error is the last one's.
    Connect {
        /// The channels tried, in order.
        channels: Vec<Channel>,
        /// Why the last one failed.
        source: io::Error,
    },
}

impl fmt::Display for AcquireError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(timeout) => write!(
                f,
                "the printer's connection stayed in use for {} s - is another queue \
                 using the same printer? Use one queue per printer",
                timeout.as_secs_f32()
            ),
            Self::NoChannels => f.write_str("no RFCOMM channel to try"),
            Self::Connect { channels, source } => {
                f.write_str("unable to connect on RFCOMM channel")?;
                for (i, channel) in channels.iter().enumerate() {
                    write!(f, "{}{channel}", if i == 0 { " " } else { ", " })?;
                }
                write!(f, ": {source}")
            }
        }
    }
}

impl StdError for AcquireError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Connect { source, .. } => Some(source),
            Self::Busy(_) | Self::NoChannels => None,
        }
    }
}

/// The connections of every printer the pool has seen.
#[derive(Debug)]
pub struct Pool<C: Connector> {
    connector: C,
    config: Config,
    slots: Mutex<HashMap<BdAddr, Arc<Slot<C::Connection>>>>,
}

impl<C: Connector> Pool<C> {
    /// An empty pool.
    pub fn new(connector: C, config: Config) -> Self {
        Self {
            connector,
            config,
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// Lease the connection to `address`, connecting if there is none or
    /// it is no longer usable.
    ///
    /// Waits up to `timeout` for a lease someone else holds to end, and
    /// gives each channel tried `timeout` to connect. The channel
    /// `preferred` is tried first, then the one that worked last, then the
    /// configured ones.
    ///
    /// # Errors
    ///
    /// Fails if the connection stays leased for `timeout`, or no channel
    /// connects.
    pub fn acquire(
        &self,
        address: BdAddr,
        preferred: Option<Channel>,
        timeout: Duration,
    ) -> Result<Lease<C::Connection>, AcquireError> {
        let slot = Arc::clone(lock(&self.slots).entry(address).or_default());

        let (previous, last_channel) = {
            let (mut inner, _) = slot
                .released
                .wait_timeout_while(lock(&slot.inner), timeout, |inner| {
                    matches!(inner.state, State::Leased)
                })
                .unwrap_or_else(PoisonError::into_inner);
            if matches!(inner.state, State::Leased) {
                return Err(AcquireError::Busy(timeout));
            }
            (mem::replace(&mut inner.state, State::Leased), inner.channel)
        };
        // Until the lease exists, an early return must give the slot back.
        let reservation = Reservation(&slot);

        let reusable = match previous {
            State::Idle { connection, since }
                if since.elapsed() < self.config.idle_timeout && connection.is_usable() =>
            {
                Some(connection)
            }
            stale => {
                // Closed before connecting again.
                drop(stale);
                None
            }
        };
        let connection = if let Some(connection) = reusable {
            connection
        } else {
            let candidates = self.candidates(preferred, last_channel);
            let (connection, channel) = self.connect(address, &candidates, timeout)?;
            lock(&slot.inner).channel = Some(channel);
            connection
        };

        reservation.keep();
        Ok(Lease {
            slot,
            connection: ManuallyDrop::new(connection),
        })
    }

    /// Close every connection that has been idle for the idle timeout at
    /// `now`; how many were closed.
    pub fn reap(&self, now: Instant) -> usize {
        let slots: Vec<_> = lock(&self.slots).values().cloned().collect();
        slots
            .iter()
            .filter(|slot| slot.close_if_idle(now, self.config.idle_timeout))
            .count()
    }

    /// [`reap`](Self::reap) every `interval`, forever.
    pub fn reap_forever(&self, interval: Duration) -> ! {
        loop {
            thread::sleep(interval);
            self.reap(Instant::now());
        }
    }

    /// The channels to try, in order, without repeats.
    fn candidates(&self, preferred: Option<Channel>, last: Option<Channel>) -> Vec<Channel> {
        let mut candidates = Vec::new();
        for channel in preferred
            .into_iter()
            .chain(last)
            .chain(self.config.channels.iter().copied())
        {
            if !candidates.contains(&channel) {
                candidates.push(channel);
            }
        }
        candidates
    }

    /// Connect on the first of `channels` that works.
    fn connect(
        &self,
        address: BdAddr,
        channels: &[Channel],
        timeout: Duration,
    ) -> Result<(C::Connection, Channel), AcquireError> {
        let mut last_error = None;
        for &channel in channels {
            match self.connector.connect(address, channel, timeout) {
                Ok(connection) => return Ok((connection, channel)),
                Err(err) => last_error = Some(err),
            }
        }
        Err(
            last_error.map_or(AcquireError::NoChannels, |source| AcquireError::Connect {
                channels: channels.to_vec(),
                source,
            }),
        )
    }
}

/// One printer's connection and who has it.
#[derive(Debug)]
struct Slot<T> {
    inner: Mutex<SlotInner<T>>,
    /// Signalled when a lease ends.
    released: Condvar,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self {
            inner: Mutex::new(SlotInner {
                state: State::Closed,
                channel: None,
            }),
            released: Condvar::new(),
        }
    }
}

impl<T> Slot<T> {
    /// Close the connection if it has been idle for `idle_timeout` at
    /// `now`; whether it was. It is closed under the lock, so that nobody
    /// connects again while it is still open.
    fn close_if_idle(&self, now: Instant, idle_timeout: Duration) -> bool {
        let mut inner = lock(&self.inner);
        let expired = matches!(
            inner.state,
            State::Idle { since, .. } if now.saturating_duration_since(since) >= idle_timeout
        );
        if expired {
            inner.state = State::Closed;
        }
        expired
    }
}

#[derive(Debug)]
struct SlotInner<T> {
    state: State<T>,
    /// The channel the last connection was made on.
    channel: Option<Channel>,
}

#[derive(Debug)]
enum State<T> {
    /// No connection.
    Closed,
    /// A connection nobody holds, unused since `since`.
    Idle { connection: T, since: Instant },
    /// Someone holds the slot: a [`Lease`], or an `acquire` connecting.
    Leased,
}

/// Exclusive use of a printer's connection; the connection goes back to
/// the pool when the lease is dropped.
#[derive(Debug)]
pub struct Lease<T: Connection> {
    slot: Arc<Slot<T>>,
    connection: ManuallyDrop<T>,
}

impl<T: Connection> Deref for Lease<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.connection
    }
}

impl<T: Connection> Drop for Lease<T> {
    fn drop(&mut self) {
        // SAFETY: the connection is taken exactly once, here, and the lease
        // is gone afterwards.
        let connection = unsafe { ManuallyDrop::take(&mut self.connection) };
        let state = if connection.is_usable() {
            State::Idle {
                connection,
                since: Instant::now(),
            }
        } else {
            // Closed before anyone can connect again.
            drop(connection);
            State::Closed
        };
        lock(&self.slot.inner).state = state;
        self.slot.released.notify_one();
    }
}

/// A slot `acquire` holds before it has a connection to lease: dropping
/// it marks the slot closed and wakes a waiter.
struct Reservation<'a, T>(&'a Slot<T>);

impl<T> Reservation<'_, T> {
    /// Hand the slot over to a [`Lease`].
    const fn keep(self) {
        mem::forget(self);
    }
}

impl<T> Drop for Reservation<'_, T> {
    fn drop(&mut self) {
        lock(&self.0.inner).state = State::Closed;
        self.0.released.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    const PRINTER: BdAddr = BdAddr::new([0x27, 0xa6, 0x4f, 0x5d, 0x03, 0x99]);
    const OTHER: BdAddr = BdAddr::new([1, 2, 3, 4, 5, 6]);
    const IDLE: Duration = Duration::from_secs(30);

    fn channel(number: u8) -> Channel {
        Channel::new(number).expect("a valid channel")
    }

    /// A numbered connection, usable while its connector says so.
    #[derive(Debug)]
    struct Fake {
        id: usize,
        usable: Arc<AtomicBool>,
        open: Arc<AtomicUsize>,
    }

    impl Connection for Fake {
        fn is_usable(&self) -> bool {
            self.usable.load(Ordering::SeqCst)
        }
    }

    impl Drop for Fake {
        fn drop(&mut self) {
            self.open.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Connects on the channels in `working`, numbering its connections.
    #[derive(Debug, Default)]
    struct FakeConnector {
        working: Vec<Channel>,
        attempts: Mutex<Vec<Channel>>,
        made: AtomicUsize,
        usable: Arc<AtomicBool>,
        /// Connections not yet dropped.
        open: Arc<AtomicUsize>,
        /// How many were open whenever a connection was attempted.
        open_at_attempts: Mutex<Vec<usize>>,
    }

    impl FakeConnector {
        fn on(channels: &[u8]) -> Self {
            Self {
                working: channels.iter().copied().map(channel).collect(),
                usable: Arc::new(AtomicBool::new(true)),
                ..Self::default()
            }
        }

        fn attempts(&self) -> Vec<u8> {
            lock(&self.attempts).iter().map(|c| c.get()).collect()
        }
    }

    impl Connector for FakeConnector {
        type Connection = Fake;

        fn connect(&self, _: BdAddr, channel: Channel, _: Duration) -> io::Result<Fake> {
            lock(&self.attempts).push(channel);
            lock(&self.open_at_attempts).push(self.open.load(Ordering::SeqCst));
            if !self.working.contains(&channel) {
                return Err(io::ErrorKind::ConnectionRefused.into());
            }
            self.open.fetch_add(1, Ordering::SeqCst);
            Ok(Fake {
                id: self.made.fetch_add(1, Ordering::SeqCst),
                usable: Arc::clone(&self.usable),
                open: Arc::clone(&self.open),
            })
        }
    }

    fn pool(connector: FakeConnector, channels: &[u8]) -> Pool<FakeConnector> {
        Pool::new(
            connector,
            Config {
                channels: channels.iter().copied().map(channel).collect(),
                idle_timeout: IDLE,
            },
        )
    }

    fn lease(pool: &Pool<FakeConnector>, address: BdAddr) -> Lease<Fake> {
        pool.acquire(address, None, Duration::from_millis(50))
            .expect("acquire")
    }

    #[test]
    fn a_released_connection_is_reused() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        let first = lease(&pool, PRINTER).id;
        assert_eq!(lease(&pool, PRINTER).id, first);
        assert_eq!(pool.connector.made.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn printers_have_their_own_connections() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        let printer = lease(&pool, PRINTER);
        let other = lease(&pool, OTHER);
        assert_ne!(printer.id, other.id);
    }

    #[test]
    fn a_connection_unusable_at_release_is_closed() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        let held = lease(&pool, PRINTER);
        pool.connector.usable.store(false, Ordering::SeqCst);
        drop(held);
        assert!(matches!(
            lock(&lock(&pool.slots)[&PRINTER].inner).state,
            State::Closed
        ));
        assert_eq!(lease(&pool, PRINTER).id, 1);
    }

    #[test]
    fn an_idle_connection_that_became_unusable_is_replaced() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        drop(lease(&pool, PRINTER));
        pool.connector.usable.store(false, Ordering::SeqCst);
        assert_eq!(lease(&pool, PRINTER).id, 1);
        // The old connection was closed before the new one was made.
        assert_eq!(*lock(&pool.connector.open_at_attempts), [0, 0]);
    }

    #[test]
    fn a_held_lease_makes_others_wait_then_fail() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        let held = lease(&pool, PRINTER);
        let started = Instant::now();
        let err = pool
            .acquire(PRINTER, None, Duration::from_millis(50))
            .expect_err("the connection is leased");
        assert!(matches!(err, AcquireError::Busy(_)));
        assert!(started.elapsed() >= Duration::from_millis(50));
        drop(held);
        lease(&pool, PRINTER);
    }

    #[test]
    fn a_waiter_gets_the_connection_when_the_lease_ends() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        let held = lease(&pool, PRINTER);
        let id = held.id;
        thread::scope(|scope| {
            let waiter = scope.spawn(|| {
                pool.acquire(PRINTER, None, Duration::from_secs(10))
                    .map(|l| l.id)
            });
            thread::sleep(Duration::from_millis(20));
            drop(held);
            assert_eq!(waiter.join().expect("waiter").expect("acquire"), id);
        });
    }

    #[test]
    fn the_reaper_closes_only_expired_idle_connections() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        drop(lease(&pool, PRINTER));
        let held = lease(&pool, OTHER);

        assert_eq!(pool.reap(Instant::now()), 0);
        assert_eq!(pool.reap(Instant::now() + IDLE), 1);
        // Closed by the time `reap` returns: only the leased one is open.
        assert_eq!(pool.connector.open.load(Ordering::SeqCst), 1);
        assert_eq!(pool.reap(Instant::now() + IDLE), 0);
        assert!(matches!(
            lock(&lock(&pool.slots)[&PRINTER].inner).state,
            State::Closed
        ));
        // The leased connection is untouched.
        assert!(matches!(
            lock(&lock(&pool.slots)[&OTHER].inner).state,
            State::Leased
        ));
        drop(held);
    }

    #[test]
    fn channels_are_tried_preferred_then_last_then_configured() {
        let pool = pool(FakeConnector::on(&[5]), &[1, 5, 6]);
        assert_eq!(
            pool.candidates(Some(channel(3)), Some(channel(5))),
            [3, 5, 1, 6].map(channel)
        );

        let connector = &pool.connector;
        drop(lease(&pool, PRINTER));
        assert_eq!(connector.attempts(), [1, 5]);
        // The working channel is remembered once the connection is closed.
        pool.reap(Instant::now() + IDLE);
        drop(lease(&pool, PRINTER));
        assert_eq!(connector.attempts(), [1, 5, 5]);
    }

    #[test]
    fn a_failed_connection_frees_the_slot() {
        let pool = pool(FakeConnector::on(&[]), &[1, 2]);
        let err = pool
            .acquire(PRINTER, None, Duration::from_millis(50))
            .expect_err("nothing connects");
        assert!(matches!(err, AcquireError::Connect { ref channels, .. } if channels.len() == 2));
        assert_eq!(
            err.to_string(),
            "unable to connect on RFCOMM channel 1, 2: connection refused"
        );
        assert!(matches!(
            lock(&lock(&pool.slots)[&PRINTER].inner).state,
            State::Closed
        ));
    }

    #[test]
    fn no_channels_is_an_error() {
        let pool = pool(FakeConnector::on(&[1]), &[]);
        let err = pool
            .acquire(PRINTER, None, Duration::from_millis(50))
            .expect_err("no channel");
        assert!(matches!(err, AcquireError::NoChannels));
    }

    #[test]
    fn a_poisoned_slot_still_works() {
        let pool = pool(FakeConnector::on(&[1]), &[1]);
        drop(lease(&pool, PRINTER));
        let slot = Arc::clone(&lock(&pool.slots)[&PRINTER]);
        let _ = thread::spawn(move || {
            let _guard = slot.inner.lock();
            panic!("poison the slot");
        })
        .join();
        assert_eq!(lease(&pool, PRINTER).id, 0);
    }

    #[test]
    fn channel_lists_parse_leniently() {
        assert_eq!(
            parse_channels("1, 2,2, 0, 31,foo,7,+3"),
            [1, 2, 7].map(channel)
        );
        assert_eq!(parse_channels(""), []);
    }
}
