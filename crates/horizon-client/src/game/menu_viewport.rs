//! Keep menu layout and hit testing in the same visible keyboard viewport.
use super::{Game, Loading};
use horizon_client::hud::HudBatch;
use winit::window::Window;

pub(super) fn batch(window: Option<&Window>, size: [u32; 2]) -> HudBatch {
    #[cfg(target_os = "ios")]
    let size = window.map_or(size, |window| {
        horizon_ios_platform::keyboard_viewport(window, size)
    });
    #[cfg(not(target_os = "ios"))]
    let _ = window;
    // Only GUI layout changes. The renderer retains its full native drawable,
    // and HudRenderer projects these vertices using the full physical viewport.
    HudBatch::new(size)
}

impl Game<'_> {
    pub(super) fn ui_batch(&self, size: [u32; 2]) -> HudBatch {
        batch(self.window.as_deref(), size)
    }
}
impl Loading {
    pub(super) fn ui_batch(&self, size: [u32; 2]) -> HudBatch {
        batch(self.window.as_deref(), size)
    }
}
