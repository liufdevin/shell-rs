use std::{io, os::windows::io::AsRawHandle as _};

use crate::host::{FlowControl, SerialSettings};
use serial2::{SerialPort, Settings};
use windows_sys::Win32::Devices::Communication::{
    COMSTAT, ClearCommError, GetCommModemStatus, MS_CTS_ON, MS_DSR_ON,
};

pub(super) fn configure(settings: &mut Settings, line: SerialSettings) {
    configure_raw(settings.as_raw_dbc_mut(), line);
}

fn configure_raw(dcb: &mut serial2::os::windows::DCB, line: SerialSettings) {
    let mask = (1 << 2) | (1 << 3) | (3 << 4) | (1 << 6) | (3 << 12) | (1 << 14);
    dcb._bitfield &= !mask;
    dcb._bitfield |= u32::from(line.dtr) << 4;
    if line.flow == FlowControl::RtsCts {
        dcb._bitfield |= (1 << 2) | (2 << 12);
    } else {
        dcb._bitfield |= u32::from(line.rts) << 12;
    }
    dcb.XonChar = 0x11;
    dcb.XoffChar = 0x13;
}

pub(super) fn verify(settings: &Settings, line: SerialSettings) -> bool {
    verify_raw(settings.as_raw_dbc(), line)
}

pub(super) fn same(actual: &Settings, desired: &Settings) -> bool {
    same_raw(actual.as_raw_dbc(), desired.as_raw_dbc())
}

fn same_raw(a: &serial2::os::windows::DCB, b: &serial2::os::windows::DCB) -> bool {
    a.DCBlength == b.DCBlength
        && a.BaudRate == b.BaudRate
        && a._bitfield == b._bitfield
        && a.XonLim == b.XonLim
        && a.XoffLim == b.XoffLim
        && a.ByteSize == b.ByteSize
        && a.Parity == b.Parity
        && a.StopBits == b.StopBits
        && a.XonChar == b.XonChar
        && a.XoffChar == b.XoffChar
        && a.ErrorChar == b.ErrorChar
        && a.EofChar == b.EofChar
        && a.EvtChar == b.EvtChar
}

fn verify_raw(dcb: &serial2::os::windows::DCB, line: SerialSettings) -> bool {
    let rts = if line.flow == FlowControl::RtsCts {
        2
    } else {
        u32::from(line.rts)
    };
    ((dcb._bitfield >> 4) & 3) == u32::from(line.dtr)
        && ((dcb._bitfield >> 12) & 3) == rts
        && ((dcb._bitfield >> 2) & 1) == u32::from(line.flow == FlowControl::RtsCts)
        && (dcb._bitfield & ((1 << 3) | (1 << 6) | (1 << 14))) == 0
        && ((dcb._bitfield >> 8) & 3)
            == if line.flow == FlowControl::XonXoff {
                3
            } else {
                0
            }
        && (line.flow != FlowControl::XonXoff || (dcb.XonChar == 0x11 && dcb.XoffChar == 0x13))
}

pub(super) fn status(port: &SerialPort) -> io::Result<(u32, Option<bool>, Option<bool>)> {
    let mut errors = 0;
    let mut queues = COMSTAT::default();
    let mut modem = 0;
    unsafe {
        if ClearCommError(port.as_raw_handle(), &mut errors, &mut queues) == 0 {
            return Err(io::Error::last_os_error());
        }
        let available = GetCommModemStatus(port.as_raw_handle(), &mut modem) != 0;
        Ok((
            errors,
            available.then_some(modem & MS_CTS_ON != 0),
            available.then_some(modem & MS_DSR_ON != 0),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_clear_inherited_abort_and_dsr_gates_and_use_rts_handshake() {
        for flow in FlowControl::ALL {
            for dtr in [false, true] {
                for rts in [false, true] {
                    let line = SerialSettings {
                        flow,
                        dtr,
                        rts,
                        ..Default::default()
                    };
                    let mut raw = serial2::os::windows::DCB {
                        _bitfield: u32::MAX,
                        ..Default::default()
                    };
                    raw._bitfield &= !(3 << 8);
                    if flow == FlowControl::XonXoff {
                        raw._bitfield |= 3 << 8;
                    }
                    configure_raw(&mut raw, line);
                    assert!(verify_raw(&raw, line));
                    assert_eq!(raw.XonChar, 0x11);
                    assert_eq!(raw.XoffChar, 0x13);
                    let mut ignored = raw;
                    ignored._bitfield ^= 1 << 4;
                    assert!(!verify_raw(&ignored, line));
                    ignored = raw;
                    ignored._bitfield ^= 1 << 12;
                    assert!(!verify_raw(&ignored, line));
                }
            }
        }
    }

    #[test]
    fn identical_driver_configuration_can_be_used_without_set_comm_state() {
        let dcb = serial2::os::windows::DCB {
            DCBlength: 28,
            BaudRate: 115200,
            _bitfield: 0x1011,
            ByteSize: 8,
            StopBits: 0,
            XonChar: 0x11,
            XoffChar: 0x13,
            ..Default::default()
        };
        assert!(same_raw(&dcb, &dcb));
        let mut changed = dcb;
        changed.BaudRate = 9600;
        assert!(!same_raw(&dcb, &changed));
        changed = dcb;
        changed._bitfield ^= 1 << 12;
        assert!(!same_raw(&dcb, &changed));
    }
}
