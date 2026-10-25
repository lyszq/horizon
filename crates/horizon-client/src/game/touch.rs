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
    event::{
        DeviceId, ElementState, MouseButton, MouseScrollDelta, TouchPhase as WinitTouchPhase,
        WindowEvent,
    },
    event_loop::ActiveEventLoop,
    keyboard::KeyCode,
};

impl Game<'_> {
    #[cfg(target_os = "ios")]
    pub(super) fn begin_touch_edit(&mut self) {
        self.touch.cancel_all();
        self.keys.clear();
        self.physical_keys.clear();
        self.left = false;
        self.right = false;
        self.toggles = Default::default();
        self.touch_editor = Some(horizon_client::touch::LayoutEditor::new(
            self.touch.layout(),
        ));
        if let Some(window) = &self.window {
            horizon_ios_platform::hide_setup(window.as_ref());
            horizon_ios_platform::hide_accessory(window.as_ref());
            horizon_ios_platform::keyboard(window.as_ref(), false);
        }
    }

    #[cfg(target_os = "ios")]
    fn touch_editor_input(
        &mut self,
        id: u64,
        phase: WinitTouchPhase,
        x: f64,
        y: f64,
    ) -> horizon_client::Result<()> {
        use horizon_client::touch::{EditorCommand, TouchPhase};
        if self.window.is_none() {
            return Ok(());
        }
        let viewport = self.touch_viewport().map(|size| size as f32);
        let full_point = [x as f32 / viewport[0], y as f32 / viewport[1]];
        let safe_area = self.touch_safe_area(viewport);
        if phase == WinitTouchPhase::Started && !safe_area.contains_full_point(full_point) {
            return Ok(());
        }
        let point = safe_area.to_safe_point(full_point);
        let phase = match phase {
            WinitTouchPhase::Ended if !safe_area.contains_full_point(full_point) => {
                TouchPhase::Cancelled
            }
            WinitTouchPhase::Started => TouchPhase::Started,
            WinitTouchPhase::Moved => TouchPhase::Moved,
            WinitTouchPhase::Ended => TouchPhase::Ended,
            WinitTouchPhase::Cancelled => TouchPhase::Cancelled,
        };
        let command = self
            .touch_editor
            .as_mut()
            .and_then(|e| e.handle(id, phase, point));
        if let Some(command) = command {
            if command == EditorCommand::Save {
                let editor = self.touch_editor.as_ref().unwrap();
                let json = editor.draft.to_json()?;
                std::fs::write("horizon-touch.json.tmp", &json)?;
                std::fs::rename("horizon-touch.json.tmp", "horizon-touch.json")?;
                self.touch.set_layout(editor.draft.clone());
                if let Some(sandbox) = std::env::var_os("HOME") {
                    let path =
                        std::path::PathBuf::from(sandbox).join("Documents/horizon-touch.json");
                    std::fs::write(path, json)?;
                }
            }
            self.mobile_reset_pad();
            self.touch_editor = None;
        }
        Ok(())
    }

    fn touch_viewport(&self) -> [u32; 2] {
        self.renderer.as_ref().map_or_else(
            || {
                self.window.as_ref().map_or([1, 1], |window| {
                    let size = window.inner_size();
                    [size.width.max(1), size.height.max(1)]
                })
            },
            |renderer| [renderer.config.width.max(1), renderer.config.height.max(1)],
        )
    }

    #[cfg(target_os = "ios")]
    fn touch_safe_area(&self, viewport: [f32; 2]) -> horizon_client::touch::TouchSafeArea {
        let Some(window) = &self.window else {
            return horizon_client::touch::TouchSafeArea::default();
        };
        let scale = window.scale_factor();
        let insets = horizon_ios_platform::safe_area(window.as_ref());
        horizon_client::touch::TouchSafeArea::from_pixel_insets(
            viewport,
            [
                (insets.left * scale) as f32,
                (insets.top * scale) as f32,
                (insets.right * scale) as f32,
                (insets.bottom * scale) as f32,
            ],
        )
    }

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
        #[cfg(target_os = "ios")]
        if self.touch_editor.is_some() {
            self.touch_editor_input(id, phase, x, y)?;
            return Ok(());
        }
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
        let size = self.touch_viewport();
        let viewport = [size[0] as f32, size[1] as f32];
        let full_position = [(x as f32) / viewport[0], (y as f32) / viewport[1]];
        let mode = if self.pad_in_menu() || self.player.is_none() {
            TouchMode::Menu
        } else {
            TouchMode::Gameplay
        };
        #[cfg(target_os = "ios")]
        let (position, viewport) = if mode == TouchMode::Gameplay {
            let safe_area = self.touch_safe_area(viewport);
            (
                safe_area.to_safe_point(full_position),
                safe_area.safe_viewport(viewport),
            )
        } else {
            (full_position, viewport)
        };
        #[cfg(not(target_os = "ios"))]
        let (position, viewport) = (full_position, viewport);
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
        #[cfg(target_os = "ios")]
        if let Some(editor) = &mut self.touch_editor {
            editor.cancel_all();
        }
        for effect in self.touch.cancel_all() {
            self.apply_touch_effect(event_loop, effect)?;
        }
        Ok(())
    }

    /// Advance stationary menu-finger gestures. The app poll hook should call
    /// this each frame so long presses turn into right clicks without needing
    /// another native touch-move event.
    #[cfg(target_os = "ios")]
    pub(super) fn touch_tick(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> horizon_client::Result<()> {
        for effect in self.touch.advance() {
            self.apply_touch_effect(event_loop, effect)?;
        }
        Ok(())
    }

    /// Complete touch action latches only after the player's tick sampled them.
    #[cfg(target_os = "ios")]
    pub(super) fn touch_player_tick(
        &mut self,
        event_loop: &ActiveEventLoop,
    ) -> horizon_client::Result<()> {
        let effects = self.touch.player_tick_complete();
        for effect in effects {
            self.apply_touch_effect(event_loop, effect)?;
        }
        Ok(())
    }

    /// Whether an active touch still owns the default keyboard mapping.
    #[cfg(target_os = "ios")]
    pub(super) fn touch_holds_default_key(&self, key: KeyCode) -> bool {
        self.touch.enabled()
            && TOUCH_ACTIONS.into_iter().any(|action| {
                self.touch.action_active(action) && touch_action_key(action) == Some(key)
            })
    }

    /// Whether an active touch still owns a default mouse mapping.
    #[cfg(target_os = "ios")]
    pub(super) fn touch_holds_mouse(&self, button: MouseButton) -> bool {
        self.touch.enabled()
            && TOUCH_ACTIONS.into_iter().any(|action| {
                self.touch.action_active(action)
                    && mouse_button(KeyBinds::default_of(&touch_mapping(action))) == Some(button)
            })
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
            TouchEffect::MenuScrolled([x, y]) => {
                self.touch_pointer_scroll(event_loop, [x, y]);
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
        if action == TouchAction::Pause {
            return self.mapped_key(KeyCode::Escape, pressed, false);
        }
        let mapping = touch_mapping(action);
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

    fn touch_pointer_scroll(&mut self, event_loop: &ActiveEventLoop, delta: [f32; 2]) {
        let Some(id) = self.window.as_ref().map(|window| window.id()) else {
            return;
        };
        self.window_event(
            event_loop,
            id,
            WindowEvent::MouseWheel {
                device_id: DeviceId::dummy(),
                delta: MouseScrollDelta::PixelDelta(PhysicalPosition::new(
                    f64::from(delta[0]),
                    f64::from(delta[1]),
                )),
                phase: WinitTouchPhase::Moved,
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
        #[cfg(target_os = "ios")]
        let safe_area = self.touch_safe_area(self.touch_viewport().map(|size| size as f32));
        #[cfg(target_os = "ios")]
        if let Some(editor) = &self.touch_editor {
            horizon_client::touch::draw_layout_editor_in_safe_area(
                editor,
                &self.gui.atlas,
                batch,
                safe_area,
            );
            return;
        }
        if !self.touch.enabled()
            || self.player.is_none()
            || self.pad_in_menu()
            || self.screen != Screen::Playing
        {
            return;
        }
        #[cfg(target_os = "ios")]
        horizon_client::touch::draw_overlay_in_safe_area(
            &self.touch,
            &self.gui.atlas,
            batch,
            self.cursor,
            safe_area,
        );
        #[cfg(not(target_os = "ios"))]
        horizon_client::touch::draw_overlay(&self.touch, &self.gui.atlas, batch, self.cursor);
    }
}

#[cfg(target_os = "ios")]
const TOUCH_ACTIONS: [TouchAction; 17] = [
    TouchAction::Break,
    TouchAction::Place,
    TouchAction::Jump,
    TouchAction::Sneak,
    TouchAction::Sprint,
    TouchAction::Drop,
    TouchAction::Inventory,
    TouchAction::Pause,
    TouchAction::Hotbar(0),
    TouchAction::Hotbar(1),
    TouchAction::Hotbar(2),
    TouchAction::Hotbar(3),
    TouchAction::Hotbar(4),
    TouchAction::Hotbar(5),
    TouchAction::Hotbar(6),
    TouchAction::Hotbar(7),
    TouchAction::Hotbar(8),
];

fn touch_mapping(action: TouchAction) -> String {
    match action {
        TouchAction::Break => "key.attack".to_owned(),
        TouchAction::Place => "key.use".to_owned(),
        TouchAction::Jump => "key.jump".to_owned(),
        TouchAction::Sneak => "key.sneak".to_owned(),
        TouchAction::Sprint => "key.sprint".to_owned(),
        TouchAction::Drop => "key.drop".to_owned(),
        TouchAction::Inventory => "key.inventory".to_owned(),
        TouchAction::Pause => "key.pause".to_owned(),
        TouchAction::Hotbar(slot) => format!("key.hotbar.{}", slot.min(8) + 1),
    }
}

#[cfg(target_os = "ios")]
fn touch_action_key(action: TouchAction) -> Option<KeyCode> {
    match action {
        TouchAction::Pause => Some(KeyCode::Escape),
        _ => key_code(KeyBinds::default_of(&touch_mapping(action))),
    }
}
