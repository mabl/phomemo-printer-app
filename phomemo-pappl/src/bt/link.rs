//! A printer's Bluetooth link: the RFCOMM stream and what the printer has
//! told us over it.
//!
//! [`Link`] is what the connection pool keeps per printer, so it outlives
//! PAPPL's device sessions: the battery level a printer pushed while one
//! session had the device is still known in the next. A link that failed
//! is not reused, so a job cut off half-way is never continued by the next
//! one's bytes.

use std::cell::Cell;
use std::io::{self, Read, Write};
use std::sync::{LazyLock, Once};
use std::thread;
use std::time::{Duration, Instant};

use phomemo_protocol::responses::{BatteryStatus, Decoder, Response};

use super::address::{BdAddr, Channel};
use super::connmgr::{Config, Connection, Connector, Pool, parse_channels};
use super::rfcomm::RfcommStream;

/// How long a plain read waits for data, as PAPPL's own socket backend does.
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a write may wait for the printer to take data.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a link may go unused before it is closed.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// How often idle links are checked; one is closed at most this much
/// after its idle timeout.
const REAP_INTERVAL: Duration = Duration::from_secs(5);

/// The most stale input taken off a link before it is reused. A printer
/// that keeps sending more is not waited for.
const STALE_INPUT_LIMIT: usize = 4096;

/// The environment variable listing the RFCOMM channels to try,
/// comma-separated; [`Channel::DEFAULT`] if unset or empty.
const CHANNELS_VARIABLE: &str = "PHOMEMO_BT_CHANNELS";

/// An open link to a printer.
#[derive(Debug)]
pub struct Link {
    stream: RfcommStream,
    /// The last battery level the printer reported, in percent.
    battery: Cell<Option<u8>>,
    /// The first failure seen: an I/O error, or the end of the stream as
    /// [`io::ErrorKind::UnexpectedEof`].
    failure: Cell<Option<io::ErrorKind>>,
}

impl Link {
    /// A link over `stream`.
    #[must_use]
    pub const fn new(stream: RfcommStream) -> Self {
        Self {
            stream,
            battery: Cell::new(None),
            failure: Cell::new(None),
        }
    }

    /// The battery level the printer last reported, in percent.
    #[must_use]
    pub fn battery(&self) -> Option<u8> {
        self.battery.get()
    }

    /// Why the link failed, if it has: it is not reused then, and nothing
    /// more is to be expected over it.
    #[must_use]
    pub fn failure(&self) -> Option<io::Error> {
        self.failure.get().map(io::Error::from)
    }

    /// Read what arrives, waiting at most the read timeout; 0 once the
    /// printer has closed the link (or if `buf` is empty). As
    /// [`io::Read::read`], but retrying when a signal interrupts it.
    ///
    /// # Errors
    ///
    /// Fails with [`io::ErrorKind::WouldBlock`] if nothing arrives in time,
    /// or with the socket's error.
    pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match (&self.stream).read(buf) {
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                result => return self.watch(result),
            }
        }
    }

    /// Send all of `data`.
    ///
    /// # Errors
    ///
    /// Fails if the printer does not take it within the write timeout, or
    /// with the socket's error. The link is not reused afterwards.
    pub fn write_all(&self, data: &[u8]) -> io::Result<()> {
        let result = (&self.stream).write_all(data);
        if let Err(err) = &result {
            self.fail(err.kind());
        }
        result
    }

    /// Take off the input that has arrived unasked - a late answer, a
    /// report from an earlier job - without waiting for more, so that
    /// whoever asks next does not mistake it for their answer. Battery
    /// reports among it are noted.
    ///
    /// # Errors
    ///
    /// Fails if the link has failed, now or before.
    pub fn discard_input(&self) -> io::Result<()> {
        if let Some(err) = self.failure() {
            return Err(err);
        }
        let stale = self
            .stream
            .read_pending(STALE_INPUT_LIMIT)
            .inspect_err(|err| self.fail(err.kind()))?;
        let mut decoder = Decoder::new();
        decoder.extend_from_slice(&stale);
        for response in decoder.responses() {
            self.note(&response);
        }
        Ok(())
    }

    /// The responses completed by what arrives before `deadline`, noting
    /// battery reports on the way.
    ///
    /// May return no responses, when what arrived did not complete one.
    ///
    /// # Errors
    ///
    /// Fails with [`io::ErrorKind::TimedOut`] if nothing arrives by
    /// `deadline`, [`io::ErrorKind::UnexpectedEof`] if the printer closed
    /// the link, or with the socket's error.
    pub fn receive(&self, decoder: &mut Decoder, deadline: Instant) -> io::Result<Vec<Response>> {
        let mut buf = [0; 256];
        let timeout = deadline.saturating_duration_since(Instant::now());
        let n = self.watch(self.stream.read_within(&mut buf, timeout))?;
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the printer closed the connection",
            ));
        }
        decoder.extend_from_slice(&buf[..n]);
        let responses: Vec<_> = decoder.responses().collect();
        for response in &responses {
            self.note(response);
        }
        Ok(responses)
    }

    /// Record a failure if `result`, of a read into a buffer that was not
    /// empty, is the end of the stream or an error other than a timeout.
    fn watch(&self, result: io::Result<usize>) -> io::Result<usize> {
        match &result {
            Ok(0) => self.fail(io::ErrorKind::UnexpectedEof),
            Err(err) if !is_timeout(err) => self.fail(err.kind()),
            _ => {}
        }
        result
    }

    /// Record `kind` as the link's failure, unless one was recorded before.
    fn fail(&self, kind: io::ErrorKind) {
        if self.failure.get().is_none() {
            self.failure.set(Some(kind));
        }
    }

    /// Remember what `response` says about the printer itself.
    fn note(&self, response: &Response) {
        if let Response::Battery(status) = response {
            self.battery.set(battery_percent(*status));
        }
    }
}

impl Connection for Link {
    /// Usable unless it failed or was closed; input that arrived since the
    /// last use is taken off first ([`discard_input`](Link::discard_input)).
    fn is_usable(&self) -> bool {
        self.discard_input().is_ok()
    }
}

/// Whether `err` is a read or write that ran out of time.
#[must_use]
pub fn is_timeout(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
    )
}

/// The charge a battery report stands for, in percent. The low-battery
/// alarms stand for 10, 5 and 3 % (`Response::Battery`).
const fn battery_percent(status: BatteryStatus) -> Option<u8> {
    match status {
        BatteryStatus::Level(percent) if percent <= 100 => Some(percent),
        BatteryStatus::LowAlarm(0xa1) => Some(10),
        BatteryStatus::LowAlarm(0xa2) => Some(5),
        BatteryStatus::LowAlarm(_) => Some(3),
        BatteryStatus::Level(_) | BatteryStatus::DryCell => None,
    }
}

/// Connects [`Link`]s over RFCOMM.
#[derive(Debug, Clone, Copy)]
pub struct RfcommConnector;

impl Connector for RfcommConnector {
    type Connection = Link;

    fn connect(&self, address: BdAddr, channel: Channel, timeout: Duration) -> io::Result<Link> {
        let stream = RfcommStream::connect(address, channel, timeout)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Link::new(stream))
    }
}

/// The process's links, with a thread closing idle ones.
///
/// The channels come from `PHOMEMO_BT_CHANNELS`, read once.
pub fn pool() -> &'static Pool<RfcommConnector> {
    static POOL: LazyLock<Pool<RfcommConnector>> = LazyLock::new(|| {
        let mut channels = std::env::var(CHANNELS_VARIABLE)
            .map(|list| parse_channels(&list))
            .unwrap_or_default();
        if channels.is_empty() {
            channels.push(Channel::DEFAULT);
        }
        Pool::new(
            RfcommConnector,
            Config {
                channels,
                idle_timeout: IDLE_TIMEOUT,
            },
        )
    });
    static REAPER: Once = Once::new();

    REAPER.call_once(|| {
        // The thread only sleeps and briefly locks the pool, so it never
        // holds up the process's exit. Without it, an idle link is still
        // replaced on its next use, just not closed before.
        if let Err(err) = thread::Builder::new()
            .name("bt-reaper".to_owned())
            .spawn(|| POOL.reap_forever(REAP_INTERVAL))
        {
            eprintln!("Unable to start closing idle Bluetooth links: {err}");
        }
    });
    &POOL
}

/// A link over a socket pair, and the printer's end of it.
#[cfg(test)]
pub(super) fn pair() -> (Link, std::os::unix::net::UnixStream) {
    let (ours, printer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    (
        Link::new(RfcommStream::from(std::os::fd::OwnedFd::from(ours))),
        printer,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn battery_reports_are_noted_as_they_arrive() {
        let (link, mut printer) = pair();
        printer.write_all(&[0x1a, 0x04, 0x55, 0x1a]).expect("send");
        let mut decoder = Decoder::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            link.receive(&mut decoder, deadline).expect("receive"),
            [Response::Battery(BatteryStatus::Level(0x55))]
        );
        assert_eq!(link.battery(), Some(0x55));
    }

    #[test]
    fn stale_input_is_taken_off_before_reuse() {
        let (link, mut printer) = pair();
        printer
            .write_all(&[0x1a, 0x0f, 0x0c, 0x1a, 0x04, 0xa2])
            .expect("send");
        assert!(link.is_usable());
        assert_eq!(link.battery(), Some(5));
        let err = link
            .receive(&mut Decoder::new(), Instant::now())
            .expect_err("nothing left");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(link.is_usable(), "a timeout does not break the link");
    }

    #[test]
    fn a_closed_link_is_not_reused() {
        let (link, printer) = pair();
        drop(printer);
        let err = link
            .receive(&mut Decoder::new(), Instant::now() + Duration::from_secs(5))
            .expect_err("closed");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(!link.is_usable());
    }

    #[test]
    fn a_failed_write_breaks_the_link() {
        let (link, printer) = pair();
        drop(printer);
        assert!(link.failure().is_none());
        let err = link.write_all(&[0; 8]).expect_err("closed");
        assert_eq!(
            link.failure().map(|failure| failure.kind()),
            Some(err.kind())
        );
        assert!(!link.is_usable());
        assert!(link.discard_input().is_err());
    }

    #[test]
    fn reading_into_nothing_is_not_the_end() {
        let (link, _printer) = pair();
        assert_eq!(link.read(&mut []).expect("empty read"), 0);
        assert!(link.failure().is_none());
    }

    #[test]
    fn the_end_of_the_stream_is_a_failure() {
        let (link, printer) = pair();
        drop(printer);
        assert_eq!(link.read(&mut [0; 8]).expect("end of file"), 0);
        assert_eq!(
            link.failure().map(|failure| failure.kind()),
            Some(io::ErrorKind::UnexpectedEof)
        );
    }

    #[test]
    fn battery_percentages() {
        assert_eq!(battery_percent(BatteryStatus::Level(42)), Some(42));
        assert_eq!(battery_percent(BatteryStatus::Level(200)), None);
        assert_eq!(battery_percent(BatteryStatus::LowAlarm(0xa1)), Some(10));
        assert_eq!(battery_percent(BatteryStatus::LowAlarm(0xa3)), Some(3));
        assert_eq!(battery_percent(BatteryStatus::DryCell), None);
    }
}
