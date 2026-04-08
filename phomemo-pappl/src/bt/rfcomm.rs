//! Raw RFCOMM socket I/O via libc.
//!
//! Provides connect/close/read/write for Bluetooth SPP links.
//! Write chunking at 1024 bytes per the Phomemo transport spec.

use std::io;
use std::os::unix::io::RawFd;
use std::time::Duration;

/// SPP write chunk size (from RE: `e50/e.java` field `f311476i = 1024`).
const SPP_CHUNK_SIZE: usize = 1024;

// Bluetooth constants — not in the `libc` crate; values from
// <bluetooth/bluetooth.h> and <bluetooth/rfcomm.h>.
const AF_BLUETOOTH: i32 = 31;
const BTPROTO_RFCOMM: i32 = 3;

/// RFCOMM socket address (matches `struct sockaddr_rc` from `BlueZ`).
#[repr(C)]
struct SockaddrRc {
    family: u16,
    bdaddr: [u8; 6],
    channel: u8,
}

/// An open RFCOMM connection.
pub struct RfcommConnection {
    fd: RawFd,
}

impl RfcommConnection {
    /// The underlying file descriptor.
    pub const fn fd(&self) -> RawFd {
        self.fd
    }
}

#[cfg(test)]
impl RfcommConnection {
    pub(crate) const fn from_raw_fd_for_test(fd: RawFd) -> Self {
        Self { fd }
    }
}

fn socklen_of<T>() -> io::Result<libc::socklen_t> {
    libc::socklen_t::try_from(std::mem::size_of::<T>())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "socklen_t overflow"))
}

fn duration_to_timeval(timeout: Duration) -> io::Result<libc::timeval> {
    let timeout_secs = libc::time_t::try_from(timeout.as_secs())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "timeout seconds overflow"))?;
    let timeout_micros = libc::suseconds_t::from(timeout.subsec_micros());
    Ok(libc::timeval {
        tv_sec: timeout_secs,
        tv_usec: timeout_micros,
    })
}

fn timeval_to_duration(tv: libc::timeval) -> Option<Duration> {
    if tv.tv_sec < 0 || tv.tv_usec < 0 {
        return None;
    }

    let secs = u64::try_from(tv.tv_sec).ok()?;
    let micros = u32::try_from(tv.tv_usec).ok()?;
    Some(Duration::new(secs, micros.saturating_mul(1000)))
}

fn set_nonblocking(fd: RawFd, enabled: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }

    let mut next = flags;
    if enabled {
        next |= libc::O_NONBLOCK;
    } else {
        next &= !libc::O_NONBLOCK;
    }

    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, next) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn connect_with_timeout(fd: RawFd, addr: &SockaddrRc, timeout: Duration) -> io::Result<()> {
    set_nonblocking(fd, true)?;

    let addr_len = socklen_of::<SockaddrRc>()?;
    let rc = unsafe { libc::connect(fd, (&raw const *addr).cast::<libc::sockaddr>(), addr_len) };
    if rc == 0 {
        set_nonblocking(fd, false)?;
        return Ok(());
    }

    let err = io::Error::last_os_error();
    let in_progress = matches!(err.raw_os_error(), Some(code) if code == libc::EINPROGRESS);
    if !in_progress {
        let _ = set_nonblocking(fd, false);
        return Err(err);
    }

    let timeout_ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLOUT,
        revents: 0,
    };
    let poll_rc = unsafe { libc::poll((&raw mut pfd), 1, timeout_ms) };

    if poll_rc == 0 {
        let _ = set_nonblocking(fd, false);
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "RFCOMM connect timed out",
        ));
    }
    if poll_rc < 0 {
        let poll_err = io::Error::last_os_error();
        let _ = set_nonblocking(fd, false);
        return Err(poll_err);
    }

    let mut so_error: libc::c_int = 0;
    let mut so_error_len = socklen_of::<libc::c_int>()?;
    let so_rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_ERROR,
            (&raw mut so_error).cast::<libc::c_void>(),
            (&raw mut so_error_len),
        )
    };

    let restore_result = set_nonblocking(fd, false);
    if so_rc < 0 {
        restore_result?;
        return Err(io::Error::last_os_error());
    }
    if so_error != 0 {
        restore_result?;
        return Err(io::Error::from_raw_os_error(so_error));
    }

    restore_result
}

/// Parse a MAC address string like "AA:BB:CC:DD:EE:FF" into 6 bytes.
///
/// `BlueZ` stores MAC in reverse byte order (little-endian), so this
/// returns the bytes reversed for `sockaddr_rc.rc_bdaddr`.
pub fn parse_mac(s: &str) -> Result<[u8; 6], io::Error> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid MAC"));
    }
    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        mac[5 - i] = u8::from_str_radix(part, 16)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "invalid MAC hex"))?;
    }
    Ok(mac)
}

/// Connect to a Bluetooth device via RFCOMM.
///
/// `mac` is the BlueZ-order (little-endian) 6-byte address.
/// `channel` is the RFCOMM channel (3 for SPP standard, or SDP-resolved).
/// `read_timeout` sets `SO_RCVTIMEO` — critical because PAPPL's
/// `read_cb` has no timeout parameter.
pub fn connect(
    mac: &[u8; 6],
    channel: u8,
    timeout: Duration,
) -> Result<RfcommConnection, io::Error> {
    // Create RFCOMM socket
    let fd = unsafe { libc::socket(AF_BLUETOOTH, libc::SOCK_STREAM, BTPROTO_RFCOMM) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }

    // Set SO_RCVTIMEO
    let tv = duration_to_timeval(timeout)?;
    let tv_len = socklen_of::<libc::timeval>()?;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw const tv).cast::<libc::c_void>(),
            tv_len,
        )
    };
    if rc < 0 {
        unsafe { libc::close(fd) };
        return Err(io::Error::last_os_error());
    }

    // Set SO_SNDTIMEO (3s base + 20ms/byte, matching Windows driver)
    let stv = libc::timeval {
        tv_sec: 5,
        tv_usec: 0,
    };
    let stv_len = socklen_of::<libc::timeval>()?;
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDTIMEO,
            (&raw const stv).cast::<libc::c_void>(),
            stv_len,
        );
    }

    // Set SO_KEEPALIVE
    let keepalive: libc::c_int = 1;
    let keepalive_len = socklen_of::<libc::c_int>()?;
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_KEEPALIVE,
            (&raw const keepalive).cast::<libc::c_void>(),
            keepalive_len,
        );
    }

    // Connect with a bounded timeout.
    let family = u16::try_from(AF_BLUETOOTH)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "AF_BLUETOOTH overflow"))?;
    let addr = SockaddrRc {
        family,
        bdaddr: *mac,
        channel,
    };
    if let Err(err) = connect_with_timeout(fd, &addr, timeout) {
        unsafe { libc::close(fd) };
        return Err(err);
    }

    Ok(RfcommConnection { fd })
}

/// Close an RFCOMM connection.
pub fn close(conn: &mut RfcommConnection) {
    if conn.fd >= 0 {
        unsafe { libc::close(conn.fd) };
        conn.fd = -1;
    }
}

/// Write data with 1024-byte chunking (SPP transport spec).
///
/// Returns total bytes written, or an error.
pub fn write_chunked(conn: &RfcommConnection, data: &[u8]) -> Result<usize, io::Error> {
    let mut offset = 0;
    while offset < data.len() {
        let end = (offset + SPP_CHUNK_SIZE).min(data.len());
        let chunk = &data[offset..end];

        let n = unsafe {
            libc::send(
                conn.fd,
                chunk.as_ptr().cast::<libc::c_void>(),
                chunk.len(),
                0,
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        if n == 0 {
            // Peer closed or transport broken — avoid infinite loop.
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "RFCOMM send returned 0",
            ));
        }
        let written = usize::try_from(n)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative send size"))?;
        offset += written;
    }
    Ok(offset)
}

/// Temporarily set `SO_RCVTIMEO` on the connection, returning the
/// previous value so the caller can restore it.
pub fn set_recv_timeout(conn: &RfcommConnection, timeout: Duration) -> Option<Duration> {
    // Read current value
    let mut old_tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let Ok(mut len) = socklen_of::<libc::timeval>() else {
        return None;
    };
    let rc = unsafe {
        libc::getsockopt(
            conn.fd,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&raw mut old_tv).cast::<libc::c_void>(),
            &raw mut len,
        )
    };
    let prev = if rc == 0 {
        timeval_to_duration(old_tv)
    } else {
        None
    };

    // Set new value
    if let Ok(tv) = duration_to_timeval(timeout) {
        let Ok(tv_len) = socklen_of::<libc::timeval>() else {
            return prev;
        };
        unsafe {
            libc::setsockopt(
                conn.fd,
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                (&raw const tv).cast::<libc::c_void>(),
                tv_len,
            );
        }
    }

    prev
}

/// Read available data from the connection.
///
/// Relies on `SO_RCVTIMEO` set during connect for timeout behavior.
/// Returns bytes read, or 0 on timeout, or an error.
pub fn read(conn: &RfcommConnection, buf: &mut [u8]) -> Result<usize, io::Error> {
    let n = unsafe {
        libc::recv(
            conn.fd,
            buf.as_mut_ptr().cast::<libc::c_void>(),
            buf.len(),
            0,
        )
    };
    if n == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "RFCOMM peer closed connection",
        ));
    }
    if n < 0 {
        let err = io::Error::last_os_error();
        // EAGAIN/EWOULDBLOCK means timeout — return 0 bytes
        if err.kind() == io::ErrorKind::WouldBlock || err.raw_os_error() == Some(libc::EAGAIN) {
            return Ok(0);
        }
        return Err(err);
    }
    usize::try_from(n).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "negative recv size"))
}

/// Drain any pending data from the connection (e.g., unsolicited battery push).
///
/// Reads with a short timeout until no more data arrives.
pub fn drain(conn: &RfcommConnection) -> Vec<u8> {
    let mut collected = Vec::new();
    let mut buf = [0u8; 1024];

    // Temporarily set a short read timeout for draining.
    // Save the previous timeout so we can restore it afterwards.
    let drain_timeout = Duration::from_millis(300);
    let prev = set_recv_timeout(conn, drain_timeout);

    loop {
        match read(conn, &mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => collected.extend_from_slice(&buf[..n]),
        }
    }

    // Restore original timeout
    if let Some(prev) = prev {
        set_recv_timeout(conn, prev);
    }

    collected
}

impl Drop for RfcommConnection {
    fn drop(&mut self) {
        close(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::RawFd;

    fn make_socketpair() -> Result<[RawFd; 2], io::Error> {
        let mut fds = [0; 2];
        let rc = unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_STREAM, 0, fds.as_mut_ptr()) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(fds)
    }

    #[test]
    fn read_returns_unexpected_eof_when_peer_closes() {
        let [fd_local, fd_peer] = make_socketpair().expect("socketpair must succeed");
        let conn = RfcommConnection { fd: fd_local };

        unsafe {
            libc::close(fd_peer);
        }

        let mut buf = [0u8; 16];
        let err = read(&conn, &mut buf).expect_err("closed peer must produce EOF");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_returns_zero_on_would_block() {
        let [fd_local, fd_peer] = make_socketpair().expect("socketpair must succeed");
        let conn = RfcommConnection { fd: fd_local };

        let flags = unsafe { libc::fcntl(fd_local, libc::F_GETFL) };
        assert!(flags >= 0, "fcntl(F_GETFL) must succeed");

        let rc = unsafe { libc::fcntl(fd_local, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(rc, 0, "fcntl(F_SETFL) must succeed");

        let mut buf = [0u8; 16];
        let n = read(&conn, &mut buf).expect("would-block should map to Ok(0)");
        assert_eq!(n, 0);

        unsafe {
            libc::close(fd_peer);
        }
    }
}
