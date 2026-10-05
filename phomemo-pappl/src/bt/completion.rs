//! Waiting until the printer has printed what it was sent.
//!
//! The printer reports each raster it has printed with `1A 0F 0C`; any
//! other `1A 0F` value is a failure, and `1A 0B B8` a job the printer
//! cancelled (`re/protocol/sequences.md`, "Completion"). Data written to
//! the socket may still be on its way when the driver is done with a job,
//! and closing the link would cut it off, so a job only ends once the
//! printer has reported every page.
//!
//! Reports left over from an earlier job - a raw job, which is not waited
//! for, or one whose wait failed - are taken off the link before a job
//! sends its first page ([`Link::discard_input`]). One the printer has not
//! sent by then still arrives during the next job and counts towards it.
//!
//! # How long a page may take
//!
//! [`page_timeout`]: [`PAGE_MARGIN`] plus the page's length at
//! [`ASSUMED_FEED_RATE`]. On an M220 at its default speed, a 30 mm gap
//! label was reported 6.2 to 6.5 s after it was sent (four labels, one and
//! two per job) - 5 mm/s if all of that were paper moving, which overstates
//! the time per millimetre, since part of it is a fixed delay. The
//! reverse-engineering notes give the speed only as levels 1 to 6
//! (`re/protocol/commands.md`, "Print Speed: `1B 4E 0D XX`"), not in mm/s,
//! and the slowest level may well be slower than the default, so the
//! assumed rate is 2.5 times slower than measured: a 30 mm label may take
//! 30 s, a 2 m one 17 minutes. The deadline only matters when the printer
//! has gone quiet.
use std::error::Error as StdError;
use std::fmt;
use std::io;
use std::time::{Duration, Instant};

use phomemo_protocol::responses::{Decoder, Response};

use super::link::{Link, is_timeout};

/// What every page may take on top of its paper: data still in flight,
/// the printer's own delay before it reports.
pub const PAGE_MARGIN: Duration = Duration::from_secs(15);

/// The paper speed assumed, in hundredths of a millimetre per second.
pub const ASSUMED_FEED_RATE: u32 = 200;

/// How long a page `length` hundredths of a millimetre long may take to
/// be reported printed; see the module documentation.
#[must_use]
pub fn page_timeout(length: u32) -> Duration {
    PAGE_MARGIN + Duration::from_millis(u64::from(length) * 1000 / u64::from(ASSUMED_FEED_RATE))
}

/// Why a job's pages were not all reported printed.
#[derive(Debug)]
pub enum PrintError {
    /// The printer reported a page as failed.
    Failed {
        /// The page, counting from 1.
        page: u32,
    },
    /// The printer cancelled the job.
    Canceled {
        /// The page it was on, counting from 1.
        page: u32,
    },
    /// The printer went quiet.
    TimedOut {
        /// Pages reported printed.
        printed: u32,
        /// Pages sent.
        pages: u32,
        /// How long it was waited for.
        timeout: Duration,
    },
    /// The link failed.
    Link(io::Error),
}

impl fmt::Display for PrintError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Failed { page } => write!(f, "the printer reported page {page} as failed"),
            Self::Canceled { page } => write!(f, "the printer canceled the job at page {page}"),
            Self::TimedOut {
                printed,
                pages,
                timeout,
            } => write!(
                f,
                "the printer reported {printed} of {pages} pages printed, then nothing for {} s",
                timeout.as_secs()
            ),
            Self::Link(err) => write!(f, "the connection failed: {err}"),
        }
    }
}

impl StdError for PrintError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Link(err) => Some(err),
            _ => None,
        }
    }
}

/// Wait until the printer has reported `pages` pages printed, allowing it
/// `page_timeout` for each. Other reports, such as a battery push, are
/// passed over.
///
/// # Errors
///
/// Fails if the printer reports a failure or cancels the job, goes quiet
/// for `page_timeout`, or the link fails - at once if it already has,
/// since what was sent may then never have arrived.
pub fn wait_printed(link: &Link, pages: u32, page_timeout: Duration) -> Result<(), PrintError> {
    if let Some(err) = link.failure() {
        return Err(PrintError::Link(err));
    }
    let mut decoder = Decoder::new();
    let mut printed = 0;
    let mut deadline = Instant::now() + page_timeout;
    while printed < pages {
        let responses = link.receive(&mut decoder, deadline).map_err(|err| {
            if is_timeout(&err) {
                PrintError::TimedOut {
                    printed,
                    pages,
                    timeout: page_timeout,
                }
            } else {
                PrintError::Link(err)
            }
        })?;
        for response in responses {
            match response {
                Response::PrintResult { success: true } => {
                    printed += 1;
                    deadline = Instant::now() + page_timeout;
                }
                Response::PrintResult { success: false } => {
                    return Err(PrintError::Failed { page: printed + 1 });
                }
                Response::PrintCancel => return Err(PrintError::Canceled { page: printed + 1 }),
                _ => {}
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::thread;

    use super::*;
    use crate::bt::link::pair;

    const PRINTED: [u8; 3] = [0x1a, 0x0f, 0x0c];
    const BATTERY: [u8; 3] = [0x1a, 0x04, 0x5a];
    const TIMEOUT: Duration = Duration::from_millis(200);

    fn wait_for(pages: u32, sent: Vec<Vec<u8>>) -> Result<(), PrintError> {
        let (link, mut printer) = pair();
        let printing = thread::spawn(move || {
            for bytes in sent {
                printer.write_all(&bytes).expect("report");
                thread::sleep(Duration::from_millis(5));
            }
            printer
        });
        let result = wait_printed(&link, pages, TIMEOUT);
        drop(printing.join().expect("printer"));
        result
    }

    #[test]
    fn every_page_is_waited_for() {
        let reports = vec![PRINTED.to_vec(), BATTERY.to_vec(), PRINTED.to_vec()];
        assert!(wait_for(2, reports).is_ok());
    }

    #[test]
    fn a_battery_push_first_is_passed_over() {
        let reports = vec![[BATTERY, PRINTED].concat()];
        assert!(wait_for(1, reports).is_ok());
    }

    #[test]
    fn reports_may_arrive_split() {
        let reports = vec![vec![0x1a], vec![0x0f], vec![0x0c, 0x1a, 0x0f], vec![0x0c]];
        assert!(wait_for(2, reports).is_ok());
    }

    #[test]
    fn a_missing_page_times_out() {
        let started = Instant::now();
        let err = wait_for(2, vec![PRINTED.to_vec()]).expect_err("one page short");
        assert!(matches!(
            err,
            PrintError::TimedOut {
                printed: 1,
                pages: 2,
                ..
            }
        ));
        assert!(started.elapsed() >= TIMEOUT);
    }

    #[test]
    fn a_failed_page_fails() {
        let err = wait_for(2, vec![[PRINTED, [0x1a, 0x0f, 0x0d]].concat()])
            .expect_err("the second page failed");
        assert!(matches!(err, PrintError::Failed { page: 2 }));
        assert_eq!(err.to_string(), "the printer reported page 2 as failed");
    }

    #[test]
    fn a_cancel_fails() {
        let err = wait_for(1, vec![vec![0x1a, 0x0b, 0xb8]]).expect_err("cancelled");
        assert!(matches!(err, PrintError::Canceled { page: 1 }));
    }

    #[test]
    fn a_link_that_failed_before_fails_at_once() {
        let (link, printer) = pair();
        drop(printer);
        link.write_all(&[0; 8]).expect_err("closed");
        let started = Instant::now();
        let err = wait_printed(&link, 1, Duration::from_secs(60)).expect_err("failed");
        assert!(matches!(err, PrintError::Link(_)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn pages_get_a_margin_plus_their_length_at_the_assumed_rate() {
        assert_eq!(page_timeout(0), PAGE_MARGIN);
        // A 30 mm label.
        assert_eq!(page_timeout(3000), Duration::from_secs(30));
        // The longest media PAPPL accepts here, 2 m.
        assert_eq!(page_timeout(200_000), Duration::from_secs(1015));
    }

    #[test]
    fn a_closed_link_fails() {
        let (link, printer) = pair();
        drop(printer);
        let err = wait_printed(&link, 1, TIMEOUT).expect_err("closed");
        assert!(matches!(err, PrintError::Link(_)));
    }
}
