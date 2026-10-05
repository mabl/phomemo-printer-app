//! Decoding what the printer sends back.
//!
//! Every response is a frame `1A cmd payload`, whose payload length is fixed
//! by `cmd` (`re/protocol/responses.md`, "Complete Response Table"). Frames
//! arrive in batches, in no particular order relative to the queries that
//! prompted them, and interleaved with unsolicited ones such as battery
//! pushes. A read may also end mid-frame.
//!
//! [`Decoder`] handles all of that: feed it bytes as they arrive and take
//! complete responses out. [`parse_frame`] decodes a single frame.

use std::iter::FusedIterator;

const FRAME_START: u8 = 0x1a;

/// A response from the printer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// `1A 03 nn`: print head temperature.
    Temperature {
        /// The head is too hot to print (`A9`).
        overheated: bool,
    },
    /// `1A 04 nn`: battery state, pushed unsolicited (on connection, among
    /// others).
    Battery(BatteryStatus),
    /// `1A 05 nn`: cover state.
    Cover {
        /// The cover is closed (`98`).
        closed: bool,
    },
    /// `1A 06 nn`: paper state.
    Paper {
        /// Paper is loaded (`89`).
        present: bool,
    },
    /// `1A 07 aa bb cc`: firmware version `aa.bb.cc`.
    FirmwareVersion {
        /// Major version.
        major: u8,
        /// Minor version.
        minor: u8,
        /// Patch version.
        patch: u8,
    },
    /// `1A 08` + 15 ASCII bytes: serial number, which is also the Bluetooth
    /// name of some models.
    SerialNumber(String),
    /// `1A 09 nn`: auto power-off delay.
    AutoOff {
        /// Minutes of idleness before power-off (`nn * 5`); 0 = never.
        minutes: u16,
    },
    /// `1A 0B B8`: the printer cancelled the job.
    PrintCancel,
    /// `1A 0C nn`: detected paper type, a
    /// [`Command::SetMediaTracking`](crate::commands::Command::SetMediaTracking)
    /// code. Print Master reads `0B` as continuous, `26` as mark and
    /// anything else as gap.
    PaperType {
        /// The paper-type code.
        code: u8,
    },
    /// `1A 0D`: acknowledgement, no content.
    Ack,
    /// `1A 0E nn`: cutter button state, from models with a cutter.
    Cutter {
        /// The button is pressed (`B8`).
        pressed: bool,
    },
    /// `1A 0F nn`: the job finished.
    PrintResult {
        /// It printed (`0C`).
        success: bool,
    },
    /// `1A 17 nn`: controller chip type (3, 7 or 8 = Jieli).
    ChipType {
        /// The chip type.
        chip: u8,
    },
    /// `1A 1D nn`: whether the printer is working.
    WorkStatus {
        /// Nothing in progress (`00`).
        idle: bool,
    },
    /// `1A 35 nn`: charging state, pushed unsolicited when it changes.
    ChargeStatus {
        /// The battery is charging (`02`).
        charging: bool,
    },
    /// `1A 3B` + 5 bytes: capability bitmap (`re/protocol/responses.md`,
    /// "Capability Bitmap"). Not all firmware sends it.
    Capabilities {
        /// Bytes D0-D4.
        data: [u8; 5],
    },
    /// `1A 3E nn`: whether a job is printing.
    PrintBusy {
        /// A job is printing (non-zero).
        busy: bool,
    },
    /// A documented response this crate does not interpret. Its whole
    /// frame, payload included, was consumed.
    Other {
        /// The frame's command byte.
        cmd: u8,
    },
    /// `1A` followed by an undocumented command byte - a response this crate
    /// does not know, or line noise. Its length is unknown, so only those two
    /// bytes were consumed and whatever follows is scanned again for frames.
    Unknown {
        /// The command byte.
        cmd: u8,
    },
}

/// Battery state from `1A 04 nn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatteryStatus {
    /// Charge level in percent.
    Level(u8),
    /// Low-battery alarm: `A1` (10 %), `A2` (5 %) or `A3` (3 %).
    LowAlarm(u8),
    /// Powered by non-rechargeable dry cells (`A4`).
    DryCell,
}

impl BatteryStatus {
    const fn from_code(code: u8) -> Self {
        match code {
            0xa1..=0xa3 => Self::LowAlarm(code),
            0xa4 => Self::DryCell,
            level => Self::Level(level),
        }
    }
}

/// Payload bytes after `1A cmd`, or `None` if `cmd` is undocumented.
///
/// Battery health (`1A 49`) has one payload byte when unsupported (`00`)
/// and two otherwise, so its length is read from the payload seen so far.
/// Responses of variable length - Wi-Fi SSID, IP address, domain name,
/// firmware-upload results and the consumable UID - are left undocumented
/// here: they belong to network, firmware and RFID features this crate does
/// not drive.
const fn payload_len(cmd: u8, payload: &[u8]) -> Option<usize> {
    let len = match cmd {
        0x0d | 0x18 | 0x3c => 0,
        0x03 | 0x04 | 0x05 | 0x06 | 0x09 | 0x0b | 0x0c | 0x0e | 0x0f | 0x16 | 0x17 | 0x1d
        | 0x20 | 0x34 | 0x35 | 0x38 | 0x3e | 0x3f | 0x5c | 0x5d | 0x5e | 0x60 => 1,
        0x4b | 0x6e => 2,
        0x07 | 0x15 | 0x31 | 0x58 => 3,
        0x3b => 5,
        0x6d => 6,
        0x40 => 14,
        0x08 => 15,
        0x49 => match payload.first() {
            Some(0) | None => 1,
            Some(_) => 2,
        },
        _ => return None,
    };
    Some(len)
}

impl Response {
    /// Decode a frame whose payload has the documented length for `cmd`.
    fn decode(cmd: u8, payload: &[u8]) -> Self {
        match (cmd, payload) {
            (0x03, &[state]) => Self::Temperature {
                overheated: state == 0xa9,
            },
            (0x04, &[code]) => Self::Battery(BatteryStatus::from_code(code)),
            (0x05, &[state]) => Self::Cover {
                closed: state == 0x98,
            },
            (0x06, &[state]) => Self::Paper {
                present: state == 0x89,
            },
            (0x07, &[major, minor, patch]) => Self::FirmwareVersion {
                major,
                minor,
                patch,
            },
            (0x08, serial) => Self::SerialNumber(String::from_utf8_lossy(serial).into_owned()),
            (0x09, &[units]) => Self::AutoOff {
                minutes: u16::from(units) * 5,
            },
            (0x0b, _) => Self::PrintCancel,
            (0x0c, &[code]) => Self::PaperType { code },
            (0x0d, _) => Self::Ack,
            (0x0e, &[state]) => Self::Cutter {
                pressed: state == 0xb8,
            },
            (0x0f, &[result]) => Self::PrintResult {
                success: result == 0x0c,
            },
            (0x17, &[chip]) => Self::ChipType { chip },
            (0x1d, &[state]) => Self::WorkStatus { idle: state == 0 },
            (0x35, &[state]) => Self::ChargeStatus {
                charging: state == 0x02,
            },
            (0x3b, &[d0, d1, d2, d3, d4]) => Self::Capabilities {
                data: [d0, d1, d2, d3, d4],
            },
            (0x3e, &[state]) => Self::PrintBusy { busy: state != 0 },
            _ => Self::Other { cmd },
        }
    }
}

/// The result of [`parse_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parse {
    /// A response occupying the first `len` bytes of the input.
    Frame {
        /// The decoded response.
        response: Response,
        /// Bytes it occupied.
        len: usize,
    },
    /// The input is the start of a frame; more bytes are needed.
    Incomplete,
    /// The input does not start with a frame: its first byte is not `1A`.
    Invalid,
}

/// Decode the frame at the start of `buf`.
#[must_use]
pub fn parse_frame(buf: &[u8]) -> Parse {
    match buf {
        [] | [FRAME_START] => Parse::Incomplete,
        [FRAME_START, cmd, rest @ ..] => {
            let Some(len) = payload_len(*cmd, rest) else {
                return Parse::Frame {
                    response: Response::Unknown { cmd: *cmd },
                    len: 2,
                };
            };
            rest.get(..len)
                .map_or(Parse::Incomplete, |payload| Parse::Frame {
                    response: Response::decode(*cmd, payload),
                    len: 2 + len,
                })
        }
        [_, ..] => Parse::Invalid,
    }
}

/// An incremental response decoder.
///
/// Feed it bytes as they are read, with [`extend_from_slice`] or
/// [`Extend`], then drain complete responses with [`responses`]. Bytes
/// that cannot start a frame are skipped, so the decoder resynchronises
/// after line noise; a partial frame stays buffered until the rest arrives.
///
/// ```
/// use phomemo_protocol::responses::{Decoder, Response};
///
/// let mut decoder = Decoder::new();
/// decoder.extend_from_slice(&[0x1a, 0x05, 0x98, 0x1a, 0x06]);
/// assert_eq!(decoder.responses().collect::<Vec<_>>(), [Response::Cover { closed: true }]);
/// decoder.extend_from_slice(&[0x89]);
/// assert_eq!(decoder.responses().next(), Some(Response::Paper { present: true }));
/// ```
///
/// [`extend_from_slice`]: Self::extend_from_slice
/// [`responses`]: Self::responses
#[derive(Debug, Clone, Default)]
pub struct Decoder {
    buf: Vec<u8>,
    /// Bytes of `buf` already decoded or skipped.
    consumed: usize,
}

impl Decoder {
    /// A decoder with nothing buffered.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: Vec::new(),
            consumed: 0,
        }
    }

    /// Append bytes read from the printer.
    pub fn extend_from_slice(&mut self, bytes: &[u8]) {
        self.compact();
        self.buf.extend_from_slice(bytes);
    }

    /// Drain the complete responses buffered so far, in arrival order.
    ///
    /// Responses left unread when the iterator is dropped stay buffered.
    pub const fn responses(&mut self) -> Responses<'_> {
        Responses(self)
    }

    /// Bytes buffered but not yet decoded: the start of an incomplete frame,
    /// or bytes not yet scanned.
    #[must_use]
    pub fn pending(&self) -> &[u8] {
        &self.buf[self.consumed..]
    }

    fn next_response(&mut self) -> Option<Response> {
        loop {
            match parse_frame(self.pending()) {
                Parse::Frame { response, len } => {
                    self.consumed += len;
                    return Some(response);
                }
                Parse::Invalid => self.consumed += 1,
                Parse::Incomplete => return None,
            }
        }
    }

    fn compact(&mut self) {
        self.buf.drain(..self.consumed);
        self.consumed = 0;
    }
}

impl Extend<u8> for Decoder {
    fn extend<I: IntoIterator<Item = u8>>(&mut self, bytes: I) {
        self.compact();
        self.buf.extend(bytes);
    }
}

impl<'a> Extend<&'a u8> for Decoder {
    fn extend<I: IntoIterator<Item = &'a u8>>(&mut self, bytes: I) {
        self.extend(bytes.into_iter().copied());
    }
}

/// Iterator over the complete responses in a [`Decoder`]; see
/// [`Decoder::responses`].
#[derive(Debug)]
pub struct Responses<'a>(&'a mut Decoder);

impl Iterator for Responses<'_> {
    type Item = Response;

    fn next(&mut self) -> Option<Response> {
        self.0.next_response()
    }
}

impl FusedIterator for Responses<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(bytes: &[u8]) -> Response {
        match parse_frame(bytes) {
            Parse::Frame { response, len } => {
                assert_eq!(len, bytes.len(), "frame length of {bytes:02x?}");
                response
            }
            other => panic!("{bytes:02x?} is not one frame: {other:?}"),
        }
    }

    fn decode_all(bytes: &[u8]) -> (Vec<Response>, Vec<u8>) {
        let mut decoder = Decoder::new();
        decoder.extend_from_slice(bytes);
        let responses = decoder.responses().collect();
        (responses, decoder.pending().to_vec())
    }

    #[test]
    fn status_frames() {
        assert_eq!(frame(&[0x1a, 0x05, 0x98]), Response::Cover { closed: true });
        assert_eq!(
            frame(&[0x1a, 0x05, 0x99]),
            Response::Cover { closed: false }
        );
        assert_eq!(
            frame(&[0x1a, 0x06, 0x89]),
            Response::Paper { present: true }
        );
        assert_eq!(
            frame(&[0x1a, 0x06, 0x88]),
            Response::Paper { present: false }
        );
        assert_eq!(
            frame(&[0x1a, 0x03, 0xa9]),
            Response::Temperature { overheated: true }
        );
        assert_eq!(
            frame(&[0x1a, 0x03, 0xa8]),
            Response::Temperature { overheated: false }
        );
    }

    #[test]
    fn battery_frames() {
        let battery = |code| frame(&[0x1a, 0x04, code]);
        assert_eq!(battery(0x14), Response::Battery(BatteryStatus::Level(20)));
        assert_eq!(
            battery(0xa1),
            Response::Battery(BatteryStatus::LowAlarm(0xa1))
        );
        assert_eq!(
            battery(0xa3),
            Response::Battery(BatteryStatus::LowAlarm(0xa3))
        );
        assert_eq!(battery(0xa4), Response::Battery(BatteryStatus::DryCell));
    }

    #[test]
    fn device_info_frames() {
        assert_eq!(
            frame(&[0x1a, 0x07, 0x03, 0x00, 0x01]),
            Response::FirmwareVersion {
                major: 3,
                minor: 0,
                patch: 1
            }
        );
        let mut serial = vec![0x1a, 0x08];
        serial.extend_from_slice(b"Q198G5949230062");
        assert_eq!(
            frame(&serial),
            Response::SerialNumber("Q198G5949230062".into())
        );
        assert_eq!(frame(&[0x1a, 0x09, 0x00]), Response::AutoOff { minutes: 0 });
        assert_eq!(
            frame(&[0x1a, 0x09, 0x06]),
            Response::AutoOff { minutes: 30 }
        );
        assert_eq!(frame(&[0x1a, 0x17, 0x03]), Response::ChipType { chip: 3 });
    }

    #[test]
    fn job_frames() {
        assert_eq!(
            frame(&[0x1a, 0x0f, 0x0c]),
            Response::PrintResult { success: true }
        );
        assert_eq!(
            frame(&[0x1a, 0x0f, 0x01]),
            Response::PrintResult { success: false }
        );
        assert_eq!(frame(&[0x1a, 0x0b, 0xb8]), Response::PrintCancel);
        assert_eq!(
            frame(&[0x1a, 0x3e, 0x01]),
            Response::PrintBusy { busy: true }
        );
        assert_eq!(
            frame(&[0x1a, 0x1d, 0x00]),
            Response::WorkStatus { idle: true }
        );
        assert_eq!(frame(&[0x1a, 0x0d]), Response::Ack);
    }

    #[test]
    fn other_documented_frames() {
        assert_eq!(
            frame(&[0x1a, 0x0c, 0x26]),
            Response::PaperType { code: 0x26 }
        );
        assert_eq!(
            frame(&[0x1a, 0x0e, 0xb8]),
            Response::Cutter { pressed: true }
        );
        assert_eq!(
            frame(&[0x1a, 0x35, 0x02]),
            Response::ChargeStatus { charging: true }
        );
        assert_eq!(
            frame(&[0x1a, 0x3b, 1, 2, 3, 4, 5]),
            Response::Capabilities {
                data: [1, 2, 3, 4, 5]
            }
        );
    }

    #[test]
    fn uninterpreted_frames_are_consumed_whole() {
        assert_eq!(frame(&[0x1a, 0x20, 0x01]), Response::Other { cmd: 0x20 });
        assert_eq!(
            frame(&[0x1a, 0x40, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
            Response::Other { cmd: 0x40 }
        );
    }

    #[test]
    fn battery_health_length_depends_on_support() {
        assert_eq!(frame(&[0x1a, 0x49, 0x00]), Response::Other { cmd: 0x49 });
        assert_eq!(
            frame(&[0x1a, 0x49, 0x01, 0x50]),
            Response::Other { cmd: 0x49 }
        );
        assert_eq!(parse_frame(&[0x1a, 0x49]), Parse::Incomplete);
        assert_eq!(parse_frame(&[0x1a, 0x49, 0x01]), Parse::Incomplete);
    }

    #[test]
    fn undocumented_commands_consume_their_header() {
        assert_eq!(
            parse_frame(&[0x1a, 0x99, 0xde]),
            Parse::Frame {
                response: Response::Unknown { cmd: 0x99 },
                len: 2
            }
        );
    }

    #[test]
    fn parse_frame_distinguishes_incomplete_from_invalid() {
        assert_eq!(parse_frame(&[]), Parse::Incomplete);
        assert_eq!(parse_frame(&[0x1a]), Parse::Incomplete);
        assert_eq!(parse_frame(&[0x1a, 0x05]), Parse::Incomplete);
        assert_eq!(parse_frame(&[0x1a, 0x08, b'Q']), Parse::Incomplete);
        assert_eq!(parse_frame(&[0xff, 0x05, 0x98]), Parse::Invalid);
    }

    #[test]
    fn decodes_a_validated_batch() {
        // `re/VALIDATION_FINDINGS.md`: the M220's answer to the handshake.
        let mut batch = vec![0x1a, 0x17, 0x03, 0x1a, 0x07, 0x03, 0x00, 0x01, 0x1a, 0x08];
        batch.extend_from_slice(b"Q198G5949230062");
        batch.extend_from_slice(&[
            0x1a, 0x04, 0x14, 0x1a, 0x09, 0x00, 0x1a, 0x05, 0x98, 0x1a, 0x06, 0x89,
        ]);
        let (responses, pending) = decode_all(&batch);
        assert_eq!(
            responses,
            [
                Response::ChipType { chip: 3 },
                Response::FirmwareVersion {
                    major: 3,
                    minor: 0,
                    patch: 1
                },
                Response::SerialNumber("Q198G5949230062".into()),
                Response::Battery(BatteryStatus::Level(20)),
                Response::AutoOff { minutes: 0 },
                Response::Cover { closed: true },
                Response::Paper { present: true },
            ]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn skips_leading_garbage() {
        let (responses, pending) = decode_all(&[0xff, 0xab, 0x1a, 0x05, 0x98]);
        assert_eq!(responses, [Response::Cover { closed: true }]);
        assert!(pending.is_empty());
    }

    #[test]
    fn skips_garbage_between_frames() {
        let (responses, pending) =
            decode_all(&[0xff, 0x1a, 0x05, 0x99, 0x00, 0x00, 0x1a, 0x06, 0x88]);
        assert_eq!(
            responses,
            [
                Response::Cover { closed: false },
                Response::Paper { present: false }
            ]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn discards_all_garbage() {
        let (responses, pending) = decode_all(&[0xff, 0xfe, 0xfd]);
        assert!(responses.is_empty());
        assert!(pending.is_empty());
    }

    #[test]
    fn keeps_a_truncated_frame_for_later() {
        let (responses, pending) = decode_all(&[0x1a, 0x05, 0x98, 0x1a]);
        assert_eq!(responses, [Response::Cover { closed: true }]);
        assert_eq!(pending, [0x1a]);
    }

    #[test]
    fn resynchronises_after_an_undocumented_command() {
        let (responses, pending) = decode_all(&[0x1a, 0x99, 0xde, 0xad, 0x1a, 0x05, 0x98]);
        assert_eq!(
            responses,
            [
                Response::Unknown { cmd: 0x99 },
                Response::Cover { closed: true }
            ]
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn undocumented_command_before_a_truncated_frame() {
        let (responses, pending) = decode_all(&[0x1a, 0x99, 0x1a, 0x05]);
        assert_eq!(responses, [Response::Unknown { cmd: 0x99 }]);
        assert_eq!(pending, [0x1a, 0x05]);
    }

    #[test]
    fn reassembles_frames_split_across_reads() {
        // A print result arriving byte by byte amid an unsolicited battery
        // push, as the driver sees it while waiting for a job to finish.
        let stream = [0x1a, 0x04, 0x50, 0x1a, 0x0f, 0x0c];
        let mut decoder = Decoder::new();
        let mut responses = Vec::new();
        for byte in stream {
            decoder.extend_from_slice(&[byte]);
            responses.extend(decoder.responses());
        }
        assert_eq!(
            responses,
            [
                Response::Battery(BatteryStatus::Level(80)),
                Response::PrintResult { success: true },
            ]
        );
        assert!(decoder.pending().is_empty());
    }

    #[test]
    fn unread_responses_stay_buffered() {
        let mut decoder = Decoder::new();
        decoder.extend([0x1a, 0x05, 0x98, 0x1a, 0x06, 0x89]);
        assert_eq!(
            decoder.responses().next(),
            Some(Response::Cover { closed: true })
        );
        assert_eq!(decoder.pending(), [0x1a, 0x06, 0x89]);
        decoder.extend(&[0x1a, 0x0d]);
        assert_eq!(
            decoder.responses().collect::<Vec<_>>(),
            [Response::Paper { present: true }, Response::Ack]
        );
    }
}
