//! Persistent RFCOMM connection manager — per-MAC connection map.
//!
//! Keeps one RFCOMM socket per MAC address open across PAPPL device
//! open/close cycles.  This avoids the Jieli chipset's slow RFCOMM
//! teardown which causes connect failures when PAPPL rapidly opens
//! and closes the device (startup ID checks, status polls, then
//! print).
//!
//! Each connection is closed automatically after an idle timeout (no
//! acquire and release activity for `IDLE_TIMEOUT`).
//!
//! Unlike the previous single-connection design, this allows multiple
//! printers to be used simultaneously — each MAC gets its own
//! independent mutex so I/O to one device never blocks another.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::rfcomm::{self, RfcommConnection};

/// Idle timeout — close the socket if no `acquire` within this
/// duration after the last `release`.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Per-connection state, wrapped in its own mutex.
struct Entry {
    conn: Arc<RfcommConnection>,
    channel: u8,
    in_use: bool,
    idle_since: Option<Instant>,
}

/// Per-MAC slot state.
struct SlotState {
    entry: Mutex<Option<Entry>>,
    cv: Condvar,
}

impl SlotState {
    const fn new() -> Self {
        Self {
            entry: Mutex::new(None),
            cv: Condvar::new(),
        }
    }
}

/// Per-MAC slot with independent synchronization.
type Slot = Arc<SlotState>;

/// The global per-MAC connection map.
///
/// Each MAC key maps to a `Slot`:
/// - `Some(entry)`: a live (or recently-idle) connection exists.
/// - `None`: the slot exists but the connection was dropped.
///
/// The outer `Mutex<HashMap>` is held only briefly to look up or
/// insert an `Arc`; per-MAC synchronization happens inside `SlotState`.
static POOL: Mutex<Option<HashMap<[u8; 6], Slot>>> = Mutex::new(None);

/// Lazily initialise the pool.
fn pool() -> &'static Mutex<Option<HashMap<[u8; 6], Slot>>> {
    // Ensure the inner HashMap exists.
    let mut p = POOL.lock().unwrap();
    if p.is_none() {
        *p = Some(HashMap::new());
    }
    drop(p);
    &POOL
}

/// A per-MAC lease to a live connection.
///
/// At most one `ConnGuard` can exist per MAC at a time. Dropping the
/// guard marks the connection idle and wakes a waiter.
pub struct ConnGuard {
    slot: Slot,
    conn: Arc<RfcommConnection>,
}

impl ConnGuard {
    /// Borrow the underlying RFCOMM connection.
    pub fn conn(&self) -> &RfcommConnection {
        self.conn.as_ref()
    }
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Ok(mut entry_guard) = self.slot.entry.lock() {
            if let Some(entry) = entry_guard.as_mut() {
                entry.in_use = false;
                entry.idle_since = Some(Instant::now());
            }
            self.slot.cv.notify_one();
        }
    }
}

/// Check if a connection is stale and should be reconnected.
fn should_reconnect(entry: &Entry) -> bool {
    entry.idle_since.map_or_else(
        || !is_alive(entry.conn.as_ref()),
        |idle_since| idle_since.elapsed() >= IDLE_TIMEOUT || !is_alive(entry.conn.as_ref()),
    )
}

fn parse_channel_list(raw: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for part in raw.split(',') {
        let trimmed = part.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(channel) = trimmed.parse::<u8>() {
            if (1..=30).contains(&channel) && !out.contains(&channel) {
                out.push(channel);
            }
        }
    }
    out
}

fn configured_channels() -> Vec<u8> {
    let from_env = std::env::var("PHOMEMO_BT_CHANNELS").unwrap_or_default();
    let parsed = parse_channel_list(&from_env);
    if parsed.is_empty() { vec![1] } else { parsed }
}

fn candidate_channels(channel_hint: Option<u8>, cached_channel: Option<u8>) -> Vec<u8> {
    let mut out = Vec::new();

    for channel in [channel_hint, cached_channel].into_iter().flatten() {
        if (1..=30).contains(&channel) && !out.contains(&channel) {
            out.push(channel);
        }
    }

    for channel in configured_channels() {
        if !out.contains(&channel) {
            out.push(channel);
        }
    }

    out
}

fn connect_with_candidates(
    mac: [u8; 6],
    timeout: Duration,
    channel_hint: Option<u8>,
    cached_channel: Option<u8>,
) -> Result<(Arc<RfcommConnection>, u8), std::io::Error> {
    let mut last_error: Option<std::io::Error> = None;

    for channel in candidate_channels(channel_hint, cached_channel) {
        match rfcomm::connect(&mac, channel, timeout) {
            Ok(conn) => return Ok((Arc::new(conn), channel)),
            Err(err) => {
                last_error = Some(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "no RFCOMM channel candidates configured",
        )
    }))
}

/// Acquire a connection to the given MAC address on RFCOMM channel 1.
///
/// If a persistent connection to this MAC exists and is alive, it is
/// reused. Otherwise a new connection is established.
///
/// The returned `ConnGuard` provides exclusive per-MAC access until dropped.
pub fn acquire(
    mac: [u8; 6],
    channel_hint: Option<u8>,
    timeout: Duration,
) -> Result<ConnGuard, std::io::Error> {
    // Step 1: get (or create) the per-MAC slot.
    let slot: Slot = {
        let mut pool_guard = pool().lock().unwrap();
        let map = pool_guard.as_mut().unwrap();
        let slot = Arc::clone(map.entry(mac).or_insert_with(|| Arc::new(SlotState::new())));
        drop(pool_guard);
        slot
    };

    // Step 2: lock and wait for exclusive per-MAC access.
    let mut guard = slot.entry.lock().unwrap();
    while guard.as_ref().is_some_and(|entry| entry.in_use) {
        guard = slot.cv.wait(guard).unwrap();
    }

    // Step 3: reconnect if needed.
    let need_reconnect = guard.as_ref().is_none_or(should_reconnect);
    let cached_channel = guard.as_ref().map(|entry| entry.channel);

    if need_reconnect {
        *guard = None;
        let (conn, channel) = connect_with_candidates(mac, timeout, channel_hint, cached_channel)?;
        *guard = Some(Entry {
            conn,
            channel,
            in_use: true,
            idle_since: None,
        });
    } else {
        let entry = guard.as_mut().expect("entry must exist when reusing slot");
        entry.in_use = true;
        entry.idle_since = None;
    }

    let conn = Arc::clone(&guard.as_ref().expect("entry must exist after acquire").conn);
    drop(guard);

    Ok(ConnGuard { slot, conn })
}

/// Check if a connection is still alive by attempting a non-blocking
/// peek.
fn is_alive(conn: &RfcommConnection) -> bool {
    let mut buf = [0u8; 1];
    let rc = unsafe {
        libc::recv(
            conn.fd(),
            buf.as_mut_ptr().cast::<libc::c_void>(),
            1,
            libc::MSG_PEEK | libc::MSG_DONTWAIT,
        )
    };
    if rc > 0 {
        return true; // data pending — alive
    }
    if rc == 0 {
        return false; // peer closed
    }
    let err = std::io::Error::last_os_error();
    matches!(
        err.raw_os_error(),
        Some(code) if code == libc::EAGAIN || code == libc::EWOULDBLOCK
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conn_guard_drop_marks_entry_idle() {
        let [fd_left, fd_right] = {
            let mut fds = [0; 2];
            let rc =
                unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
            assert_eq!(rc, 0, "socketpair must succeed");
            fds
        };

        let slot = Arc::new(SlotState::new());
        {
            let mut guard = slot.entry.lock().expect("slot lock must succeed");
            *guard = Some(Entry {
                conn: Arc::new(RfcommConnection::from_raw_fd_for_test(fd_left)),
                channel: 1,
                in_use: true,
                idle_since: None,
            });
        }

        let conn = {
            let guard = slot.entry.lock().expect("slot lock must succeed");
            Arc::clone(&guard.as_ref().expect("entry must exist").conn)
        };

        let lease = ConnGuard {
            slot: Arc::clone(&slot),
            conn,
        };
        drop(lease);

        let in_use = slot
            .entry
            .lock()
            .expect("slot lock must succeed")
            .as_ref()
            .is_some_and(|entry| entry.in_use);
        let idle_marked = slot
            .entry
            .lock()
            .expect("slot lock must succeed")
            .as_ref()
            .is_some_and(|entry| entry.idle_since.is_some());
        assert!(!in_use);
        assert!(idle_marked);

        unsafe {
            libc::close(fd_right);
        }
    }

    #[test]
    fn parse_channel_list_filters_invalid_values() {
        assert_eq!(parse_channel_list("1, 2,2, 0, 31,foo,7"), vec![1, 2, 7]);
    }

    #[test]
    fn candidate_channels_prioritize_hint_and_cache() {
        let original = std::env::var("PHOMEMO_BT_CHANNELS").ok();
        unsafe {
            std::env::set_var("PHOMEMO_BT_CHANNELS", "5,6");
        }

        let channels = candidate_channels(Some(3), Some(5));
        assert_eq!(channels, vec![3, 5, 6]);

        match original {
            Some(value) => unsafe {
                std::env::set_var("PHOMEMO_BT_CHANNELS", value);
            },
            None => unsafe {
                std::env::remove_var("PHOMEMO_BT_CHANNELS");
            },
        }
    }
}
