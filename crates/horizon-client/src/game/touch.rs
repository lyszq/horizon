//! iPad/multitouch adapter for the platform-independent touch state machine.
use super::{Game, Screen};
use horizon_client::{
    hud::HudBatch,
    keybinds::{KeyBinds, key_code, mouse_button},
    touch::{TouchAction, TouchEffect, TouchEvent, TouchMode, TouchMouseButton, TouchPhase},
};
use winit::{
    application::ApplicationHandler,
    dpi::PhysicalPosition,
    event::{DeviceId, ElementState, MouseButton, TouchPhase as WinitTouchPhase, WindowEvent},
    event_loop::ActiveEventLoop,
    keyboard::KeyCode,
};

impl Game<'_> {
    /// Feed one native touch sample to the controls. The event hook passes the
    /// platform's physical position; capture and layout stay normalized.
    pub(super) fn touch_input(
        &mut self,
        event_loop: &ActiveEventLoop,
        id: u64,
        phase: WinitTouchPhase,
        x: f64,
        y: f64,
    ) -> horizon_client::Result<()> {
        if !self.touch.enabled() {
            return Ok(());
        }
        if !self.focused {
            let effects = self.touch.cancel_all();
            for effect in effects {
                self.apply_touch_effect(event_loop, effect)?;
            }
            return Ok(());
        }
        let size = self.renderer.as_ref().map_or_else(
            || {
                self.window.as_ref().map_or([1, 1], |window| {
                    let size = window.inner_size();
                    [size.width.max(1), size.height.max(1)]
                })
            },
            |renderer| [renderer.config.width.max(1), renderer.config.height.max(1)],
        );
        let viewport = [size[0] as f32, size[1] as f32];
        let position = [(x as f32) / viewport[0], (y as f32) / viewport[1]];
        let mode = if self.pad_in_menu() || self.player.is_none() {
            TouchMode::Menu
        } else {
            TouchMode::Gameplay
        };
        let phase = match phase {
            WinitTouchPhase::Started => TouchPhase::Started,
            WinitTouchPhase::Moved => TouchPhase::Moved,
            WinitTouchPhase::Ended => TouchPhase::Ended,
            WinitTouchPhase::Cancelled => TouchPhase::Cancelled,
        };
        self.last_input = std::time::Instant::now();
        let effects = self.touch.handle(TouchEvent {
            id,
            phase,
            position,
            viewport,
            mode,
        });
        for effect in effects {
            self.apply_touch_effect(event_loop, effect)?;
        }
        Ok(())
    }

    /// Release touches on focus loss, app suspension or a scene teardown.
    pub(super) fn cancel_touch_input(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> horizon_client::Result<()> {
        for effect in self.touch.cancel_all() {
            self.apply_touch_effect(event_loop, effect)?;
        }
        Ok(())
    }

    /// Advance stationary menu-finger gestures. The app poll hook should call
    /// this each frame so long presses turn into right clicks without needing
    /// another native touch-move event.
    pub(super) fn touch_tick(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> horizon_client::Result<()> {
        for effect in self.touch.advance() {
            self.apply_touch_effect(event_loop, effect)?;
        }
        Ok(())
    }

    fn apply_touch_effect(
        &mut self,
        event_loop: &ActiveEventLoop,
        effect: TouchEffect,
    ) -> horizon_client::Result<()> {
        match effect {
            // The analog vector remains in TouchControls and is read beside
            // `pad_move()` when the game builds its player movement keys.
            TouchEffect::MovementChanged(_) => {}
            TouchEffect::LookDelta([dx, dy]) => {
                if self.screen == Screen::Playing && self.player.is_some() {
                    // Reuse MouseHandler's sensitivity, inversion, smoothing
                    // and zoom path. A temporary agent-pointer flag lets that
                    // existing path accept touch motion without grabbing a
                    // desktop cursor.
                    let was_agent_pointer = self.agent_pointer;
                    self.agent_pointer = true;
                    self.mouse.add_motion(f64::from(dx), f64::from(dy));
                    self.apply_mouse_look();
                    self.agent_pointer = was_agent_pointer;
                }
            }
            TouchEffect::ActionChanged { action, pressed } => {
                self.touch_action(action, pressed)?;
            }
            TouchEffect::MenuPointerMoved(position) => {
                self.touch_pointer_to(event_loop, position);
            }
            TouchEffect::MenuMouseButton {
                button,
                pressed,
                cancelled,
            } => {
                if cancelled && !pressed {
                    self.cancel_touch_menu_drag();
                }
                self.touch_pointer_button(event_loop, button, pressed);
            }
        }
        Ok(())
    }

    fn touch_action(&mut self, action: TouchAction, pressed: bool) -> horizon_client::Result<()> {
        let mapping = match action {
            TouchAction::Break => "key.attack".to_owned(),
            TouchAction::Place => "key.use".to_owned(),
            TouchAction::Jump => "key.jump".to_owned(),
            TouchAction::Sneak => "key.sneak".to_owned(),
            TouchAction::Sprint => "key.sprint".to_owned(),
            TouchAction::Drop => "key.drop".to_owned(),
            TouchAction::Inventory => "key.inventory".to_owned(),
            TouchAction::Pause => return self.mapped_key(KeyCode::Escape, pressed, false),
            TouchAction::Hotbar(slot) => format!("key.hotbar.{}", slot.min(8) + 1),
        };
        let default = KeyBinds::default_of(&mapping);
        if let Some(button) = mouse_button(default) {
            if self.screen == Screen::Playing && self.title.is_none() {
                self.play_mouse(button, pressed)?;
            }
        } else if let Some(key) = key_code(default) {
            self.mapped_key(key, pressed, false)?;
        }
        Ok(())
    }

    fn touch_pointer_to(&mut self, event_loop: &ActiveEventLoop, point: [f32; 2]) {
        let (Some(window), Some(size)) = (
            self.window.as_ref(),
            self.renderer
                .as_ref()
                .map(|renderer| [renderer.config.width, renderer.config.height]),
        ) else {
            return;
        };
        let id = window.id();
        let batch = HudBatch::new(size);
        let x = (point[0].clamp(0.0, 1.0) * batch.width).clamp(0.0, batch.width - 1.0);
        let y = (point[1].clamp(0.0, 1.0) * batch.height).clamp(0.0, batch.height - 1.0);
        let position =
            PhysicalPosition::new(f64::from(x * batch.scale), f64::from(y * batch.scale));
        self.window_event(
            event_loop,
            id,
            WindowEvent::CursorMoved {
                device_id: DeviceId::dummy(),
                position,
            },
        );
    }

    fn touch_pointer_button(
        &mut self,
        event_loop: &ActiveEventLoop,
        button: TouchMouseButton,
        pressed: bool,
    ) {
        let Some(id) = self.window.as_ref().map(|window| window.id()) else {
            return;
        };
        let button = match button {
            TouchMouseButton::Left => MouseButton::Left,
            TouchMouseButton::Right => MouseButton::Right,
        };
        self.window_event(
            event_loop,
            id,
            WindowEvent::MouseInput {
                device_id: DeviceId::dummy(),
                state: if pressed {
                    ElementState::Pressed
                } else {
                    ElementState::Released
                },
                button,
            },
        );
    }

    /// A system cancellation must release the synthetic mouse without
    /// committing a half-finished inventory quick-craft or scrollbar drag.
    fn cancel_touch_menu_drag(&mut self) {
        self.inv_drag = None;
        self.inv_clicks = Default::default();
        self.inv_clicks.skip_release();
        self.last_inv_click = None;
        self.tweak_release();
        self.loom_release();
        self.creative_screen.release();
        self.dragging = false;
        if let Some(title) = &mut self.title {
            title.release();
            title.release_field();
        }
    }

    /// Draw the touch controls over gameplay. Menu touches use the normal
    /// cursor and mouse handlers, so this overlay stays out of vanilla screens.
    pub(super) fn draw_touch_overlay(&self, batch: &mut HudBatch) {
        if !self.touch.enabled()
            || self.player.is_none()
            || self.pad_in_menu()
            || self.screen != Screen::Playing
        {
            return;
        }
        horizon_client::touch::draw_overlay(&self.touch, &self.gui.atlas, batch, self.cursor);
    }
}
