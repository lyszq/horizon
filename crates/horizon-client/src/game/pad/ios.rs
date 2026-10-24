//! GameController reads the same PadState consumed by the desktop bindings.
use super::{super::Game, Pads};
use horizon_client::gamepad::{PadState, Pulse, Rumble};
use serde_json::{Value, json};

impl Pads {
    pub(super) fn read(&mut self) -> PadState {
        let state = horizon_ios_platform::controller();
        PadState {
            buttons: state.buttons,
            left: state.left,
            right: state.right,
            triggers: state.triggers,
        }
    }
}
impl Game<'_> {
    pub(super) fn pad_output_off(&mut self) {}
    pub(super) fn pad_output_frame(&mut self) {}
    pub(in crate::game) fn pad_close_output(&mut self) {}
    // Input has no dependency on raw HID. Optional controller haptics are
    // reported separately from gameplay controls; no unsupported HID access.
    pub(super) fn pad_vibrate(&mut self, _: Rumble, _: Pulse) {}
    pub(super) fn pad_devices_status(&self) -> Value {
        let state = horizon_ios_platform::controller();
        json!({"backend":"GameController","connected":state.connected,"raw_hid":false,"haptics":false})
    }
}
