//! Platform-independent multitouch input state for the game client.
//!
//! Touch positions and saved control bounds are normalized to the full view,
//! so the layout survives window and orientation changes. Each finger keeps
//! the control it first touched until it ends or is cancelled.
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::time::{Duration, Instant};

use crate::{
    hud::{HudAtlas, HudBatch, rgba},
    ui_theme::{self, Face, Ui},
};

pub const TOUCH_LAYOUT_VERSION: u8 = 1;
const STICK_DEADZONE: f32 = 0.12;
const MENU_LONG_PRESS: Duration = Duration::from_millis(420);
const MENU_DRAG_THRESHOLD_PX: f32 = 10.0;
const HOTBAR_LABELS: [&str; 9] = ["1", "2", "3", "4", "5", "6", "7", "8", "9"];

/// A control area expressed as fractions of the drawable view.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NormalizedRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl NormalizedRect {
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    fn sanitize(&mut self) {
        self.width = finite_or(self.width, 0.1).clamp(0.035, 1.0);
        self.height = finite_or(self.height, 0.1).clamp(0.035, 1.0);
        self.x = finite_or(self.x, 0.0).clamp(0.0, 1.0 - self.width);
        self.y = finite_or(self.y, 0.0).clamp(0.0, 1.0 - self.height);
    }

    fn contains(self, point: [f32; 2]) -> bool {
        point[0] >= self.x
            && point[0] < self.x + self.width
            && point[1] >= self.y
            && point[1] < self.y + self.height
    }
}

/// Controls that can own a finger. The look area captures a swipe but has no
/// visible button; the action controls are drawn by the game overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", content = "slot", rename_all = "snake_case")]
pub enum TouchControl {
    MoveStick,
    Look,
    Break,
    Place,
    Jump,
    Sneak,
    Sprint,
    Drop,
    Inventory,
    Pause,
    /// Hotbar slot, zero-based (0 through 8).
    Hotbar(u8),
}

impl TouchControl {
    pub const fn action(self) -> Option<TouchAction> {
        match self {
            Self::Break => Some(TouchAction::Break),
            Self::Place => Some(TouchAction::Place),
            Self::Jump => Some(TouchAction::Jump),
            Self::Sneak => Some(TouchAction::Sneak),
            Self::Sprint => Some(TouchAction::Sprint),
            Self::Drop => Some(TouchAction::Drop),
            Self::Inventory => Some(TouchAction::Inventory),
            Self::Pause => Some(TouchAction::Pause),
            Self::Hotbar(slot) => Some(TouchAction::Hotbar(if slot > 8 { 8 } else { slot })),
            Self::MoveStick | Self::Look => None,
        }
    }
}

/// A touch control and its normalized hit rectangle.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TouchRegion {
    pub control: TouchControl,
    pub rect: NormalizedRect,
}

/// A versioned, serde-ready touch layout. Store `to_json()` beside the other
/// Horizon preferences and load it with `from_json()`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TouchLayout {
    pub version: u8,
    pub look_sensitivity: f32,
    pub stick_deadzone: f32,
    pub controls: Vec<TouchRegion>,
}

impl Default for TouchLayout {
    fn default() -> Self {
        let mut controls = vec![
            TouchRegion {
                control: TouchControl::MoveStick,
                rect: NormalizedRect::new(0.025, 0.53, 0.30, 0.43),
            },
            TouchRegion {
                control: TouchControl::Look,
                rect: NormalizedRect::new(0.34, 0.14, 0.65, 0.82),
            },
            TouchRegion {
                control: TouchControl::Break,
                rect: NormalizedRect::new(0.82, 0.55, 0.105, 0.13),
            },
            TouchRegion {
                control: TouchControl::Place,
                rect: NormalizedRect::new(0.68, 0.71, 0.105, 0.13),
            },
            TouchRegion {
                control: TouchControl::Jump,
                rect: NormalizedRect::new(0.83, 0.79, 0.105, 0.13),
            },
            TouchRegion {
                control: TouchControl::Sneak,
                rect: NormalizedRect::new(0.68, 0.55, 0.105, 0.12),
            },
            TouchRegion {
                control: TouchControl::Sprint,
                rect: NormalizedRect::new(0.30, 0.78, 0.10, 0.12),
            },
            TouchRegion {
                control: TouchControl::Drop,
                rect: NormalizedRect::new(0.935, 0.43, 0.06, 0.09),
            },
            TouchRegion {
                control: TouchControl::Inventory,
                rect: NormalizedRect::new(0.025, 0.045, 0.09, 0.09),
            },
            TouchRegion {
                control: TouchControl::Pause,
                rect: NormalizedRect::new(0.885, 0.045, 0.09, 0.09),
            },
        ];
        for slot in 0..9 {
            controls.push(TouchRegion {
                control: TouchControl::Hotbar(slot),
                rect: NormalizedRect::new(0.305 + slot as f32 * 0.044, 0.93, 0.039, 0.065),
            });
        }
        Self {
            version: TOUCH_LAYOUT_VERSION,
            look_sensitivity: 0.45,
            stick_deadzone: STICK_DEADZONE,
            controls,
        }
    }
}

impl TouchLayout {
    /// Serialize the current normalized control layout.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut layout = self.clone();
        layout.sanitize();
        serde_json::to_string_pretty(&layout)
    }

    /// Read a saved layout and clamp every rectangle to the normalized view.
    pub fn from_json(value: &str) -> Result<Self, String> {
        let mut layout: Self = serde_json::from_str(value).map_err(|error| error.to_string())?;
        if layout.version != TOUCH_LAYOUT_VERSION {
            return Err(format!(
                "unsupported touch layout version {} (expected {})",
                layout.version, TOUCH_LAYOUT_VERSION
            ));
        }
        layout.sanitize();
        Ok(layout)
    }

    /// Clamp programmatically edited or deserialized values before use.
    pub fn sanitize(&mut self) {
        if self.version != TOUCH_LAYOUT_VERSION {
            self.version = TOUCH_LAYOUT_VERSION;
        }
        self.look_sensitivity = finite_or(self.look_sensitivity, 0.45).clamp(0.05, 2.0);
        self.stick_deadzone = finite_or(self.stick_deadzone, STICK_DEADZONE).clamp(0.0, 0.9);
        for region in &mut self.controls {
            region.rect.sanitize();
            if let TouchControl::Hotbar(slot) = &mut region.control {
                *slot = (*slot).min(8);
            }
        }
    }
}

/// Gameplay action generated by a touch button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TouchAction {
    Break,
    Place,
    Jump,
    Sneak,
    Sprint,
    Drop,
    Inventory,
    Pause,
    Hotbar(u8),
}

/// Which screen owns new fingers. Existing fingers retain their original
/// capture until their mode changes or the platform cancels them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchMode {
    Gameplay,
    Menu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchPhase {
    Started,
    Moved,
    Ended,
    Cancelled,
}

/// A platform-neutral touch sample. `position` is normalized to the view;
/// `viewport` is in physical pixels for aspect-correct stick and look deltas.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TouchEvent {
    pub id: u64,
    pub phase: TouchPhase,
    pub position: [f32; 2],
    pub viewport: [f32; 2],
    pub mode: TouchMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchMouseButton {
    Left,
    Right,
}

/// Effects for the game adapter to apply using its existing key, mouse and
/// window-event routes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchEffect {
    MovementChanged([f32; 2]),
    /// Delta in physical pixels after the saved touch look multiplier.
    LookDelta([f32; 2]),
    ActionChanged {
        action: TouchAction,
        pressed: bool,
    },
    MenuPointerMoved([f32; 2]),
    /// Winit-style wheel delta in physical pixels, opposite to finger motion.
    MenuScrolled([f32; 2]),
    MenuMouseButton {
        button: TouchMouseButton,
        pressed: bool,
        cancelled: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Capture {
    MoveStick,
    Look,
    Action(TouchAction),
    MenuPointer,
    Ignored,
}

#[derive(Clone, Copy, Debug)]
struct Finger {
    capture: Capture,
    mode: TouchMode,
    anchor: [f32; 2],
    last: [f32; 2],
    started_at: Instant,
    menu_button: Option<TouchMouseButton>,
    menu_scroll: bool,
}

/// Active touch state. The first finger in a control owns it until up/cancel;
/// action references are counted so one finger cannot release another's key.
#[derive(Debug)]
pub struct TouchControls {
    layout: TouchLayout,
    enabled: bool,
    viewport: [f32; 2],
    fingers: BTreeMap<u64, Finger>,
    action_holds: HashMap<TouchAction, usize>,
    action_latches: HashSet<TouchAction>,
    actions_sampled_by_player: HashSet<TouchAction>,
    movement: [f32; 2],
    move_finger: Option<u64>,
    menu_finger: Option<u64>,
    mode: Option<TouchMode>,
}

/// Draw the gameplay touch overlay from the client library, where the themed
/// UI primitives are intentionally crate-private. The binary adapter only
/// supplies its current atlas, framebuffer batch and cursor position.
pub fn draw_overlay(
    controls: &TouchControls,
    atlas: &HudAtlas,
    batch: &mut HudBatch,
    cursor: (f32, f32),
) {
    if !controls.enabled() {
        return;
    }
    let mut ui = Ui::new(atlas, batch, cursor);
    for region in &controls.layout().controls {
        let rect = region.rect;
        let (x, y, w, h) = (
            rect.x * ui.w,
            rect.y * ui.h,
            rect.width * ui.w,
            rect.height * ui.h,
        );
        match region.control {
            TouchControl::MoveStick => {
                let radius = (w.min(h) * 0.39).max(18.0);
                let (center, knob) = controls.stick_visual().unwrap_or_else(|| {
                    let center = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
                    (center, center)
                });
                let center = (center[0] * ui.w, center[1] * ui.h);
                let knob = (knob[0] * ui.w, knob[1] * ui.h);
                ui.rounded(
                    center.0 - radius,
                    center.1 - radius,
                    radius * 2.0,
                    radius * 2.0,
                    radius,
                    rgba(13, 14, 19, 92),
                );
                ui.border(
                    center.0 - radius,
                    center.1 - radius,
                    radius * 2.0,
                    radius * 2.0,
                    radius,
                    rgba(236, 237, 243, 112),
                );
                let knob_radius = radius * 0.38;
                ui.rounded(
                    knob.0 - knob_radius,
                    knob.1 - knob_radius,
                    knob_radius * 2.0,
                    knob_radius * 2.0,
                    knob_radius,
                    rgba(236, 237, 243, 96),
                );
            }
            TouchControl::Look => {}
            control => {
                let action = control.action();
                let pressed = action.is_some_and(|action| controls.action_active(action));
                let fill = if pressed {
                    ui_theme::color::ACCENT
                } else {
                    rgba(13, 14, 19, 164)
                };
                let text = if pressed {
                    ui_theme::color::ON_ACCENT
                } else {
                    ui_theme::color::TEXT
                };
                let radius = h.min(w) / 2.0;
                ui.rounded(x, y, w, h, radius, fill);
                ui.border(x, y, w, h, radius, rgba(236, 237, 243, 116));
                let label = match control {
                    TouchControl::Break => "Break",
                    TouchControl::Place => "Place",
                    TouchControl::Jump => "Jump",
                    TouchControl::Sneak => "Sneak",
                    TouchControl::Sprint => "Sprint",
                    TouchControl::Drop => "Drop",
                    TouchControl::Inventory => "Inv",
                    TouchControl::Pause => "Menu",
                    TouchControl::Hotbar(slot) => HOTBAR_LABELS[slot.min(8) as usize],
                    TouchControl::MoveStick | TouchControl::Look => continue,
                };
                ui.text_centered(
                    Face::Medium,
                    ui_theme::BODY,
                    x + w / 2.0,
                    y + (h - ui_theme::BODY * 1.45) / 2.0,
                    text,
                    label,
                );
            }
        }
    }
    ui.finish();
}

impl Default for TouchControls {
    fn default() -> Self {
        Self {
            layout: TouchLayout::default(),
            enabled: cfg!(target_os = "ios"),
            viewport: [1.0, 1.0],
            fingers: BTreeMap::new(),
            action_holds: HashMap::new(),
            action_latches: HashSet::new(),
            actions_sampled_by_player: HashSet::new(),
            movement: [0.0, 0.0],
            move_finger: None,
            menu_finger: None,
            mode: None,
        }
    }
}

impl TouchControls {
    pub fn layout(&self) -> &TouchLayout {
        &self.layout
    }

    pub fn set_layout(&mut self, mut layout: TouchLayout) -> Vec<TouchEffect> {
        layout.sanitize();
        let effects = self.cancel_all();
        self.layout = layout;
        effects
    }

    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn set_enabled(&mut self, enabled: bool) -> Vec<TouchEffect> {
        self.enabled = enabled;
        if !enabled {
            return self.cancel_all();
        }
        Vec::new()
    }

    /// The current analog stick vector, ready for `Keys::with_stick`.
    pub fn movement_vector(&self) -> [f32; 2] {
        self.movement
    }

    pub fn action_active(&self, action: TouchAction) -> bool {
        self.action_holds
            .get(&action)
            .is_some_and(|count| *count > 0)
            || self.action_latches.contains(&action)
    }

    /// Finish one player tick after it has sampled the current touch actions.
    /// A short Jump tap stays active through that tick, then releases here.
    pub fn player_tick_complete(&mut self) -> Vec<TouchEffect> {
        self.actions_sampled_by_player
            .extend(self.action_holds.keys().copied());
        let latches = std::mem::take(&mut self.action_latches);
        let mut effects = Vec::new();
        for action in latches {
            if !self.action_active(action) {
                self.actions_sampled_by_player.remove(&action);
                effects.push(TouchEffect::ActionChanged {
                    action,
                    pressed: false,
                });
            }
        }
        effects
    }

    pub fn control_active(&self, control: TouchControl) -> bool {
        self.fingers
            .values()
            .any(|finger| match (control, finger.capture) {
                (TouchControl::MoveStick, Capture::MoveStick)
                | (TouchControl::Look, Capture::Look) => true,
                (control, Capture::Action(action)) => control.action() == Some(action),
                _ => false,
            })
    }

    /// Current normalized joystick anchor and knob positions, if held.
    pub fn stick_visual(&self) -> Option<([f32; 2], [f32; 2])> {
        let finger = self.move_finger.and_then(|id| self.fingers.get(&id))?;
        Some((finger.anchor, finger.last))
    }

    /// Route a platform touch sample through per-finger capture.
    pub fn handle(&mut self, event: TouchEvent) -> Vec<TouchEffect> {
        if !valid_viewport(event.viewport) && event.phase != TouchPhase::Cancelled {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if valid_viewport(event.viewport) {
            self.viewport = event.viewport;
        }
        let fallback = self
            .fingers
            .get(&event.id)
            .map_or([0.0, 0.0], |finger| finger.last);
        let point = clean_point(event.position, fallback);
        if event.phase == TouchPhase::Started {
            effects.extend(self.advance_at(Instant::now()));
        }
        self.change_mode(event.mode, &mut effects);

        match event.phase {
            TouchPhase::Started => {
                if let Some(old) = self.fingers.remove(&event.id) {
                    self.finish_finger(event.id, old, true, &mut effects);
                }
                self.start_finger(event.id, point, event.mode, &mut effects);
            }
            TouchPhase::Moved => {
                self.move_finger_sample(event.id, point, &mut effects);
                effects.extend(self.advance_at(Instant::now()));
            }
            TouchPhase::Ended => {
                if let Some(mut finger) = self.fingers.remove(&event.id) {
                    self.update_finger(event.id, &mut finger, point, &mut effects);
                    self.fingers.insert(event.id, finger);
                    effects.extend(self.advance_at(Instant::now()));
                }
                if let Some(finger) = self.fingers.remove(&event.id) {
                    self.finish_finger(event.id, finger, false, &mut effects);
                }
            }
            TouchPhase::Cancelled => {
                if let Some(finger) = self.fingers.remove(&event.id) {
                    self.finish_finger(event.id, finger, true, &mut effects);
                }
            }
        }
        effects
    }

    /// Advance time-based gestures. Call this from the frame/poll loop while
    /// touches are active so a stationary menu press can become a right-click.
    pub fn advance(&mut self) -> Vec<TouchEffect> {
        self.advance_at(Instant::now())
    }

    /// Deterministic time advancement for gesture tests and embedders that
    /// already have a monotonic frame timestamp.
    pub fn advance_at(&mut self, now: Instant) -> Vec<TouchEffect> {
        let Some(id) = self.menu_finger else {
            return Vec::new();
        };
        let Some(finger) = self.fingers.get_mut(&id) else {
            return Vec::new();
        };
        if finger.capture != Capture::MenuPointer
            || finger.menu_button.is_some()
            || finger.menu_scroll
            || now.saturating_duration_since(finger.started_at) < MENU_LONG_PRESS
        {
            return Vec::new();
        }
        finger.menu_button = Some(TouchMouseButton::Right);
        vec![
            TouchEffect::MenuPointerMoved(clamp_point(finger.anchor)),
            TouchEffect::MenuMouseButton {
                button: TouchMouseButton::Right,
                pressed: true,
                cancelled: false,
            },
        ]
    }

    /// Release all active keys/buttons and clear every capture. Call on focus
    /// loss, app suspension or a platform-level cancellation.
    pub fn cancel_all(&mut self) -> Vec<TouchEffect> {
        let active: HashSet<_> = self
            .action_holds
            .keys()
            .copied()
            .chain(self.action_latches.iter().copied())
            .collect();
        let mut effects = Vec::new();
        let fingers = std::mem::take(&mut self.fingers);
        for (id, finger) in fingers {
            self.finish_finger(id, finger, true, &mut effects);
        }
        self.action_latches.clear();
        self.action_holds.clear();
        self.actions_sampled_by_player.clear();
        for action in active {
            if !effects.iter().any(|effect| {
                matches!(effect, TouchEffect::ActionChanged { action: released, pressed: false } if *released == action)
            }) {
                effects.push(TouchEffect::ActionChanged {
                    action,
                    pressed: false,
                });
            }
        }
        self.move_finger = None;
        self.menu_finger = None;
        self.mode = None;
        if self.movement != [0.0, 0.0] {
            self.movement = [0.0, 0.0];
            effects.push(TouchEffect::MovementChanged(self.movement));
        }
        effects
    }

    fn change_mode(&mut self, mode: TouchMode, effects: &mut Vec<TouchEffect>) {
        if self.mode == Some(mode) {
            return;
        }
        let stale: Vec<_> = self
            .fingers
            .iter()
            .filter_map(|(&id, finger)| (finger.mode != mode).then_some(id))
            .collect();
        for id in stale {
            if let Some(finger) = self.fingers.remove(&id) {
                self.finish_finger(id, finger, true, effects);
            }
        }
        effects.extend(self.player_tick_complete());
        self.mode = Some(mode);
    }

    fn start_finger(
        &mut self,
        id: u64,
        point: [f32; 2],
        mode: TouchMode,
        effects: &mut Vec<TouchEffect>,
    ) {
        let capture = match mode {
            TouchMode::Menu if self.menu_finger.is_none() => Capture::MenuPointer,
            TouchMode::Menu => Capture::Ignored,
            TouchMode::Gameplay => self.capture_gameplay(point),
        };
        let finger = Finger {
            capture,
            mode,
            anchor: point,
            last: point,
            started_at: Instant::now(),
            menu_button: None,
            menu_scroll: false,
        };
        match capture {
            Capture::MoveStick => {
                self.move_finger = Some(id);
                self.update_movement(id, &finger, effects);
            }
            Capture::Action(action) => self.press_action(action, effects),
            Capture::MenuPointer => {
                self.menu_finger = Some(id);
            }
            Capture::Look | Capture::Ignored => {}
        }
        self.fingers.insert(id, finger);
    }

    fn capture_gameplay(&self, point: [f32; 2]) -> Capture {
        // Action buttons override the larger look and stick hit areas.
        for region in &self.layout.controls {
            if region.control.action().is_some() && region.rect.contains(point) {
                return Capture::Action(region.control.action().expect("checked action"));
            }
        }
        if self.move_finger.is_none()
            && self.layout.controls.iter().any(|region| {
                region.control == TouchControl::MoveStick && region.rect.contains(point)
            })
        {
            return Capture::MoveStick;
        }
        if self
            .layout
            .controls
            .iter()
            .any(|region| region.control == TouchControl::Look && region.rect.contains(point))
        {
            return Capture::Look;
        }
        Capture::Ignored
    }

    fn move_finger_sample(&mut self, id: u64, point: [f32; 2], effects: &mut Vec<TouchEffect>) {
        let Some(mut finger) = self.fingers.remove(&id) else {
            return;
        };
        self.update_finger(id, &mut finger, point, effects);
        self.fingers.insert(id, finger);
    }

    fn update_finger(
        &mut self,
        id: u64,
        finger: &mut Finger,
        point: [f32; 2],
        effects: &mut Vec<TouchEffect>,
    ) {
        match finger.capture {
            Capture::MoveStick => self.update_movement(
                id,
                &Finger {
                    last: point,
                    ..*finger
                },
                effects,
            ),
            Capture::Look => {
                let dx =
                    (point[0] - finger.last[0]) * self.viewport[0] * self.layout.look_sensitivity;
                let dy =
                    (point[1] - finger.last[1]) * self.viewport[1] * self.layout.look_sensitivity;
                if dx != 0.0 || dy != 0.0 {
                    effects.push(TouchEffect::LookDelta([dx, dy]));
                }
            }
            Capture::MenuPointer if self.menu_finger == Some(id) => match finger.menu_button {
                Some(_) => {
                    effects.push(TouchEffect::MenuPointerMoved(clamp_point(point)));
                }
                None if finger.menu_scroll => {
                    effects.push(TouchEffect::MenuPointerMoved(clamp_point(point)));
                    let delta = [
                        (finger.last[0] - point[0]) * self.viewport[0],
                        (finger.last[1] - point[1]) * self.viewport[1],
                    ];
                    if delta != [0.0, 0.0] {
                        effects.push(TouchEffect::MenuScrolled(delta));
                    }
                }
                None if self.menu_drag_started(finger.anchor, point) => {
                    let dx = (point[0] - finger.anchor[0]) * self.viewport[0];
                    let dy = (point[1] - finger.anchor[1]) * self.viewport[1];
                    if dy.abs() > dx.abs() {
                        finger.menu_scroll = true;
                        effects.push(TouchEffect::MenuPointerMoved(clamp_point(point)));
                        effects.push(TouchEffect::MenuScrolled([-dx, -dy]));
                    } else {
                        effects.push(TouchEffect::MenuPointerMoved(clamp_point(finger.anchor)));
                        effects.push(TouchEffect::MenuMouseButton {
                            button: TouchMouseButton::Left,
                            pressed: true,
                            cancelled: false,
                        });
                        finger.menu_button = Some(TouchMouseButton::Left);
                        effects.push(TouchEffect::MenuPointerMoved(clamp_point(point)));
                    }
                }
                None => {}
            },
            Capture::Action(_) | Capture::MenuPointer | Capture::Ignored => {}
        }
        finger.last = point;
    }

    fn update_movement(&mut self, id: u64, finger: &Finger, effects: &mut Vec<TouchEffect>) {
        if self.move_finger != Some(id) {
            return;
        }
        let Some(rect) = self
            .layout
            .controls
            .iter()
            .find(|region| region.control == TouchControl::MoveStick)
            .map(|region| region.rect)
        else {
            return;
        };
        let radius = (rect.width * self.viewport[0]).min(rect.height * self.viewport[1]) * 0.45;
        // Touch coordinates grow right and down, while the shared stick vector
        // is `[left, forward]` (the gamepad adapter performs the same inversion).
        let mut x = (finger.anchor[0] - finger.last[0]) * self.viewport[0] / radius;
        let mut y = (finger.anchor[1] - finger.last[1]) * self.viewport[1] / radius;
        let length = (x * x + y * y).sqrt();
        if length > 1.0 {
            x /= length;
            y /= length;
        }
        let length = (x * x + y * y).sqrt();
        let vector = if length <= self.layout.stick_deadzone || length == 0.0 {
            [0.0, 0.0]
        } else {
            let adjusted = ((length - self.layout.stick_deadzone)
                / (1.0 - self.layout.stick_deadzone))
                .clamp(0.0, 1.0);
            [x / length * adjusted, y / length * adjusted]
        };
        if vector != self.movement {
            self.movement = vector;
            effects.push(TouchEffect::MovementChanged(vector));
        }
    }

    fn finish_finger(
        &mut self,
        id: u64,
        finger: Finger,
        cancelled: bool,
        effects: &mut Vec<TouchEffect>,
    ) {
        match finger.capture {
            Capture::MoveStick if self.move_finger == Some(id) => {
                self.move_finger = None;
                if self.movement != [0.0, 0.0] {
                    self.movement = [0.0, 0.0];
                    effects.push(TouchEffect::MovementChanged(self.movement));
                }
            }
            Capture::Action(action) => self.release_action(action, cancelled, effects),
            Capture::MenuPointer if self.menu_finger == Some(id) => {
                self.menu_finger = None;
                if !finger.menu_scroll {
                    let button = finger.menu_button.unwrap_or(TouchMouseButton::Left);
                    if finger.menu_button.is_none() && !cancelled {
                        // A short, stationary tap commits a regular left click
                        // only when it ends, so inventory presses can still
                        // become right-click gestures after a long hold.
                        effects.push(TouchEffect::MenuPointerMoved(clamp_point(finger.last)));
                        effects.push(TouchEffect::MenuMouseButton {
                            button: TouchMouseButton::Left,
                            pressed: true,
                            cancelled: false,
                        });
                    }
                    if finger.menu_button.is_some() || !cancelled {
                        if finger.menu_button.is_some() {
                            effects.push(TouchEffect::MenuPointerMoved(clamp_point(finger.last)));
                        }
                        effects.push(TouchEffect::MenuMouseButton {
                            button,
                            pressed: false,
                            cancelled,
                        });
                    }
                }
            }
            Capture::MoveStick | Capture::Look | Capture::MenuPointer | Capture::Ignored => {}
        }
    }

    fn press_action(&mut self, action: TouchAction, effects: &mut Vec<TouchEffect>) {
        let was_active = self.action_active(action);
        let count = self.action_holds.entry(action).or_default();
        *count += 1;
        if !was_active {
            self.actions_sampled_by_player.remove(&action);
            effects.push(TouchEffect::ActionChanged {
                action,
                pressed: true,
            });
        }
    }

    fn release_action(
        &mut self,
        action: TouchAction,
        cancelled: bool,
        effects: &mut Vec<TouchEffect>,
    ) {
        let was_active = self.action_active(action);
        let Some(count) = self.action_holds.get_mut(&action) else {
            return;
        };
        *count = count.saturating_sub(1);
        if *count == 0 {
            self.action_holds.remove(&action);
            if action == TouchAction::Jump && !cancelled {
                if !self.actions_sampled_by_player.remove(&action) {
                    self.action_latches.insert(action);
                }
            } else {
                self.actions_sampled_by_player.remove(&action);
            }
        }
        if was_active && !self.action_active(action) {
            effects.push(TouchEffect::ActionChanged {
                action,
                pressed: false,
            });
        }
    }

    fn menu_drag_started(&self, anchor: [f32; 2], point: [f32; 2]) -> bool {
        let dx = (point[0] - anchor[0]) * self.viewport[0];
        let dy = (point[1] - anchor[1]) * self.viewport[1];
        dx * dx + dy * dy >= MENU_DRAG_THRESHOLD_PX * MENU_DRAG_THRESHOLD_PX
    }
}

fn finite_or(value: f32, fallback: f32) -> f32 {
    if value.is_finite() { value } else { fallback }
}

fn valid_viewport(viewport: [f32; 2]) -> bool {
    viewport.iter().all(|v| v.is_finite() && *v >= 1.0)
}

fn clean_point(point: [f32; 2], fallback: [f32; 2]) -> [f32; 2] {
    std::array::from_fn(|axis| {
        let v = point[axis];
        if v.is_finite() {
            v.clamp(-4.0, 4.0)
        } else {
            fallback[axis]
        }
    })
}

fn clamp_point(point: [f32; 2]) -> [f32; 2] {
    point.map(|v| v.clamp(0.0, 1.0))
}

/// A draft layout is isolated from live gameplay until the player presses Save.
pub struct LayoutEditor {
    pub draft: TouchLayout,
    fingers: BTreeMap<u64, (usize, [f32; 2])>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCommand {
    Save,
    Cancel,
}

impl LayoutEditor {
    pub fn new(layout: &TouchLayout) -> Self {
        Self {
            draft: layout.clone(),
            fingers: BTreeMap::new(),
        }
    }
    pub fn handle(&mut self, id: u64, phase: TouchPhase, point: [f32; 2]) -> Option<EditorCommand> {
        if !point.iter().all(|v| v.is_finite()) {
            self.fingers.remove(&id);
            return None;
        }
        let point = clamp_point(point);
        match phase {
            TouchPhase::Started if point[1] <= 0.11 => {
                // A toolbar capture is deliberately outside the controls' index range.
                let action = if point[0] < 0.20 {
                    0
                } else if point[0] < 0.40 {
                    1
                } else if point[0] < 0.60 {
                    2
                } else if point[0] < 0.80 {
                    3
                } else {
                    4
                };
                self.fingers.insert(id, (usize::MAX - action, point));
            }
            TouchPhase::Started => {
                if let Some(index) = self
                    .draft
                    .controls
                    .iter()
                    .rposition(|r| r.rect.contains(point))
                {
                    self.fingers.insert(id, (index, point));
                }
            }
            TouchPhase::Moved => {
                let (index, previous) = self.fingers.get(&id).copied()?;
                if index >= self.draft.controls.len() {
                    return None;
                }
                let other = self
                    .fingers
                    .iter()
                    .find(|(other_id, (i, _))| **other_id != id && *i == index)
                    .map(|(_, (_, p))| *p);
                let rect = &mut self.draft.controls[index].rect;
                if let Some(anchor) = other {
                    let distance = |p: [f32; 2]| {
                        ((p[0] - anchor[0]).powi(2) + (p[1] - anchor[1]).powi(2)).sqrt()
                    };
                    let old = distance(previous);
                    if old > 0.015 {
                        let scale = (distance(point) / old).clamp(0.8, 1.25);
                        let center = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
                        rect.width *= scale;
                        rect.height *= scale;
                        rect.x = center[0] - rect.width / 2.0;
                        rect.y = center[1] - rect.height / 2.0;
                    }
                } else {
                    rect.x += point[0] - previous[0];
                    rect.y += point[1] - previous[1];
                }
                rect.sanitize();
                rect.y = rect.y.max(0.12).min(1.0 - rect.height);
                self.fingers.insert(id, (index, point));
            }
            TouchPhase::Ended => {
                let (index, start) = self.fingers.remove(&id)?;
                if index > usize::MAX - 5 && point[1] <= 0.11 && (point[0] - start[0]).abs() < 0.12
                {
                    match usize::MAX - index {
                        0 => return Some(EditorCommand::Save),
                        1 => {
                            self.draft = TouchLayout::default();
                            self.fingers.clear();
                        }
                        2 => {
                            self.draft.look_sensitivity =
                                (self.draft.look_sensitivity - 0.05).max(0.05)
                        }
                        3 => {
                            self.draft.look_sensitivity =
                                (self.draft.look_sensitivity + 0.05).min(2.0)
                        }
                        _ => return Some(EditorCommand::Cancel),
                    }
                }
            }
            TouchPhase::Cancelled => {
                self.fingers.remove(&id);
            }
        }
        None
    }

    /// Drop in-progress editor gestures when the platform cancels touches.
    pub fn cancel_all(&mut self) {
        self.fingers.clear();
    }
}

pub fn draw_layout_editor(editor: &LayoutEditor, atlas: &HudAtlas, batch: &mut HudBatch) {
    let mut controls = TouchControls::default();
    controls.set_layout(editor.draft.clone());
    controls.set_enabled(true);
    draw_overlay(&controls, atlas, batch, (0.0, 0.0));
    let mut ui = Ui::new(atlas, batch, (0.0, 0.0));
    for region in &editor.draft.controls {
        if region.control == TouchControl::Look {
            let rect = region.rect;
            let (x, y, w, h) = (
                rect.x * ui.w,
                rect.y * ui.h,
                rect.width * ui.w,
                rect.height * ui.h,
            );
            ui.border(x, y, w, h, 8.0, rgba(236, 237, 243, 112));
            ui.text_centered(
                Face::Medium,
                ui_theme::BODY * 0.8,
                x + w / 2.0,
                y + 8.0,
                ui_theme::color::TEXT,
                "Look zone",
            );
        }
    }
    ui.rounded(0.0, 0.0, ui.w, ui.h * 0.12, 0.0, rgba(13, 14, 19, 245));
    for (index, label) in ["Save", "Reset", "Look -", "Look +", "Cancel"]
        .iter()
        .enumerate()
    {
        ui.text_centered(
            Face::Medium,
            ui_theme::BODY,
            ui.w * (index as f32 * 0.2 + 0.1),
            ui.h * 0.025,
            ui_theme::color::TEXT,
            label,
        );
    }
    let message = format!(
        "Drag to move; two fingers on a control to resize. Look sensitivity: {:.2}",
        editor.draft.look_sensitivity
    );
    ui.text_centered(
        Face::Regular,
        ui_theme::BODY * 0.8,
        ui.w * 0.5,
        ui.h * 0.125,
        ui_theme::color::TEXT,
        &message,
    );
    ui.finish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editor_keeps_changes_in_a_draft_and_cancel_does_not_commit() {
        let original = TouchLayout::default();
        let mut editor = LayoutEditor::new(&original);
        let rect = editor.draft.controls[0].rect;
        let start = [rect.x + rect.width / 2.0, rect.y + rect.height / 2.0];
        editor.handle(1, TouchPhase::Started, start);
        editor.handle(1, TouchPhase::Moved, [start[0] + 0.1, start[1] - 0.1]);
        assert_ne!(editor.draft.controls[0].rect, original.controls[0].rect);
        editor.handle(2, TouchPhase::Started, [0.9, 0.05]);
        assert_eq!(
            editor.handle(2, TouchPhase::Ended, [0.9, 0.05]),
            Some(EditorCommand::Cancel)
        );
        assert_eq!(original, TouchLayout::default());
    }

    fn event(
        id: u64,
        phase: TouchPhase,
        position: [f32; 2],
        viewport: [f32; 2],
        mode: TouchMode,
    ) -> TouchEvent {
        TouchEvent {
            id,
            phase,
            position,
            viewport,
            mode,
        }
    }

    fn has_action(effects: &[TouchEffect], action: TouchAction, pressed: bool) -> bool {
        effects.contains(&TouchEffect::ActionChanged { action, pressed })
    }

    #[test]
    fn simultaneous_stick_look_and_action_keep_independent_capture() {
        let mut touch = TouchControls::default();
        let viewport = [1200.0, 800.0];
        touch.handle(event(
            1,
            TouchPhase::Started,
            [0.12, 0.68],
            viewport,
            TouchMode::Gameplay,
        ));
        touch.handle(event(
            2,
            TouchPhase::Started,
            [0.52, 0.45],
            viewport,
            TouchMode::Gameplay,
        ));
        let pressed = touch.handle(event(
            3,
            TouchPhase::Started,
            [0.87, 0.60],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&pressed, TouchAction::Break, true));

        let movement = touch.handle(event(
            1,
            TouchPhase::Moved,
            [0.20, 0.68],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(
            movement
                .iter()
                .any(|effect| matches!(effect, TouchEffect::MovementChanged([x, _]) if *x < -0.5))
        );
        let look = touch.handle(event(
            2,
            TouchPhase::Moved,
            [0.56, 0.48],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(look.iter().any(
            |effect| matches!(effect, TouchEffect::LookDelta([x, y]) if *x > 0.0 && *y > 0.0)
        ));
        assert!(touch.action_active(TouchAction::Break));

        let released = touch.handle(event(
            3,
            TouchPhase::Ended,
            [0.87, 0.60],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&released, TouchAction::Break, false));
        assert!(!touch.action_active(TouchAction::Break));
        assert!(touch.movement_vector()[0] < -0.5);

        let cancelled = touch.handle(event(
            1,
            TouchPhase::Cancelled,
            [0.20, 0.68],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(cancelled.contains(&TouchEffect::MovementChanged([0.0, 0.0])));
        assert_eq!(touch.movement_vector(), [0.0, 0.0]);
    }

    #[test]
    fn cancellation_releases_actions_and_menu_pointer_without_leaks() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        let press = touch.handle(event(
            10,
            TouchPhase::Started,
            [0.87, 0.60],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&press, TouchAction::Break, true));
        let cancel = touch.cancel_all();
        assert!(has_action(&cancel, TouchAction::Break, false));
        assert!(!touch.action_active(TouchAction::Break));
        assert_eq!(touch.movement_vector(), [0.0, 0.0]);

        let start = touch.handle(event(
            11,
            TouchPhase::Started,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(start.is_empty());
        let cancel = touch.handle(event(
            11,
            TouchPhase::Cancelled,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(cancel.is_empty());
        assert!(touch.cancel_all().is_empty());
    }

    #[test]
    fn overlapping_fingers_release_action_only_after_the_last_owner() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        let first = touch.handle(event(
            1,
            TouchPhase::Started,
            [0.87, 0.60],
            viewport,
            TouchMode::Gameplay,
        ));
        let second = touch.handle(event(
            2,
            TouchPhase::Started,
            [0.87, 0.60],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&first, TouchAction::Break, true));
        assert!(!has_action(&second, TouchAction::Break, true));
        assert!(
            touch
                .handle(event(
                    1,
                    TouchPhase::Cancelled,
                    [0.87, 0.60],
                    viewport,
                    TouchMode::Gameplay
                ))
                .is_empty()
        );
        assert!(touch.action_active(TouchAction::Break));
        let last = touch.handle(event(
            2,
            TouchPhase::Ended,
            [0.87, 0.60],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&last, TouchAction::Break, false));
    }

    #[test]
    fn normalized_layout_survives_resize_and_serde_round_trip() {
        let layout = TouchLayout::default();
        let json = layout.to_json().unwrap();
        let restored = TouchLayout::from_json(&json).unwrap();
        assert_eq!(layout, restored);
        let break_rect = restored
            .controls
            .iter()
            .find(|region| region.control == TouchControl::Break)
            .unwrap()
            .rect;
        let point = [
            break_rect.x + break_rect.width / 2.0,
            break_rect.y + break_rect.height / 2.0,
        ];
        for viewport in [[2048.0, 1536.0], [1024.0, 768.0], [2732.0, 2048.0]] {
            let mut touch = TouchControls::default();
            touch.set_layout(restored.clone());
            let effects = touch.handle(event(
                1,
                TouchPhase::Started,
                point,
                viewport,
                TouchMode::Gameplay,
            ));
            assert!(has_action(&effects, TouchAction::Break, true));
        }
    }

    #[test]
    fn menu_pointer_has_one_drag_owner_and_mode_change_cancels_it_safely() {
        let mut touch = TouchControls::default();
        let viewport = [1000.0, 700.0];
        let start = touch.handle(event(
            1,
            TouchPhase::Started,
            [0.5, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(start.is_empty());
        let drag = touch.handle(event(
            1,
            TouchPhase::Moved,
            [0.52, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(drag.contains(&TouchEffect::MenuMouseButton {
            button: TouchMouseButton::Left,
            pressed: true,
            cancelled: false,
        }));
        assert!(
            touch
                .handle(event(
                    2,
                    TouchPhase::Started,
                    [0.7, 0.7],
                    viewport,
                    TouchMode::Menu
                ))
                .is_empty()
        );
        assert!(
            touch
                .handle(event(
                    2,
                    TouchPhase::Moved,
                    [0.9, 0.9],
                    viewport,
                    TouchMode::Menu
                ))
                .is_empty()
        );
        let switched = touch.handle(event(
            2,
            TouchPhase::Started,
            [0.1, 0.7],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(switched.contains(&TouchEffect::MenuMouseButton {
            button: TouchMouseButton::Left,
            pressed: false,
            cancelled: true,
        }));
    }

    #[test]
    fn menu_tap_defers_left_press_until_release_and_cancellation_stays_inert() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        let start = touch.handle(event(
            1,
            TouchPhase::Started,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(start.is_empty());
        let cancel = touch.handle(event(
            1,
            TouchPhase::Cancelled,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(cancel.is_empty());

        let start = touch.handle(event(
            2,
            TouchPhase::Started,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(start.is_empty());
        let tap = touch.handle(event(
            2,
            TouchPhase::Ended,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert_eq!(
            tap.iter()
                .filter(|effect| matches!(effect, TouchEffect::MenuMouseButton { .. }))
                .copied()
                .collect::<Vec<_>>(),
            vec![
                TouchEffect::MenuMouseButton {
                    button: TouchMouseButton::Left,
                    pressed: true,
                    cancelled: false,
                },
                TouchEffect::MenuMouseButton {
                    button: TouchMouseButton::Left,
                    pressed: false,
                    cancelled: false,
                },
            ]
        );
    }

    #[test]
    fn stationary_menu_long_press_is_right_click_and_cancels_without_commit() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        touch.handle(event(
            1,
            TouchPhase::Started,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));

        let press = touch.advance_at(Instant::now() + MENU_LONG_PRESS + Duration::from_millis(1));
        assert!(press.contains(&TouchEffect::MenuMouseButton {
            button: TouchMouseButton::Right,
            pressed: true,
            cancelled: false,
        }));
        assert!(!press.iter().any(|effect| matches!(
            effect,
            TouchEffect::MenuMouseButton {
                button: TouchMouseButton::Left,
                ..
            }
        )));

        let cancel = touch.handle(event(
            1,
            TouchPhase::Cancelled,
            [0.4, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(cancel.contains(&TouchEffect::MenuMouseButton {
            button: TouchMouseButton::Right,
            pressed: false,
            cancelled: true,
        }));
        assert!(touch.advance().is_empty());
    }

    #[test]
    fn menu_drag_crossing_threshold_uses_left_button_and_keeps_capture() {
        let mut touch = TouchControls::default();
        let viewport = [1000.0, 700.0];
        touch.handle(event(
            1,
            TouchPhase::Started,
            [0.5, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        let drag = touch.handle(event(
            1,
            TouchPhase::Moved,
            [0.52, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(drag.contains(&TouchEffect::MenuMouseButton {
            button: TouchMouseButton::Left,
            pressed: true,
            cancelled: false,
        }));
        let end = touch.handle(event(
            1,
            TouchPhase::Ended,
            [0.54, 0.5],
            viewport,
            TouchMode::Menu,
        ));
        assert!(end.contains(&TouchEffect::MenuMouseButton {
            button: TouchMouseButton::Left,
            pressed: false,
            cancelled: false,
        }));
    }

    #[test]
    fn joystick_uses_left_and_forward_axes_for_screen_directions() {
        let mut touch = TouchControls::default();
        let viewport = [1200.0, 800.0];
        let anchor = [0.16, 0.70];
        touch.handle(event(
            1,
            TouchPhase::Started,
            anchor,
            viewport,
            TouchMode::Gameplay,
        ));

        touch.handle(event(
            1,
            TouchPhase::Moved,
            [anchor[0] + 0.20, anchor[1]],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(touch.movement_vector()[0] < -0.99);
        assert!(touch.movement_vector()[1].abs() < 0.001);

        touch.handle(event(
            1,
            TouchPhase::Moved,
            [anchor[0], anchor[1] - 0.20],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(touch.movement_vector()[0].abs() < 0.001);
        assert!(touch.movement_vector()[1] > 0.99);

        touch.handle(event(
            1,
            TouchPhase::Moved,
            [anchor[0], anchor[1] + 0.20],
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(touch.movement_vector()[1] < -0.99);
    }

    #[test]
    fn short_jump_tap_stays_held_until_one_player_tick() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        let point = [0.88, 0.855];
        let press = touch.handle(event(
            1,
            TouchPhase::Started,
            point,
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&press, TouchAction::Jump, true));

        let release = touch.handle(event(
            1,
            TouchPhase::Ended,
            point,
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(!has_action(&release, TouchAction::Jump, false));
        assert!(touch.action_active(TouchAction::Jump));

        let tick = touch.player_tick_complete();
        assert_eq!(
            tick,
            vec![TouchEffect::ActionChanged {
                action: TouchAction::Jump,
                pressed: false,
            }]
        );
        assert!(!touch.action_active(TouchAction::Jump));
        assert!(touch.player_tick_complete().is_empty());
    }

    #[test]
    fn held_jump_releases_after_sampled_tick_and_cancel_flushes_a_tap_latch() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        let point = [0.88, 0.855];
        let press = event(1, TouchPhase::Started, point, viewport, TouchMode::Gameplay);
        let release = event(1, TouchPhase::Ended, point, viewport, TouchMode::Gameplay);

        touch.handle(press);
        assert!(touch.player_tick_complete().is_empty());
        let release_after_tick = touch.handle(release);
        assert!(has_action(&release_after_tick, TouchAction::Jump, false));
        assert!(!touch.action_active(TouchAction::Jump));

        touch.handle(press);
        touch.handle(release);
        assert!(touch.action_active(TouchAction::Jump));
        assert!(has_action(&touch.cancel_all(), TouchAction::Jump, false));
        assert!(!touch.action_active(TouchAction::Jump));
        assert!(touch.player_tick_complete().is_empty());
    }

    #[test]
    fn pending_jump_tap_does_not_extend_a_new_hold_past_its_sampled_tick() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        let point = [0.88, 0.855];

        touch.handle(event(
            1,
            TouchPhase::Started,
            point,
            viewport,
            TouchMode::Gameplay,
        ));
        touch.handle(event(
            1,
            TouchPhase::Ended,
            point,
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(touch.action_active(TouchAction::Jump));

        touch.handle(event(
            2,
            TouchPhase::Started,
            point,
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(touch.player_tick_complete().is_empty());
        assert!(touch.action_active(TouchAction::Jump));

        let release = touch.handle(event(
            2,
            TouchPhase::Ended,
            point,
            viewport,
            TouchMode::Gameplay,
        ));
        assert!(has_action(&release, TouchAction::Jump, false));
        assert!(!touch.action_active(TouchAction::Jump));
        assert!(touch.player_tick_complete().is_empty());
    }

    #[test]
    fn vertical_menu_swipe_scrolls_without_clicking() {
        let mut touch = TouchControls::default();
        let viewport = [1024.0, 768.0];
        touch.handle(event(
            1,
            TouchPhase::Started,
            [0.5, 0.5],
            viewport,
            TouchMode::Menu,
        ));

        let first = touch.handle(event(
            1,
            TouchPhase::Moved,
            [0.5, 0.55],
            viewport,
            TouchMode::Menu,
        ));
        assert!(first.iter().any(|effect| matches!(
            effect,
            TouchEffect::MenuScrolled([x, y]) if x.abs() < 0.001 && (*y + 38.4).abs() < 0.01
        )));
        assert!(
            !first
                .iter()
                .any(|effect| matches!(effect, TouchEffect::MenuMouseButton { .. }))
        );

        let continued = touch.handle(event(
            1,
            TouchPhase::Moved,
            [0.5, 0.60],
            viewport,
            TouchMode::Menu,
        ));
        assert!(continued.iter().any(|effect| matches!(
            effect,
            TouchEffect::MenuScrolled([x, y]) if x.abs() < 0.001 && (*y + 38.4).abs() < 0.01
        )));

        let end = touch.handle(event(
            1,
            TouchPhase::Ended,
            [0.5, 0.60],
            viewport,
            TouchMode::Menu,
        ));
        assert!(
            !end.iter()
                .any(|effect| matches!(effect, TouchEffect::MenuMouseButton { .. }))
        );
    }
}
