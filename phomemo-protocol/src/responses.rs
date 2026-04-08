//! Phomemo RX response parsing.
//!
//! All standard responses start with `0x1A` followed by a command byte.
//! Responses arrive as batches and may be out of order relative to queries.
//! The parser iterates through the buffer consuming each packet by its
//! known size.

/// Parsed response from the printer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Response {
    /// Temperature status: `1A 03 XX`.
    Temperature { overheated: bool },
    /// Battery level (unsolicited push): `1A 04 XX`.
    Battery(BatteryStatus),
    /// Cover status: `1A 05 XX`.
    Cover { closed: bool },
    /// Paper status: `1A 06 XX`.
    Paper { present: bool },
    /// Firmware version: `1A 07 v1 v2 v3`.
    FirmwareVersion { major: u8, minor: u8, patch: u8 },
    /// Serial number: `1A 08` + 15 ASCII bytes.
    SerialNumber(String),
    /// Auto-off timer: `1A 09 XX` — value * 5 = minutes.  0 = disabled.
    AutoOff { raw: u8 },
    /// Print cancel (printer-initiated): `1A 0B B8`.
    PrintCancel,
    /// Paper type: `1A 0C XX`.
    PaperType { raw: u8 },
    /// Acknowledged (no-op): `1A 0D`.
    Ack,
    /// Cutter status: `1A 0E XX`.
    Cutter { pressed: bool },
    /// Print result: `1A 0F XX`.
    PrintResult { success: bool },
    /// Chip type: `1A 17 XX`.
    ChipType { chip: u8 },
    /// Work status: `1A 1D XX`.
    WorkStatus { idle: bool },
    /// Charge status (unsolicited): `1A 35 XX`.
    ChargeStatus { charging: bool },
    /// Capability bitmap: `1A 3B D0..D4`.
    Capabilities { data: [u8; 5] },
    /// Print busy: `1A 3E XX`.
    PrintBusy { busy: bool },
    /// Unknown/unrecognised response.
    Unknown { cmd: u8 },
}

/// Battery status, decoded from `1A 04 XX`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatteryStatus {
    /// Raw percentage (0-100).
    Level(u8),
    /// Low battery alarm (10% / 5% / 3%).
    LowAlarm(u8),
    /// Non-rechargeable dry cell battery.
    DryCell,
}

// Response byte constants
const TEMP_OVERHEATED: u8 = 0xa9;
const COVER_CLOSED: u8 = 0x98;
const PAPER_PRESENT: u8 = 0x89;
const CUTTER_PRESSED: u8 = 0xb8;
const PRINT_COMPLETE: u8 = 0x0c;
const BATTERY_ALARM_10: u8 = 0xa1;
const BATTERY_ALARM_5: u8 = 0xa2;
const BATTERY_ALARM_3: u8 = 0xa3;
const BATTERY_DRY_CELL: u8 = 0xa4;
const CHARGE_CHARGING: u8 = 0x02;

/// Response sizes by command byte.  Returns `None` for variable-length
/// responses that need special handling, or unknown command bytes.
const fn response_size(cmd: u8) -> Option<usize> {
    match cmd {
        0x03 | 0x04 | 0x05 | 0x06 | 0x09 | 0x0b | 0x0c | 0x0e | 0x0f | 0x16 | 0x17 | 0x1d
        | 0x20 | 0x34 | 0x35 | 0x38 | 0x3e | 0x3f | 0x49 | 0x5c | 0x5d | 0x5e | 0x60 => Some(3),
        0x07 | 0x15 | 0x31 | 0x58 => Some(5),
        0x3b => Some(7),
        0x08 => Some(17),
        0x0d | 0x18 | 0x3c => Some(2),
        0x4b | 0x6e => Some(4),
        0x6d => Some(8),
        0x40 => Some(16),
        _ => None,
    }
}

/// Try to parse the next response from `buf`, returning the response
/// and the number of bytes consumed.
///
/// Returns `None` if the buffer doesn't start with `0x1A` or is too
/// short for a complete response.
#[must_use]
pub fn parse_one(buf: &[u8]) -> Option<(Response, usize)> {
    if buf.is_empty() || buf[0] != 0x1a {
        return None;
    }
    if buf.len() < 2 {
        return None;
    }

    let cmd = buf[1];
    let size = response_size(cmd)?;

    if buf.len() < size {
        return None;
    }

    let resp = match cmd {
        0x03 => Response::Temperature {
            overheated: buf[2] == TEMP_OVERHEATED,
        },
        0x04 => {
            let v = buf[2];
            let status = match v {
                BATTERY_ALARM_10 | BATTERY_ALARM_5 | BATTERY_ALARM_3 => BatteryStatus::LowAlarm(v),
                BATTERY_DRY_CELL => BatteryStatus::DryCell,
                _ => BatteryStatus::Level(v),
            };
            Response::Battery(status)
        }
        0x05 => Response::Cover {
            closed: buf[2] == COVER_CLOSED,
        },
        0x06 => Response::Paper {
            present: buf[2] == PAPER_PRESENT,
        },
        0x07 => Response::FirmwareVersion {
            major: buf[2],
            minor: buf[3],
            patch: buf[4],
        },
        0x08 => {
            let s = String::from_utf8_lossy(&buf[2..17]).into_owned();
            Response::SerialNumber(s)
        }
        0x09 => Response::AutoOff { raw: buf[2] },
        0x0b => Response::PrintCancel,
        0x0c => Response::PaperType { raw: buf[2] },
        0x0d => Response::Ack,
        0x0e => Response::Cutter {
            pressed: buf[2] == CUTTER_PRESSED,
        },
        0x0f => Response::PrintResult {
            success: buf[2] == PRINT_COMPLETE,
        },
        0x17 => Response::ChipType { chip: buf[2] },
        0x1d => Response::WorkStatus { idle: buf[2] == 0 },
        0x35 => Response::ChargeStatus {
            charging: buf[2] == CHARGE_CHARGING,
        },
        0x3b => {
            let mut data = [0u8; 5];
            data.copy_from_slice(&buf[2..7]);
            Response::Capabilities { data }
        }
        0x3e => Response::PrintBusy { busy: buf[2] != 0 },
        _ => Response::Unknown { cmd },
    };

    Some((resp, size))
}

/// Parse all complete responses from a buffer.
#[must_use]
pub fn parse_all(buf: &[u8]) -> Vec<Response> {
    parse_all_resilient(buf).0
}

/// Parse all complete responses, skipping over leading garbage bytes.
///
/// Returns (parsed responses, number of bytes consumed from `buf`).
/// This is resilient to framing errors — if the buffer contains bytes
/// that are not a valid `0x1A` header, they are skipped until the next
/// valid response start is found.
#[must_use]
pub fn parse_all_resilient(mut buf: &[u8]) -> (Vec<Response>, usize) {
    let mut responses = Vec::new();
    let original_len = buf.len();

    while !buf.is_empty() {
        if let Some((resp, consumed)) = parse_one(buf) {
            responses.push(resp);
            buf = &buf[consumed..];
        } else if buf[0] != 0x1a {
            // Skip non-0x1A garbage byte and try the next position.
            buf = &buf[1..];
        } else if buf.len() < 2 {
            // Trailing 0x1A without command byte: keep it for next read.
            break;
        } else if response_size(buf[1]).is_none() {
            // Unknown command after a valid 0x1A prefix. Consume just the
            // 2-byte header so parsing can continue for later known frames.
            buf = &buf[2..];
        } else {
            // Starts with 0x1A but not parseable (truncated or
            // truncated known response) — stop here.
            break;
        }
    }

    (responses, original_len - buf.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cover_closed() {
        let buf = [0x1a, 0x05, 0x98];
        let (resp, consumed) = parse_one(&buf).unwrap();
        assert_eq!(consumed, 3);
        assert_eq!(resp, Response::Cover { closed: true });
    }

    #[test]
    fn test_parse_cover_open() {
        let buf = [0x1a, 0x05, 0x99];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Cover { closed: false });
    }

    #[test]
    fn test_parse_paper_present() {
        let buf = [0x1a, 0x06, 0x89];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Paper { present: true });
    }

    #[test]
    fn test_parse_paper_absent() {
        let buf = [0x1a, 0x06, 0x88];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Paper { present: false });
    }

    #[test]
    fn test_parse_firmware_version() {
        let buf = [0x1a, 0x07, 0x03, 0x00, 0x01];
        let (resp, consumed) = parse_one(&buf).unwrap();
        assert_eq!(consumed, 5);
        assert_eq!(
            resp,
            Response::FirmwareVersion {
                major: 3,
                minor: 0,
                patch: 1
            }
        );
    }

    #[test]
    fn test_parse_serial_number() {
        let mut buf = vec![0x1a, 0x08];
        buf.extend_from_slice(b"Q198G594923006"); // 14 chars
        buf.push(b'2'); // 15th char
        let (resp, consumed) = parse_one(&buf).unwrap();
        assert_eq!(consumed, 17);
        assert!(matches!(resp, Response::SerialNumber(s) if s == "Q198G5949230062"));
    }

    #[test]
    fn test_parse_battery_level() {
        let buf = [0x1a, 0x04, 0x14]; // 20%
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Battery(BatteryStatus::Level(20)));
    }

    #[test]
    fn test_parse_battery_alarm() {
        let buf = [0x1a, 0x04, 0xa1];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Battery(BatteryStatus::LowAlarm(0xa1)));
    }

    #[test]
    fn test_parse_battery_dry_cell() {
        let buf = [0x1a, 0x04, 0xa4];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Battery(BatteryStatus::DryCell));
    }

    #[test]
    fn test_parse_temperature() {
        let buf = [0x1a, 0x03, 0xa9];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::Temperature { overheated: true });
    }

    #[test]
    fn test_parse_chip_type() {
        let buf = [0x1a, 0x17, 0x03]; // Jieli chip 3
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::ChipType { chip: 3 });
    }

    #[test]
    fn test_parse_print_complete() {
        let buf = [0x1a, 0x0f, 0x0c];
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::PrintResult { success: true });
    }

    #[test]
    fn test_parse_auto_off() {
        let buf = [0x1a, 0x09, 0x00]; // disabled
        let (resp, _) = parse_one(&buf).unwrap();
        assert_eq!(resp, Response::AutoOff { raw: 0 });
    }

    #[test]
    fn test_parse_batch() {
        // M220 validated: chip type + cover + paper in one buffer
        let buf = [
            0x1a, 0x17, 0x03, // chip type = 3
            0x1a, 0x05, 0x98, // cover closed
            0x1a, 0x06, 0x89, // paper present
        ];
        let responses = parse_all(&buf);
        assert_eq!(responses.len(), 3);
        assert_eq!(responses[0], Response::ChipType { chip: 3 });
        assert_eq!(responses[1], Response::Cover { closed: true });
        assert_eq!(responses[2], Response::Paper { present: true });
    }

    #[test]
    fn test_parse_empty() {
        assert!(parse_one(&[]).is_none());
        assert_eq!(parse_all(&[]).len(), 0);
    }

    #[test]
    fn test_parse_truncated() {
        // Only 1 byte — too short for any response
        assert!(parse_one(&[0x1a]).is_none());
        // 2 bytes but cover needs 3
        assert!(parse_one(&[0x1a, 0x05]).is_none());
    }

    #[test]
    fn test_parse_non_1a_prefix() {
        assert!(parse_one(&[0xff, 0x05, 0x98]).is_none());
    }

    #[test]
    fn test_resilient_skips_garbage() {
        // 2 garbage bytes, then a valid cover response
        let buf = [0xff, 0xab, 0x1a, 0x05, 0x98];
        let (responses, consumed) = parse_all_resilient(&buf);
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0], Response::Cover { closed: true });
        assert_eq!(consumed, 5);
    }

    #[test]
    fn test_resilient_mixed_garbage_and_responses() {
        // garbage, cover, garbage, paper
        let buf = [
            0xff, // garbage
            0x1a, 0x05, 0x99, // cover open
            0x00, 0x00, // garbage
            0x1a, 0x06, 0x88, // paper absent
        ];
        let (responses, consumed) = parse_all_resilient(&buf);
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0], Response::Cover { closed: false });
        assert_eq!(responses[1], Response::Paper { present: false });
        assert_eq!(consumed, 9);
    }

    #[test]
    fn test_resilient_all_garbage() {
        let buf = [0xff, 0xfe, 0xfd];
        let (responses, consumed) = parse_all_resilient(&buf);
        assert_eq!(responses.len(), 0);
        assert_eq!(consumed, 3);
    }

    #[test]
    fn test_resilient_stops_at_truncated() {
        // Valid response, then truncated 0x1a
        let buf = [0x1a, 0x05, 0x98, 0x1a];
        let (responses, consumed) = parse_all_resilient(&buf);
        assert_eq!(responses.len(), 1);
        assert_eq!(consumed, 3); // stops at the orphan 0x1a
    }

    #[test]
    fn test_resilient_skips_unknown_cmd_header() {
        // Unknown cmd (0x99), then a valid cover response.
        let buf = [0x1a, 0x99, 0xde, 0xad, 0x1a, 0x05, 0x98];
        let (responses, consumed) = parse_all_resilient(&buf);

        assert_eq!(responses, vec![Response::Cover { closed: true }]);
        assert_eq!(consumed, buf.len());
    }

    #[test]
    fn test_resilient_unknown_cmd_before_truncated_known() {
        // Unknown cmd is skipped, then parser stops at truncated known response.
        let buf = [0x1a, 0x99, 0x1a, 0x05];
        let (responses, consumed) = parse_all_resilient(&buf);

        assert!(responses.is_empty());
        assert_eq!(consumed, 2);
    }
}
