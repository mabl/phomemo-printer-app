//! The printer's status, from its answers to the status queries, as
//! `pappl_preason_t` bits.

use std::ffi::c_uint;
use std::time::{Duration, Instant};

use phomemo_protocol::commands::{Command, Query};
use phomemo_protocol::responses::{BatteryStatus, Decoder, Response};

use super::link::{Link, is_timeout};
use crate::pappl::{
    PM_PREASON_COVER_OPEN, PM_PREASON_MARKER_SUPPLY_LOW, PM_PREASON_MEDIA_EMPTY,
    PM_PREASON_OFFLINE, PM_PREASON_OTHER,
};

/// How long the printer has to answer before the queries are sent again.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(1);

/// How often the queries are sent before the status is taken as it is.
const ATTEMPTS: usize = 2;

/// The queries asked: the battery first, whose answer is noted but not
/// waited for - the printer answers in the order it was asked, and a model
/// that does not know the query must not look half-answered - then the
/// three whose answers make up the status.
const QUERIES: [Query; 4] = [
    Query::Battery,
    Query::Cover,
    Query::Paper,
    Query::Temperature,
];

/// Ask the printer for its status, giving it `answer_timeout` per attempt.
///
/// The queries go out in one write. Their answers may come in any
/// order, split or batched, among unsolicited reports; anything said
/// within the deadline counts, including late answers to the first
/// attempt during the second.
///
/// A printer that cannot be written to, closes the link or says nothing at
/// all is offline. One that answers only some queries is reported with
/// what it said plus `other`, so the gap shows.
pub fn query(link: &Link, answer_timeout: Duration) -> c_uint {
    let mut queries = Vec::new();
    for query in QUERIES {
        Command::Query(query).encode_into(&mut queries);
    }

    let mut report = Report::default();
    let mut decoder = Decoder::new();
    for _ in 0..ATTEMPTS {
        if link.write_all(&queries).is_err() {
            return PM_PREASON_OFFLINE;
        }
        let deadline = Instant::now() + answer_timeout;
        while !report.is_complete() {
            match link.receive(&mut decoder, deadline) {
                Ok(responses) => responses.iter().for_each(|response| report.apply(response)),
                Err(err) if is_timeout(&err) => break,
                Err(_) => return PM_PREASON_OFFLINE,
            }
        }
        if report.is_complete() {
            break;
        }
    }
    report.reasons()
}

/// What the printer has said about its state.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Report {
    cover_closed: Option<bool>,
    paper_present: Option<bool>,
    overheated: Option<bool>,
    /// Reasons from reports nobody asked for: battery, busy, canceled.
    unsolicited: c_uint,
    /// The printer said anything documented at all.
    answered: bool,
}

impl Report {
    const fn apply(&mut self, response: &Response) {
        match *response {
            // An undocumented command byte may be line noise: it is no
            // sign that the printer answered.
            Response::Unknown { .. } => return,
            Response::Cover { closed } => self.cover_closed = Some(closed),
            Response::Paper { present } => self.paper_present = Some(present),
            Response::Temperature { overheated } => self.overheated = Some(overheated),
            Response::Battery(BatteryStatus::Level(percent)) if percent <= 10 => {
                self.unsolicited |= PM_PREASON_MARKER_SUPPLY_LOW;
            }
            Response::Battery(BatteryStatus::LowAlarm(_)) => {
                self.unsolicited |= PM_PREASON_MARKER_SUPPLY_LOW;
            }
            Response::WorkStatus { idle: false }
            | Response::PrintBusy { busy: true }
            | Response::PrintCancel => self.unsolicited |= PM_PREASON_OTHER,
            _ => {}
        }
        self.answered = true;
    }

    const fn is_complete(self) -> bool {
        self.cover_closed.is_some() && self.paper_present.is_some() && self.overheated.is_some()
    }

    const fn reasons(self) -> c_uint {
        let mut reasons = self.unsolicited;
        if matches!(self.cover_closed, Some(false)) {
            reasons |= PM_PREASON_COVER_OPEN;
        }
        if matches!(self.paper_present, Some(false)) {
            reasons |= PM_PREASON_MEDIA_EMPTY;
        }
        if matches!(self.overheated, Some(true)) {
            reasons |= PM_PREASON_OTHER;
        }
        if !self.answered {
            reasons |= PM_PREASON_OFFLINE;
        } else if !self.is_complete() {
            reasons |= PM_PREASON_OTHER;
        }
        reasons
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;
    use std::thread;

    use super::*;
    use crate::bt::link::pair;

    const COVER_CLOSED: [u8; 3] = [0x1a, 0x05, 0x98];
    const COVER_OPEN: [u8; 3] = [0x1a, 0x05, 0x99];
    const PAPER_PRESENT: [u8; 3] = [0x1a, 0x06, 0x89];
    const PAPER_OUT: [u8; 3] = [0x1a, 0x06, 0x88];
    const COOL: [u8; 3] = [0x1a, 0x03, 0x00];
    const BATTERY_80: [u8; 3] = [0x1a, 0x04, 80];
    const QUERY_BYTES: [u8; 12] = [
        0x1f, 0x11, 0x08, 0x1f, 0x11, 0x12, 0x1f, 0x11, 0x11, 0x1f, 0x11, 0x13,
    ];

    /// Query a printer that answers each query batch with the next of
    /// `answers`, and return the reasons and the bytes it was sent.
    fn query_printer(answers: Vec<Vec<Vec<u8>>>) -> (c_uint, Vec<u8>) {
        let (link, mut printer) = pair();
        let answering = thread::spawn(move || {
            let mut received = Vec::new();
            for writes in answers {
                let mut queries = [0; QUERY_BYTES.len()];
                if printer.read_exact(&mut queries).is_err() {
                    break;
                }
                received.extend_from_slice(&queries);
                for bytes in writes {
                    printer.write_all(&bytes).expect("answer");
                    thread::sleep(Duration::from_millis(5));
                }
            }
            (received, printer)
        });
        let reasons = query(&link, Duration::from_millis(100));
        let (mut received, mut printer): (Vec<u8>, UnixStream) = answering.join().expect("printer");
        drop(link);
        printer.read_to_end(&mut received).expect("rest");
        (reasons, received)
    }

    #[test]
    fn a_complete_answer_is_the_status() {
        let answer = [COVER_OPEN, PAPER_OUT, COOL].concat();
        let (reasons, sent) = query_printer(vec![vec![answer]]);
        assert_eq!(reasons, PM_PREASON_COVER_OPEN | PM_PREASON_MEDIA_EMPTY);
        assert_eq!(sent, QUERY_BYTES, "answered at the first attempt");
    }

    #[test]
    fn answers_may_be_split_reordered_and_mixed_with_reports() {
        let answer = [BATTERY_80, COOL, PAPER_PRESENT, COVER_CLOSED].concat();
        let writes = answer.chunks(2).map(<[u8]>::to_vec).collect();
        let (reasons, _) = query_printer(vec![writes]);
        assert_eq!(reasons, 0);
    }

    #[test]
    fn the_battery_level_is_noted() {
        let (link, mut printer) = pair();
        let answering = thread::spawn(move || {
            let mut queries = [0; QUERY_BYTES.len()];
            printer.read_exact(&mut queries).expect("queries");
            let answer = [[0x1a, 0x04, 0x64], COVER_CLOSED, PAPER_PRESENT, COOL].concat();
            printer.write_all(&answer).expect("answer");
            printer
        });
        assert_eq!(query(&link, Duration::from_secs(5)), 0);
        assert_eq!(link.battery(), Some(100));
        drop(answering.join().expect("printer"));
    }

    #[test]
    fn a_low_battery_is_a_reason() {
        let answer = [COVER_CLOSED, PAPER_PRESENT, COOL, [0x1a, 0x04, 0xa1]].concat();
        let (reasons, _) = query_printer(vec![vec![answer]]);
        assert_eq!(reasons, PM_PREASON_MARKER_SUPPLY_LOW);
    }

    #[test]
    fn missing_answers_are_asked_for_again() {
        let (reasons, sent) = query_printer(vec![
            vec![COVER_CLOSED.to_vec()],
            vec![[PAPER_PRESENT, COOL].concat()],
        ]);
        assert_eq!(reasons, 0);
        assert_eq!(sent, [QUERY_BYTES, QUERY_BYTES].concat());
    }

    #[test]
    fn a_partial_answer_is_online_but_flagged() {
        let (reasons, _) = query_printer(vec![vec![COVER_OPEN.to_vec()], vec![]]);
        assert_eq!(reasons, PM_PREASON_COVER_OPEN | PM_PREASON_OTHER);
    }

    #[test]
    fn a_silent_printer_is_offline() {
        let (reasons, sent) = query_printer(vec![vec![], vec![]]);
        assert_eq!(reasons, PM_PREASON_OFFLINE);
        assert_eq!(sent.len(), 2 * QUERY_BYTES.len());
    }

    #[test]
    fn line_noise_is_not_an_answer() {
        let (reasons, _) = query_printer(vec![vec![vec![0x1a, 0x99]], vec![]]);
        assert_eq!(reasons, PM_PREASON_OFFLINE);
    }

    #[test]
    fn a_closed_link_is_offline() {
        let (link, printer) = pair();
        drop(printer);
        assert_eq!(query(&link, Duration::from_secs(5)), PM_PREASON_OFFLINE);
    }
}
