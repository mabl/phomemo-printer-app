//! Printer status from the printer's answers to the status queries, as
//! `pappl_preason_t` bits.

use std::ffi::c_uint;

use phomemo_protocol::responses::{BatteryStatus, Response};

use crate::pappl::{
    PM_PREASON_COVER_OPEN, PM_PREASON_MARKER_SUPPLY_LOW, PM_PREASON_MEDIA_EMPTY,
    PM_PREASON_OFFLINE, PM_PREASON_OTHER,
};

pub(super) const STATUS_QUERY_ATTEMPTS: usize = 2;
const STATUS_SEEN_COVER: u8 = 0x01;
const STATUS_SEEN_PAPER: u8 = 0x02;
const STATUS_SEEN_TEMP: u8 = 0x04;

#[derive(Debug, Default, Clone, Copy)]
pub struct StatusState {
    pub reasons: c_uint,
    mandatory_seen: u8,
    saw_response: bool,
}

impl StatusState {
    const fn mark_cover(&mut self) {
        self.mandatory_seen |= STATUS_SEEN_COVER;
    }

    const fn mark_paper(&mut self) {
        self.mandatory_seen |= STATUS_SEEN_PAPER;
    }

    const fn mark_temperature(&mut self) {
        self.mandatory_seen |= STATUS_SEEN_TEMP;
    }

    pub const fn completeness_score(self) -> u8 {
        let mut score = 0;
        if self.mandatory_seen & STATUS_SEEN_COVER != 0 {
            score += 1;
        }
        if self.mandatory_seen & STATUS_SEEN_PAPER != 0 {
            score += 1;
        }
        if self.mandatory_seen & STATUS_SEEN_TEMP != 0 {
            score += 1;
        }
        score
    }

    pub const fn is_complete(self) -> bool {
        self.mandatory_seen == (STATUS_SEEN_COVER | STATUS_SEEN_PAPER | STATUS_SEEN_TEMP)
    }
}

pub fn apply_status_response(state: &mut StatusState, response: &Response) -> Option<i32> {
    // An undocumented command byte may be line noise: it is no sign that
    // the printer answered.
    if matches!(response, Response::Unknown { .. }) {
        return None;
    }
    state.saw_response = true;
    match response {
        Response::Cover { closed } => {
            state.mark_cover();
            if !closed {
                state.reasons |= PM_PREASON_COVER_OPEN;
            }
            None
        }
        Response::Paper { present } => {
            state.mark_paper();
            if !present {
                state.reasons |= PM_PREASON_MEDIA_EMPTY;
            }
            None
        }
        Response::Temperature { overheated } => {
            state.mark_temperature();
            if *overheated {
                state.reasons |= PM_PREASON_OTHER;
            }
            None
        }
        Response::Battery(BatteryStatus::Level(pct)) => {
            if *pct <= 10 {
                state.reasons |= PM_PREASON_MARKER_SUPPLY_LOW;
            }
            Some(i32::from(*pct))
        }
        Response::Battery(BatteryStatus::LowAlarm(_)) => {
            state.reasons |= PM_PREASON_MARKER_SUPPLY_LOW;
            None
        }
        Response::WorkStatus { idle } => {
            if !idle {
                state.reasons |= PM_PREASON_OTHER;
            }
            None
        }
        Response::PrintBusy { busy } => {
            if *busy {
                state.reasons |= PM_PREASON_OTHER;
            }
            None
        }
        Response::PrintCancel => {
            state.reasons |= PM_PREASON_OTHER;
            None
        }
        _ => None,
    }
}

pub const fn finalize_status_state(state: StatusState, had_transport_error: bool) -> c_uint {
    if state.is_complete() {
        return state.reasons;
    }

    if !state.saw_response || had_transport_error {
        return state.reasons | PM_PREASON_OFFLINE;
    }

    // Partial status can happen during transient BT timing jitter.
    // Keep printer online but flag state as degraded.
    state.reasons | PM_PREASON_OTHER
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_finalize_marks_offline_when_no_data() {
        let state = StatusState::default();
        let reasons = finalize_status_state(state, false);
        assert_eq!(reasons & PM_PREASON_OFFLINE, PM_PREASON_OFFLINE);
    }

    #[test]
    fn status_finalize_keeps_partial_online_without_transport_error() {
        let state = StatusState {
            mandatory_seen: STATUS_SEEN_COVER,
            saw_response: true,
            ..StatusState::default()
        };
        let reasons = finalize_status_state(state, false);
        assert_eq!(reasons & PM_PREASON_OFFLINE, 0);
        assert_eq!(reasons & PM_PREASON_OTHER, PM_PREASON_OTHER);
    }

    #[test]
    fn status_ignores_undocumented_frames() {
        let mut state = StatusState::default();
        assert_eq!(
            apply_status_response(&mut state, &Response::Unknown { cmd: 0x99 }),
            None
        );
        assert!(!state.saw_response);
        let reasons = finalize_status_state(state, false);
        assert_eq!(reasons & PM_PREASON_OFFLINE, PM_PREASON_OFFLINE);
    }

    #[test]
    fn status_apply_response_maps_core_reasons() {
        let mut state = StatusState::default();
        let battery =
            apply_status_response(&mut state, &Response::Battery(BatteryStatus::Level(7)));
        assert_eq!(battery, Some(7));
        assert_eq!(
            state.reasons & PM_PREASON_MARKER_SUPPLY_LOW,
            PM_PREASON_MARKER_SUPPLY_LOW
        );

        let _ = apply_status_response(&mut state, &Response::Cover { closed: false });
        let _ = apply_status_response(&mut state, &Response::Paper { present: false });
        let _ = apply_status_response(&mut state, &Response::Temperature { overheated: true });

        assert!(state.is_complete());
        assert_eq!(state.reasons & PM_PREASON_COVER_OPEN, PM_PREASON_COVER_OPEN);
        assert_eq!(
            state.reasons & PM_PREASON_MEDIA_EMPTY,
            PM_PREASON_MEDIA_EMPTY
        );
        assert_eq!(state.reasons & PM_PREASON_OTHER, PM_PREASON_OTHER);
    }
}
