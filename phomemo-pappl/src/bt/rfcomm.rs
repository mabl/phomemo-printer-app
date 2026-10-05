//! RFCOMM sockets: the byte stream to a Bluetooth SPP printer.
//!
//! [`RfcommStream`] is to an RFCOMM socket what [`std::net::TcpStream`] is
//! to a TCP one: [`Read`] and [`Write`], with socket-level read and write
//! timeouts. On top of that it can wait for input with a deadline of its
//! own ([`read_within`](RfcommStream::read_within)), and take whatever
//! input is already queued without waiting
//! ([`read_pending`](RfcommStream::read_pending)), which also tells whether
//! the printer is still there. Neither touches the socket's own timeouts.
//!
//! The system calls go through `rustix`, so the only unsafe code is the
//! one place the kernel's `sockaddr_rc` is handed to it.

use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec, poll};
use rustix::io::{Errno, ioctl_fionbio};
use rustix::net::addr::{SocketAddrArg, SocketAddrLen, SocketAddrOpaque};
use rustix::net::sockopt::{self, Timeout};
use rustix::net::{
    AddressFamily, Protocol, RecvFlags, SendFlags, SocketFlags, SocketType, connect, recv, send,
    socket_with,
};

use super::address::{BdAddr, Channel};

/// `BTPROTO_RFCOMM` from `<bluetooth/bluetooth.h>`.
const BTPROTO_RFCOMM: Protocol = Protocol::from_raw(NonZeroU32::new(3).unwrap());

/// The most bytes one write sends. Writes are split at 1024 bytes, as the
/// Phomemo app's SPP writer does (`re/protocol/transport.md`, "Write
/// Chunking"); the boundary is the app's choice rather than the
/// protocol's, but it is the one the printers were validated with.
const SPP_CHUNK_SIZE: usize = 1024;

/// An open RFCOMM connection.
///
/// The descriptor is close-on-exec, so the link does not outlive this
/// process in a child such as `lpadmin`.
#[derive(Debug)]
pub struct RfcommStream {
    fd: OwnedFd,
}

impl RfcommStream {
    /// Connect to `channel` of `address`, giving up after `timeout`.
    ///
    /// # Errors
    ///
    /// Fails if the socket cannot be created, the connection is refused or
    /// fails, or it is not established within `timeout`
    /// ([`io::ErrorKind::TimedOut`]).
    pub fn connect(address: BdAddr, channel: Channel, timeout: Duration) -> io::Result<Self> {
        let fd = socket_with(
            AddressFamily::BLUETOOTH,
            SocketType::STREAM,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            Some(BTPROTO_RFCOMM),
        )?;
        match connect(&fd, &RfcommAddr { address, channel }) {
            Ok(()) => {}
            Err(Errno::INPROGRESS) => {
                if !wait_for(fd.as_fd(), PollFlags::OUT, deadline_after(timeout))? {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "the RFCOMM connection was not established in time",
                    ));
                }
                sockopt::socket_error(&fd)??;
            }
            Err(err) => return Err(err.into()),
        }
        ioctl_fionbio(&fd, false)?;
        Ok(Self { fd })
    }

    /// Bound how long a [`Read`] waits for data: a read that times out
    /// fails with [`io::ErrorKind::WouldBlock`], as
    /// [`TcpStream`](std::net::TcpStream)'s does. `None` waits forever.
    ///
    /// # Errors
    ///
    /// Fails if the socket rejects the timeout.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        Ok(sockopt::set_socket_timeout(
            &self.fd,
            Timeout::Recv,
            timeout,
        )?)
    }

    /// Bound how long a [`Write`] waits for the printer to take data; as
    /// [`set_read_timeout`](Self::set_read_timeout).
    ///
    /// # Errors
    ///
    /// Fails if the socket rejects the timeout.
    pub fn set_write_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        Ok(sockopt::set_socket_timeout(
            &self.fd,
            Timeout::Send,
            timeout,
        )?)
    }

    /// Read what arrives within `timeout` into `buf`.
    ///
    /// Returns the number of bytes read, which is 0 once the printer has
    /// closed the connection (or if `buf` is empty).
    ///
    /// # Errors
    ///
    /// Fails with [`io::ErrorKind::TimedOut`] if nothing arrives in time,
    /// or with the socket's error.
    pub fn read_within(&self, buf: &mut [u8], timeout: Duration) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let deadline = deadline_after(timeout);
        loop {
            if !wait_for(self.fd.as_fd(), PollFlags::IN, deadline)? {
                return Err(io::ErrorKind::TimedOut.into());
            }
            match self.recv(buf, RecvFlags::DONTWAIT) {
                // Readable, then nothing to read after all: wait on.
                Err(err) if is_retryable(&err) => {}
                result => return result,
            }
        }
    }

    /// The input that has already arrived, up to `limit` bytes, without
    /// waiting for more.
    ///
    /// # Errors
    ///
    /// Fails with [`io::ErrorKind::UnexpectedEof`] if the printer has
    /// closed the connection, or with the socket's error.
    pub fn read_pending(&self, limit: usize) -> io::Result<Vec<u8>> {
        let mut pending = Vec::new();
        let mut buf = [0; 256];
        while pending.len() < limit {
            let wanted = buf.len().min(limit - pending.len());
            match self.recv(&mut buf[..wanted], RecvFlags::DONTWAIT) {
                Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
                Ok(n) => pending.extend_from_slice(&buf[..n]),
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => break,
                Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
                Err(err) => return Err(err),
            }
        }
        Ok(pending)
    }

    fn recv(&self, buf: &mut [u8], flags: RecvFlags) -> io::Result<usize> {
        let (read, _) = recv(&self.fd, buf, flags)?;
        Ok(read)
    }
}

/// The instant `timeout` from now, or `None` - never - if that is too
/// far to represent.
fn deadline_after(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

/// Whether a non-blocking call that failed with `err` may simply be
/// retried: nothing was ready, or a signal interrupted it.
fn is_retryable(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
}

/// Wait until `fd` is ready for `events` or `deadline` has passed; whether
/// it became ready. An error or hang-up counts as ready, for the following
/// call to report. No deadline waits forever.
fn wait_for(fd: BorrowedFd<'_>, events: PollFlags, deadline: Option<Instant>) -> io::Result<bool> {
    loop {
        let remaining = deadline
            .map(|deadline| Timespec::try_from(deadline.saturating_duration_since(Instant::now())))
            .transpose()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "timeout out of range"))?;
        match poll(
            &mut [PollFd::from_borrowed_fd(fd, events)],
            remaining.as_ref(),
        ) {
            Ok(ready) => return Ok(ready > 0),
            Err(Errno::INTR) => {}
            Err(err) => return Err(err.into()),
        }
    }
}

impl From<OwnedFd> for RfcommStream {
    /// Wrap a connected stream socket.
    fn from(fd: OwnedFd) -> Self {
        Self { fd }
    }
}

impl AsFd for RfcommStream {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Read for &RfcommStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.recv(buf, RecvFlags::empty())
    }
}

impl Read for RfcommStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

/// Each write sends at most [`SPP_CHUNK_SIZE`] bytes, so
/// [`write_all`](Write::write_all) sends a buffer in SPP-sized chunks.
impl Write for &RfcommStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let chunk = &buf[..buf.len().min(SPP_CHUNK_SIZE)];
        // A printer that went away must fail the write, not raise SIGPIPE.
        Ok(send(&self.fd, chunk, SendFlags::NOSIGNAL)?)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Write for RfcommStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        (&*self).flush()
    }
}

/// `struct sockaddr_rc` from `<bluetooth/rfcomm.h>`, with its trailing
/// padding spelt out so that every byte the kernel reads is initialised.
#[repr(C)]
struct SockaddrRc {
    /// `rc_family`: `AF_BLUETOOTH`.
    family: u16,
    /// `rc_bdaddr`, least significant byte first.
    bdaddr: [u8; 6],
    /// `rc_channel`.
    channel: u8,
    padding: u8,
}

/// `sizeof(struct sockaddr_rc)`.
const SOCKADDR_RC_LEN: SocketAddrLen = 10;
const _: () = assert!(size_of::<SockaddrRc>() == 10);

/// The address of an RFCOMM channel, as `connect` takes it.
struct RfcommAddr {
    address: BdAddr,
    channel: Channel,
}

// SAFETY: `with_sockaddr` hands `f` a pointer to a fully initialised
// `sockaddr_rc` on its own stack, together with that struct's exact size,
// and the struct lives until `f` returns.
unsafe impl SocketAddrArg for RfcommAddr {
    unsafe fn with_sockaddr<R>(
        &self,
        f: impl FnOnce(*const SocketAddrOpaque, SocketAddrLen) -> R,
    ) -> R {
        let raw = SockaddrRc {
            family: AddressFamily::BLUETOOTH.as_raw(),
            bdaddr: self.address.to_bdaddr_t(),
            channel: self.channel.get(),
            padding: 0,
        };
        f((&raw const raw).cast(), SOCKADDR_RC_LEN)
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::thread;

    use super::*;

    /// A stream and the printer's end of it.
    fn pair() -> (RfcommStream, UnixStream) {
        let (ours, printer) = UnixStream::pair().expect("socketpair");
        (RfcommStream::from(OwnedFd::from(ours)), printer)
    }

    #[test]
    fn reads_what_the_printer_sends() {
        let (stream, mut printer) = pair();
        printer.write_all(&[0x1a, 0x0f, 0x0c]).expect("send");
        let mut buf = [0; 8];
        let n = (&stream).read(&mut buf).expect("read");
        assert_eq!(&buf[..n], [0x1a, 0x0f, 0x0c]);
    }

    #[test]
    fn a_closed_connection_reads_as_end_of_file() {
        let (stream, printer) = pair();
        drop(printer);
        assert_eq!((&stream).read(&mut [0; 8]).expect("read"), 0);
    }

    #[test]
    fn a_read_times_out_with_would_block() {
        let (stream, _printer) = pair();
        stream
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("timeout");
        let err = (&stream).read(&mut [0; 8]).expect_err("nothing was sent");
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);
    }

    #[test]
    fn read_within_returns_data_or_times_out() {
        let (stream, mut printer) = pair();
        let started = Instant::now();
        let err = stream
            .read_within(&mut [0; 8], Duration::from_millis(30))
            .expect_err("nothing was sent");
        assert_eq!(err.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(30));

        printer.write_all(&[1, 2]).expect("send");
        let mut buf = [0; 8];
        let n = stream
            .read_within(&mut buf, Duration::from_secs(5))
            .expect("read");
        assert_eq!(&buf[..n], [1, 2]);

        drop(printer);
        assert_eq!(
            stream
                .read_within(&mut buf, Duration::from_secs(5))
                .expect("end of file"),
            0
        );
    }

    #[test]
    fn writes_are_split_into_spp_chunks() {
        let (stream, mut printer) = pair();
        let data: Vec<u8> = (0..3000_u32).map(|i| i.to_le_bytes()[0]).collect();
        assert_eq!((&stream).write(&data).expect("write"), SPP_CHUNK_SIZE);

        let reader = thread::spawn(move || {
            let mut received = vec![0; 3000 - SPP_CHUNK_SIZE];
            printer.read_exact(&mut received).expect("receive");
            received
        });
        (&stream)
            .write_all(&data[SPP_CHUNK_SIZE..])
            .expect("write_all");
        assert_eq!(reader.join().expect("reader"), data[SPP_CHUNK_SIZE..]);
    }

    #[test]
    fn writing_to_a_closed_connection_fails() {
        let (stream, printer) = pair();
        drop(printer);
        let err = (&stream).write(&[0; 4]).expect_err("the peer is gone");
        assert_eq!(err.kind(), io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn pending_input_is_taken_without_waiting_and_bounded() {
        let (stream, mut printer) = pair();
        let started = Instant::now();
        assert!(
            stream
                .read_pending(4096)
                .expect("nothing pending")
                .is_empty()
        );

        printer.write_all(&[7; 5000]).expect("send");
        assert_eq!(stream.read_pending(4096).expect("pending").len(), 4096);
        assert_eq!(stream.read_pending(4096).expect("the rest").len(), 904);
        assert!(started.elapsed() < Duration::from_secs(1));

        drop(printer);
        let err = stream.read_pending(4096).expect_err("closed");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }
}
