//! Native activity signals; readings remain transient and never enter storage.
use std::time::{Duration, Instant};
use windows::Gaming::Input::{GameControllerSwitchPosition, Gamepad, RawGameController};
use windows::Win32::UI::Input::XboxController::{XInputGetState, XINPUT_STATE};

pub(super) fn interactive_desktop() -> bool {
    use windows::Win32::{Foundation::HANDLE, System::StationsAndDesktops::*};
    // SAFETY: the query-only desktop is closed on every path, and Windows
    // receives an initialized, correctly sized UTF-16 buffer.
    unsafe {
        let Ok(desktop) = OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_READOBJECTS)
        else {
            return false;
        };
        let mut name = [0u16; 64];
        let ok = GetUserObjectInformationW(
            HANDLE(desktop.0),
            UOI_NAME,
            Some(name.as_mut_ptr().cast()),
            std::mem::size_of_val(&name) as u32,
            None,
        )
        .is_ok();
        let _ = CloseDesktop(desktop);
        let len = name.iter().position(|c| *c == 0).unwrap_or(name.len());
        ok && String::from_utf16_lossy(&name[..len]).eq_ignore_ascii_case("default")
    }
}

struct RawPad {
    controller: RawGameController,
    buttons: Vec<bool>,
    switches: Vec<GameControllerSwitchPosition>,
    axes: Vec<f64>,
    previous_axes: Option<Vec<f64>>,
}

pub(super) struct Gamepads {
    pads: Vec<Gamepad>,
    raw: Vec<RawPad>,
    refresh_at: Instant,
    previous_xinput: [bool; 4],
    xinput_connected: [bool; 4],
    previous_gamepad: Vec<bool>,
}

impl Gamepads {
    pub fn new() -> Self {
        Self {
            pads: vec![],
            raw: vec![],
            refresh_at: Instant::now(),
            previous_xinput: [false; 4],
            xinput_connected: [true; 4],
            previous_gamepad: vec![],
        }
    }

    fn refresh(&mut self) {
        self.refresh_at = Instant::now() + Duration::from_secs(10);
        // Disconnected slots are expensive on some controller drivers. Probe
        // them only on discovery, then poll connected devices for short presses.
        self.xinput_connected = [true; 4];
        self.pads = Gamepad::Gamepads()
            .ok()
            .map(|pads| pads.into_iter().take(16).collect())
            .unwrap_or_default();
        self.previous_gamepad.resize(self.pads.len(), false);
        self.raw = RawGameController::RawGameControllers()
            .ok()
            .map(|pads| {
                pads.into_iter()
                    .take(16)
                    .filter_map(|controller| {
                        let buttons = usize::try_from(controller.ButtonCount().ok()?).ok()?;
                        let switches = usize::try_from(controller.SwitchCount().ok()?).ok()?;
                        let axes = usize::try_from(controller.AxisCount().ok()?).ok()?;
                        if buttons > 128 || switches > 32 || axes > 32 {
                            return None;
                        }
                        Some(RawPad {
                            controller,
                            buttons: vec![false; buttons],
                            switches: vec![GameControllerSwitchPosition::Center; switches],
                            axes: vec![0.0; axes],
                            previous_axes: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
    }

    pub fn activity(&mut self) -> bool {
        if Instant::now() >= self.refresh_at {
            self.refresh();
        }
        let mut activity = false;
        for index in 0..4 {
            if !self.xinput_connected[index] {
                continue;
            }
            let mut state = XINPUT_STATE::default();
            // SAFETY: XInput writes only this initialized stack structure.
            let connected = unsafe { XInputGetState(index as u32, &mut state) == 0 };
            self.xinput_connected[index] = connected;
            let pad = state.Gamepad;
            let active = connected
                && (pad.wButtons.0 != 0
                    || pad.bLeftTrigger > 30
                    || pad.bRightTrigger > 30
                    || stick_active(pad.sThumbLX, pad.sThumbLY, 7849)
                    || stick_active(pad.sThumbRX, pad.sThumbRY, 8689));
            activity |= active || self.previous_xinput[index] != active;
            self.previous_xinput[index] = active;
        }
        for (index, pad) in self.pads.iter().enumerate() {
            if let Ok(reading) = pad.GetCurrentReading() {
                let active = reading.Buttons.0 != 0
                    || reading.LeftTrigger > 0.12
                    || reading.RightTrigger > 0.12
                    || reading.LeftThumbstickX.hypot(reading.LeftThumbstickY) > 0.24
                    || reading.RightThumbstickX.hypot(reading.RightThumbstickY) > 0.27;
                activity |= active || self.previous_gamepad[index] != active;
                self.previous_gamepad[index] = active;
            }
        }
        for pad in &mut self.raw {
            if pad
                .controller
                .GetCurrentReading(&mut pad.buttons, &mut pad.switches, &mut pad.axes)
                .is_ok()
            {
                activity |= pad.buttons.iter().any(|b| *b)
                    || pad
                        .switches
                        .iter()
                        .any(|s| *s != GameControllerSwitchPosition::Center);
                if let Some(previous) = pad.previous_axes.as_ref() {
                    activity |= pad.axes.iter().zip(previous).any(|(now, old)| {
                        now.is_finite() && old.is_finite() && (now - old).abs() >= 0.06
                    });
                }
                pad.previous_axes = Some(pad.axes.clone());
            }
        }
        activity
    }

    pub fn connected(&self) -> bool {
        self.xinput_connected.iter().any(|connected| *connected)
            || !self.pads.is_empty()
            || !self.raw.is_empty()
    }
}

fn stick_active(x: i16, y: i16, dead_zone: i32) -> bool {
    i64::from(x).pow(2) + i64::from(y).pow(2) > i64::from(dead_zone).pow(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn neutral_stick_noise_is_not_activity() {
        assert!(!stick_active(1400, -900, 7849));
        assert!(stick_active(i16::MIN, i16::MIN, 7849));
        assert!(stick_active(6000, 6000, 7849));
    }
}
