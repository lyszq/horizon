//! Minimal Objective-C ABI bindings. All UI calls run on the main thread and
//! accept a live winit window handle instead of arbitrary object pointers.
#![allow(unsafe_op_in_unsafe_fn)]
use objc2::runtime::Bool;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::{
    cell::RefCell,
    ffi::{CStr, CString, c_char, c_void},
    path::{Path, PathBuf},
    ptr,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
};

type Id = *mut c_void;
type Sel = *mut c_void;
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Point {
    x: f64,
    y: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Size {
    width: f64,
    height: f64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Rect {
    origin: Point,
    size: Size,
}
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Insets {
    pub top: f64,
    pub left: f64,
    pub bottom: f64,
    pub right: f64,
}

#[link(name = "objc")]
unsafe extern "C" {
    fn objc_getClass(name: *const c_char) -> Id;
    fn sel_registerName(name: *const c_char) -> Sel;
    fn objc_msgSend();
    fn objc_allocateClassPair(superclass: Id, name: *const c_char, bytes: usize) -> Id;
    fn objc_registerClassPair(class: Id);
    fn class_addMethod(
        class: Id,
        selector: Sel,
        method: unsafe extern "C" fn(),
        types: *const c_char,
    ) -> bool;
}
#[link(name = "UIKit", kind = "framework")]
unsafe extern "C" {}
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}
#[link(name = "GameController", kind = "framework")]
unsafe extern "C" {}
#[link(name = "AVFAudio", kind = "framework")]
unsafe extern "C" {}
unsafe extern "C" {
    // mach_task_self() is a C macro for this exported port, not a function.
    static mut mach_task_self_: u32;
    fn task_info(task: u32, flavor: u32, info: *mut u32, count: *mut u32) -> i32;
}

macro_rules! msg {
    ($object:expr, $selector:literal, ($($arg:expr => $ty:ty),* $(,)?) -> $ret:ty) => {{
        let call: unsafe extern "C" fn(Id, Sel, $($ty),*) -> $ret =
            std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
        call($object, sel_registerName(concat!($selector, "\0").as_ptr().cast()), $($arg),*)
    }};
}
unsafe fn class(name: &'static CStr) -> Id {
    objc_getClass(name.as_ptr())
}
unsafe fn string(value: &str) -> Id {
    let value = CString::new(value.replace('\0', "")).expect("NUL removed");
    msg!(class(c"NSString"), "stringWithUTF8String:", (value.as_ptr() => *const c_char) -> Id)
}
unsafe fn text(object: Id) -> String {
    if object.is_null() {
        return String::new();
    }
    let bytes = msg!(object, "UTF8String", () -> *const c_char);
    if bytes.is_null() {
        String::new()
    } else {
        CStr::from_ptr(bytes).to_string_lossy().into_owned()
    }
}
unsafe fn array(object: Id) -> Id {
    msg!(class(c"NSArray"), "arrayWithObject:", (object => Id) -> Id)
}
unsafe fn main_thread() -> bool {
    msg!(class(c"NSThread"), "isMainThread", () -> bool)
}
unsafe fn app() -> Id {
    msg!(class(c"UIApplication"), "sharedApplication", () -> Id)
}

fn handles(window: &impl HasWindowHandle) -> Result<(Id, Id), String> {
    // HasWindowHandle guarantees validity for the duration of this borrow.
    if !unsafe { main_thread() } {
        return Err("UIKit operation must run on the main thread".into());
    }
    match window.window_handle().map_err(|e| e.to_string())?.as_raw() {
        RawWindowHandle::UiKit(h) => h
            .ui_view_controller
            .map(|controller| (h.ui_view.as_ptr(), controller.as_ptr()))
            .ok_or_else(|| "The iPad window has no view controller".into()),
        _ => Err("A UIKit window is required".into()),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickKind {
    Resources,
    World,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
}
#[derive(Debug)]
pub enum Event {
    Picked { path: PathBuf, kind: PickKind },
    Cancelled,
    ImportResources,
    ImportWorld,
    EditTouch,
    Play,
    Benchmark,
    ShowSetup,
    Share,
    Text(String),
    Backspace,
    Enter,
    MouseMotion(f64, f64),
    MouseButton { button: MouseButton, pressed: bool },
    MouseWheel(f64, f64),
    AudioInterrupted,
    AudioResume,
    AudioRouteChanged,
    Error(String),
}
static EVENTS: Mutex<Vec<Event>> = Mutex::new(Vec::new());
fn push(event: Event) {
    if let Ok(mut events) = EVENTS.lock() {
        events.push(event);
    }
}
pub fn poll_events() -> Vec<Event> {
    // GameController does not feed GCMouse events through winit on UIKit. Bind
    // its relative-only callbacks from the main-thread frame poll; touch and
    // UIKit cursor-position events remain owned by winit and aren't repeated.
    unsafe {
        sync_mouse();
    }
    EVENTS
        .lock()
        .map(|mut e| std::mem::take(&mut *e))
        .unwrap_or_default()
}
static PICK_KIND: AtomicUsize = AtomicUsize::new(0);

// Import mode gives the application its own copy in Inbox. The selection is
// therefore safe for a worker to read after the delegate returns; the provider
// original is never moved or modified.
unsafe extern "C" fn picked(_: Id, _: Sel, _: Id, urls: Id) {
    let kind = PICK_KIND.swap(0, Ordering::SeqCst);
    if msg!(urls, "count", () -> usize) == 0 {
        push(Event::Cancelled);
        return;
    }
    let url = msg!(urls, "objectAtIndex:", (0usize => usize) -> Id);
    let path = text(msg!(url, "path", () -> Id));
    if !path.is_empty() {
        push(Event::Picked {
            path: PathBuf::from(path),
            kind: if kind == 2 {
                PickKind::World
            } else {
                PickKind::Resources
            },
        });
    }
}
unsafe extern "C" fn cancelled(_: Id, _: Sel, _: Id) {
    PICK_KIND.store(0, Ordering::SeqCst);
    push(Event::Cancelled);
}
unsafe extern "C" fn import_resources(_: Id, _: Sel, _: Id) {
    push(Event::ImportResources);
}
unsafe extern "C" fn import_world(_: Id, _: Sel, _: Id) {
    push(Event::ImportWorld);
}
unsafe extern "C" fn edit_touch(_: Id, _: Sel, _: Id) {
    push(Event::EditTouch);
}
unsafe extern "C" fn play(_: Id, _: Sel, _: Id) {
    push(Event::Play);
}
unsafe extern "C" fn benchmark(_: Id, _: Sel, _: Id) {
    push(Event::Benchmark);
}
unsafe extern "C" fn show_setup_action(_: Id, _: Sel, _: Id) {
    push(Event::ShowSetup);
}
unsafe extern "C" fn share(_: Id, _: Sel, _: Id) {
    push(Event::Share);
}
unsafe extern "C" fn changed(_: Id, _: Sel, field: Id) {
    if !msg!(field, "markedTextRange", () -> Id).is_null() {
        return;
    }
    let value = text(msg!(field, "text", () -> Id));
    if value.is_empty() {
        push(Event::Backspace);
    } else {
        let value = value.strip_prefix(' ').unwrap_or(&value);
        if !value.is_empty() {
            push(Event::Text(value.to_owned()));
        }
    }
    msg!(field, "setText:", (string(" ") => Id) -> ());
}
unsafe extern "C" fn enter(_: Id, _: Sel, field: Id) -> Bool {
    push(Event::Enter);
    msg!(field, "resignFirstResponder", () -> bool);
    Bool::YES
}
unsafe fn delegate() -> Id {
    static DELEGATE: OnceLock<usize> = OnceLock::new();
    *DELEGATE.get_or_init(|| {
        let name = c"HorizonIPadActions";
        let cls = objc_allocateClassPair(class(c"NSObject"), name.as_ptr(), 0);
        macro_rules! method {
            ($name:literal, $function:path, $types:literal, $signature:ty) => {
                assert!(class_addMethod(
                    cls,
                    sel_registerName(concat!($name, "\0").as_ptr().cast()),
                    std::mem::transmute::<$signature, unsafe extern "C" fn()>($function),
                    concat!($types, "\0").as_ptr().cast()
                ));
            };
        }
        method!(
            "documentPicker:didPickDocumentsAtURLs:",
            picked,
            "v@:@@",
            unsafe extern "C" fn(Id, Sel, Id, Id)
        );
        method!(
            "documentPickerWasCancelled:",
            cancelled,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!(
            "importResources:",
            import_resources,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!(
            "importWorld:",
            import_world,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!(
            "editTouch:",
            edit_touch,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!("play:", play, "v@:@", unsafe extern "C" fn(Id, Sel, Id));
        method!(
            "benchmark:",
            benchmark,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!(
            "showSetup:",
            show_setup_action,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!("share:", share, "v@:@", unsafe extern "C" fn(Id, Sel, Id));
        method!(
            "textChanged:",
            changed,
            "v@:@",
            unsafe extern "C" fn(Id, Sel, Id)
        );
        method!(
            "textFieldShouldReturn:",
            enter,
            "B@:@",
            unsafe extern "C" fn(Id, Sel, Id) -> Bool
        );
        objc_registerClassPair(cls);
        msg!(msg!(cls, "alloc", () -> Id), "init", () -> Id) as usize
    }) as Id
}

pub fn open_picker(window: &impl HasWindowHandle, kind: PickKind) -> Result<(), String> {
    let (_, controller) = handles(window)?;
    if PICK_KIND
        .compare_exchange(
            0,
            if kind == PickKind::World { 2 } else { 1 },
            Ordering::SeqCst,
            Ordering::SeqCst,
        )
        .is_err()
    {
        return Err("A file picker is already open".into());
    }
    unsafe {
        let picker = msg!(msg!(class(c"UIDocumentPickerViewController"), "alloc", () -> Id),
            "initWithDocumentTypes:inMode:", (array(string("public.zip-archive")) => Id, 0isize => isize) -> Id);
        if picker.is_null() {
            PICK_KIND.store(0, Ordering::SeqCst);
            return Err("File picker could not be created".into());
        }
        msg!(picker, "setDelegate:", (delegate() => Id) -> ());
        msg!(picker, "setAllowsMultipleSelection:", (false => bool) -> ());
        msg!(controller, "presentViewController:animated:completion:", (picker => Id, true => bool, ptr::null_mut::<c_void>() => Id) -> ());
        msg!(picker, "release", () -> ());
    }
    Ok(())
}

const SETUP_TAG: isize = 804320;
const KEYBOARD_TAG: isize = 804321;
const SETUP_ACCESSORY_TAG: isize = 804322;
pub fn hide_setup(window: &impl HasWindowHandle) {
    if let Ok((view, _)) = handles(window) {
        unsafe {
            let panel = msg!(view, "viewWithTag:", (SETUP_TAG => isize) -> Id);
            msg!(panel, "removeFromSuperview", () -> ());
        }
    }
}
pub fn show_setup(window: &impl HasWindowHandle, message: &str, ready: bool) -> Result<(), String> {
    let (view, _) = handles(window)?;
    hide_setup(window);
    unsafe {
        let accessory = msg!(view, "viewWithTag:", (SETUP_ACCESSORY_TAG => isize) -> Id);
        msg!(accessory, "removeFromSuperview", () -> ());
        let bounds = msg!(view, "bounds", () -> Rect);
        // Stage Manager can make the window shorter than the setup controls.
        // Keep all actions reachable instead of placing them outside hit-testing.
        let panel = msg!(msg!(class(c"UIScrollView"), "alloc", () -> Id), "initWithFrame:", (bounds => Rect) -> Id);
        msg!(panel, "setTag:", (SETUP_TAG => isize) -> ());
        msg!(panel, "setAutoresizingMask:", (18usize => usize) -> ());
        msg!(panel, "setContentSize:", (Size { width: bounds.size.width, height: bounds.size.height.max(620.0) } => Size) -> ());
        msg!(panel, "setAlwaysBounceVertical:", (true => bool) -> ());
        let background = msg!(class(c"UIColor"), "colorWithRed:green:blue:alpha:", (0.075f64 => f64, 0.09f64 => f64, 0.11f64 => f64, 1.0f64 => f64) -> Id);
        msg!(panel, "setBackgroundColor:", (background => Id) -> ());
        let width = bounds.size.width.min(660.0) - 48.0;
        let x = (bounds.size.width - width) / 2.0;
        let label = msg!(msg!(class(c"UILabel"), "alloc", () -> Id), "initWithFrame:",
            (Rect { origin: Point { x, y: 54.0 }, size: Size { width, height: 150.0 } } => Rect) -> Id);
        msg!(label, "setText:", (string(&format!("Horizon for iPad\n\n{message}")) => Id) -> ());
        msg!(label, "setTextColor:", (msg!(class(c"UIColor"), "whiteColor", () -> Id) => Id) -> ());
        msg!(label, "setNumberOfLines:", (0isize => isize) -> ());
        msg!(label, "setTextAlignment:", (1isize => isize) -> ());
        // Keep the centered content inside the panel when Split View or rotation
        // changes the UIKit view's width.
        msg!(label, "setAutoresizingMask:", (7usize => usize) -> ());
        msg!(panel, "addSubview:", (label => Id) -> ());
        msg!(label, "release", () -> ());
        for (index, (title, action, enabled)) in [
            ("Import resources", "importResources:", true),
            ("Touch controls", "editTouch:", ready),
            ("Play", "play:", ready),
            ("Import world", "importWorld:", ready),
            ("Run benchmark", "benchmark:", ready),
            ("Share diagnostics / backups", "share:", true),
        ]
        .iter()
        .enumerate()
        {
            let button = msg!(class(c"UIButton"), "buttonWithType:", (1isize => isize) -> Id);
            msg!(button, "setFrame:", (Rect { origin: Point { x, y: 218.0 + index as f64 * 58.0 }, size: Size { width, height: 48.0 } } => Rect) -> ());
            msg!(button, "setAutoresizingMask:", (7usize => usize) -> ());
            msg!(button, "setTitle:forState:", (string(title) => Id, 0usize => usize) -> ());
            msg!(button, "setEnabled:", (*enabled => bool) -> ());
            let selector = CString::new(*action).expect("static selector");
            msg!(button, "addTarget:action:forControlEvents:", (delegate() => Id, sel_registerName(selector.as_ptr()) => Sel, 1usize << 6 => usize) -> ());
            msg!(panel, "addSubview:", (button => Id) -> ());
        }
        msg!(view, "addSubview:", (panel => Id) -> ());
        msg!(panel, "release", () -> ());
    }
    Ok(())
}

/// Install a compact native Files control over the running game view. The
/// action only queues `Event::ShowSetup`; the caller must save/close the world
/// before replacing the game view with the setup panel.
pub fn setup_accessory(window: &impl HasWindowHandle) -> Result<(), String> {
    let (view, _) = handles(window)?;
    unsafe {
        if !msg!(view, "viewWithTag:", (SETUP_ACCESSORY_TAG => isize) -> Id).is_null() {
            return Ok(());
        }
        let insets = msg!(view, "safeAreaInsets", () -> Insets);
        let button = msg!(class(c"UIButton"), "buttonWithType:", (1isize => isize) -> Id);
        msg!(button, "setTag:", (SETUP_ACCESSORY_TAG => isize) -> ());
        msg!(button, "setFrame:", (Rect {
            origin: Point { x: insets.left + 14.0, y: insets.top + 14.0 },
            size: Size { width: 82.0, height: 44.0 },
        } => Rect) -> ());
        // Keep the shortcut anchored at the upper-left when the game view rotates.
        msg!(button, "setAutoresizingMask:", (36usize => usize) -> ());
        msg!(button, "setTitle:forState:", (string("Files") => Id, 0usize => usize) -> ());
        msg!(button, "setTitleColor:forState:", (msg!(class(c"UIColor"), "whiteColor", () -> Id) => Id, 0usize => usize) -> ());
        msg!(button, "setBackgroundColor:", (msg!(class(c"UIColor"), "colorWithWhite:alpha:", (0.08f64 => f64, 0.84f64 => f64) -> Id) => Id) -> ());
        msg!(button, "addTarget:action:forControlEvents:", (delegate() => Id, sel_registerName(c"showSetup:".as_ptr()) => Sel, 1usize << 6 => usize) -> ());
        msg!(view, "addSubview:", (button => Id) -> ());
    }
    Ok(())
}

/// Remove the native Files shortcut while a touch-layout editor owns the screen.
pub fn hide_accessory(window: &impl HasWindowHandle) {
    if let Ok((view, _)) = handles(window) {
        unsafe {
            let accessory = msg!(view, "viewWithTag:", (SETUP_ACCESSORY_TAG => isize) -> Id);
            msg!(accessory, "removeFromSuperview", () -> ());
        }
    }
}

pub fn keyboard(window: &impl HasWindowHandle, visible: bool) {
    let Ok((view, _)) = handles(window) else {
        return;
    };
    unsafe {
        let mut field = msg!(view, "viewWithTag:", (KEYBOARD_TAG => isize) -> Id);
        if visible && field.is_null() {
            field = msg!(msg!(class(c"UITextField"), "alloc", () -> Id), "initWithFrame:", (Rect { size: Size { width: 1.0, height: 1.0 }, ..Rect::default() } => Rect) -> Id);
            msg!(field, "setTag:", (KEYBOARD_TAG => isize) -> ());
            msg!(field, "setAlpha:", (0.01f64 => f64) -> ());
            msg!(field, "setAutocorrectionType:", (1isize => isize) -> ());
            msg!(field, "setAutocapitalizationType:", (0isize => isize) -> ());
            msg!(field, "setText:", (string(" ") => Id) -> ());
            msg!(field, "setDelegate:", (delegate() => Id) -> ());
            msg!(field, "addTarget:action:forControlEvents:", (delegate() => Id, sel_registerName(c"textChanged:".as_ptr()) => Sel, 1usize << 17 => usize) -> ());
            msg!(view, "addSubview:", (field => Id) -> ());
            msg!(field, "release", () -> ());
        }
        if !field.is_null() {
            if visible {
                msg!(field, "becomeFirstResponder", () -> bool);
            } else {
                msg!(field, "resignFirstResponder", () -> bool);
            }
        }
    }
}
/// Physical viewport above the docked keyboard. UIKit tracks split-window and
/// keyboard animation geometry; an undocked keyboard leaves the layout intact.
pub fn keyboard_viewport(window: &impl HasWindowHandle, physical: [u32; 2]) -> [u32; 2] {
    let Ok((view, _)) = handles(window) else {
        return physical;
    };
    unsafe {
        let field = msg!(view, "viewWithTag:", (KEYBOARD_TAG => isize) -> Id);
        if field.is_null() || !msg!(field, "isFirstResponder", () -> bool) {
            return physical;
        }
        let bounds = msg!(view, "bounds", () -> Rect);
        let guide = msg!(view, "keyboardLayoutGuide", () -> Id);
        let frame = msg!(guide, "layoutFrame", () -> Rect);
        if bounds.size.height <= 0.0 || frame.size.height <= 0.0 {
            return physical;
        }
        let fraction = ((frame.origin.y - bounds.origin.y) / bounds.size.height).clamp(0.0, 1.0);
        [
            physical[0],
            ((f64::from(physical[1]) * fraction).floor() as u32).max(1),
        ]
    }
}
pub fn safe_area(window: &impl HasWindowHandle) -> Insets {
    handles(window)
        .map(|(view, _)| unsafe { msg!(view, "safeAreaInsets", () -> Insets) })
        .unwrap_or_default()
}
pub fn clipboard_get() -> String {
    unsafe {
        if !main_thread() {
            return String::new();
        }
        text(msg!(msg!(class(c"UIPasteboard"), "generalPasteboard", () -> Id), "string", () -> Id))
    }
}
pub fn clipboard_set(value: &str) {
    unsafe {
        if main_thread() {
            msg!(msg!(class(c"UIPasteboard"), "generalPasteboard", () -> Id), "setString:", (string(value) => Id) -> ());
        }
    }
}
pub fn share_file(window: &impl HasWindowHandle, path: &Path) -> Result<(), String> {
    let (view, controller) = handles(window)?;
    if !path.is_file() {
        return Err("There is no file to share yet".into());
    }
    unsafe {
        let url = msg!(class(c"NSURL"), "fileURLWithPath:", (string(&path.to_string_lossy()) => Id) -> Id);
        let share = msg!(msg!(class(c"UIActivityViewController"), "alloc", () -> Id), "initWithActivityItems:applicationActivities:", (array(url) => Id, ptr::null_mut::<c_void>() => Id) -> Id);
        let popover = msg!(share, "popoverPresentationController", () -> Id);
        msg!(popover, "setSourceView:", (view => Id) -> ());
        msg!(popover, "setSourceRect:", (Rect { origin: Point { x: 24.0, y: 24.0 }, size: Size { width: 1.0, height: 1.0 } } => Rect) -> ());
        msg!(controller, "presentViewController:animated:completion:", (share => Id, true => bool, ptr::null_mut::<c_void>() => Id) -> ());
        msg!(share, "release", () -> ());
    }
    Ok(())
}

#[derive(Default, Clone, Copy, Debug)]
pub struct ControllerState {
    pub connected: bool,
    pub buttons: u32,
    pub left: [f32; 2],
    pub right: [f32; 2],
    pub triggers: [f32; 2],
}
unsafe fn button(gamepad: Id, name: &'static CStr) -> Id {
    let selector = sel_registerName(name.as_ptr());
    if !msg!(gamepad, "respondsToSelector:", (selector => Sel) -> bool) {
        return ptr::null_mut();
    }
    let call: unsafe extern "C" fn(Id, Sel) -> Id =
        std::mem::transmute(objc_msgSend as unsafe extern "C" fn());
    call(gamepad, selector)
}
pub fn controller() -> ControllerState {
    unsafe {
        if !main_thread() {
            return ControllerState::default();
        }
        let mut active = msg!(class(c"GCController"), "current", () -> Id);
        if active.is_null() {
            let controllers = msg!(class(c"GCController"), "controllers", () -> Id);
            if msg!(controllers, "count", () -> usize) == 0 {
                return ControllerState::default();
            }
            active = msg!(controllers, "objectAtIndex:", (0usize => usize) -> Id);
        }
        let pad = msg!(active, "extendedGamepad", () -> Id);
        if pad.is_null() {
            return ControllerState::default();
        }
        let mut state = ControllerState {
            connected: true,
            ..ControllerState::default()
        };
        for (index, name) in [
            c"buttonA",
            c"buttonB",
            c"buttonX",
            c"buttonY",
            c"leftShoulder",
            c"rightShoulder",
            c"leftTrigger",
            c"rightTrigger",
            c"buttonOptions",
            c"buttonMenu",
            c"leftThumbstickButton",
            c"rightThumbstickButton",
        ]
        .iter()
        .enumerate()
        {
            if msg!(button(pad, name), "isPressed", () -> bool) {
                state.buttons |= 1 << index;
            }
        }
        let dpad = msg!(pad, "dpad", () -> Id);
        for (index, name) in [c"up", c"down", c"left", c"right"].iter().enumerate() {
            if msg!(button(dpad, name), "isPressed", () -> bool) {
                state.buttons |= 1 << (12 + index);
            }
        }
        let stick = |name| {
            let stick = button(pad, name);
            [
                msg!(msg!(stick, "xAxis", () -> Id), "value", () -> f32),
                msg!(msg!(stick, "yAxis", () -> Id), "value", () -> f32),
            ]
        };
        state.left = stick(c"leftThumbstick");
        state.right = stick(c"rightThumbstick");
        state.triggers = [
            msg!(button(pad, c"leftTrigger"), "value", () -> f32),
            msg!(button(pad, c"rightTrigger"), "value", () -> f32),
        ];
        state
    }
}

type MouseMovedHandler = dyn Fn(Id, f32, f32);
type ButtonChangedHandler = dyn Fn(Id, f32, Bool);
type ScrollChangedHandler = dyn Fn(Id, f32, f32);

struct MouseBinding {
    mouse: Id,
    input: Id,
    buttons: [Id; 3],
    scroll: Id,
    _moved: block2::RcBlock<MouseMovedHandler>,
    _button_handlers: [Option<block2::RcBlock<ButtonChangedHandler>>; 3],
    _scroll: block2::RcBlock<ScrollChangedHandler>,
}

thread_local! {
    static MOUSE_BINDING: RefCell<Option<MouseBinding>> = const { RefCell::new(None) };
}

unsafe fn current_mouse() -> Id {
    let mice = msg!(class(c"GCMouse"), "mice", () -> Id);
    let count = msg!(mice, "count", () -> usize);
    if count == 0 {
        return ptr::null_mut();
    }
    let current = msg!(class(c"GCMouse"), "current", () -> Id);
    if !current.is_null() {
        for index in 0..count {
            if msg!(mice, "objectAtIndex:", (index => usize) -> Id) == current {
                return current;
            }
        }
    }
    msg!(mice, "objectAtIndex:", (0usize => usize) -> Id)
}

unsafe fn clear_mouse(binding: MouseBinding) {
    msg!(binding.input, "setMouseMovedHandler:", (ptr::null_mut::<block2::Block<MouseMovedHandler>>() => *mut block2::Block<MouseMovedHandler>) -> ());
    for button in binding.buttons {
        if !button.is_null() {
            msg!(button, "setPressedChangedHandler:", (ptr::null_mut::<block2::Block<ButtonChangedHandler>>() => *mut block2::Block<ButtonChangedHandler>) -> ());
        }
    }
    if !binding.scroll.is_null() {
        msg!(binding.scroll, "setValueChangedHandler:", (ptr::null_mut::<block2::Block<ScrollChangedHandler>>() => *mut block2::Block<ScrollChangedHandler>) -> ());
    }
    // The binding owns one explicit retain so its profile and element pointers
    // remain valid until all callback properties have been cleared.
    msg!(binding.mouse, "release", () -> ());
}

unsafe fn bind_mouse(mouse: Id) -> Option<MouseBinding> {
    msg!(mouse, "retain", () -> Id);
    let input = msg!(mouse, "mouseInput", () -> Id);
    if input.is_null() {
        msg!(mouse, "release", () -> ());
        return None;
    }

    let moved: block2::RcBlock<MouseMovedHandler> =
        block2::RcBlock::new(|_input: Id, delta_x: f32, delta_y: f32| {
            // GameController's raw Y is upward-positive; MouseHandler follows
            // winit's downward-positive window coordinates (as touch does).
            push(Event::MouseMotion(f64::from(delta_x), -f64::from(delta_y)));
        });
    msg!(input, "setMouseMovedHandler:", (block2::RcBlock::as_ptr(&moved) => *mut block2::Block<MouseMovedHandler>) -> ());

    let names = [c"leftButton", c"rightButton", c"middleButton"];
    let kinds = [MouseButton::Left, MouseButton::Right, MouseButton::Middle];
    let mut buttons = [ptr::null_mut(); 3];
    let mut button_handlers: [Option<block2::RcBlock<ButtonChangedHandler>>; 3] =
        [None, None, None];
    for index in 0..names.len() {
        let button = button(input, names[index]);
        buttons[index] = button;
        if button.is_null() {
            continue;
        }
        let kind = kinds[index];
        let handler: block2::RcBlock<ButtonChangedHandler> =
            block2::RcBlock::new(move |_button: Id, _value: f32, pressed: Bool| {
                push(Event::MouseButton {
                    button: kind,
                    pressed: pressed.as_bool(),
                });
            });
        msg!(button, "setPressedChangedHandler:", (block2::RcBlock::as_ptr(&handler) => *mut block2::Block<ButtonChangedHandler>) -> ());
        button_handlers[index] = Some(handler);
    }

    let scroll = msg!(input, "scroll", () -> Id);
    let scroll_handler: block2::RcBlock<ScrollChangedHandler> =
        block2::RcBlock::new(|_cursor: Id, delta_x: f32, delta_y: f32| {
            push(Event::MouseWheel(f64::from(delta_x), f64::from(delta_y)));
        });
    if !scroll.is_null() {
        msg!(scroll, "setValueChangedHandler:", (block2::RcBlock::as_ptr(&scroll_handler) => *mut block2::Block<ScrollChangedHandler>) -> ());
    }

    Some(MouseBinding {
        mouse,
        input,
        buttons,
        scroll,
        _moved: moved,
        _button_handlers: button_handlers,
        _scroll: scroll_handler,
    })
}

unsafe fn sync_mouse() {
    if !main_thread() {
        return;
    }
    let current = current_mouse();
    MOUSE_BINDING.with(|slot| {
        if slot
            .borrow()
            .as_ref()
            .is_some_and(|binding| binding.mouse == current)
        {
            return;
        }
        let previous = slot.borrow_mut().take();
        if let Some(previous) = previous {
            unsafe {
                clear_mouse(previous);
            }
            for button in [MouseButton::Left, MouseButton::Right, MouseButton::Middle] {
                push(Event::MouseButton {
                    button,
                    pressed: false,
                });
            }
        }
        if !current.is_null()
            && let Some(binding) = unsafe { bind_mouse(current) }
        {
            *slot.borrow_mut() = Some(binding);
        }
    });
}

type NotificationHandler = dyn Fn(Id);
struct AudioObservers {
    _interruption_token: Id,
    _route_token: Id,
    _interruption: block2::RcBlock<NotificationHandler>,
    _route: block2::RcBlock<NotificationHandler>,
}

thread_local! {
    static AUDIO_OBSERVERS: RefCell<Option<AudioObservers>> = const { RefCell::new(None) };
}

unsafe fn notification_uint(notification: Id, key: &str) -> Option<usize> {
    if notification.is_null() {
        return None;
    }
    let user_info = msg!(notification, "userInfo", () -> Id);
    if user_info.is_null() {
        return None;
    }
    let value = msg!(user_info, "objectForKey:", (string(key) => Id) -> Id);
    if value.is_null() {
        None
    } else {
        Some(msg!(value, "unsignedIntegerValue", () -> usize))
    }
}

unsafe fn add_notification_observer(
    name: &str,
    handler: &block2::RcBlock<NotificationHandler>,
) -> Id {
    let center = msg!(class(c"NSNotificationCenter"), "defaultCenter", () -> Id);
    let token = msg!(center, "addObserverForName:object:queue:usingBlock:",
        (string(name) => Id, ptr::null_mut::<c_void>() => Id, ptr::null_mut::<c_void>() => Id,
         block2::RcBlock::as_ptr(handler) => *mut block2::Block<NotificationHandler>) -> Id);
    if !token.is_null() {
        msg!(token, "retain", () -> Id);
    }
    token
}

unsafe fn ensure_audio_observers() {
    if !main_thread() {
        return;
    }
    AUDIO_OBSERVERS.with(|slot| {
        if slot.borrow().is_some() {
            return;
        }
        let interruption: block2::RcBlock<NotificationHandler> =
            block2::RcBlock::new(|notification: Id| unsafe {
                match notification_uint(notification, "AVAudioSessionInterruptionTypeKey") {
                    Some(1) => push(Event::AudioInterrupted),
                    Some(0)
                        if notification_uint(
                            notification,
                            "AVAudioSessionInterruptionOptionKey",
                        )
                        .is_some_and(|options| options & 1 != 0) =>
                    {
                        push(Event::AudioResume)
                    }
                    _ => {}
                }
            });
        let route: block2::RcBlock<NotificationHandler> =
            block2::RcBlock::new(|_notification: Id| push(Event::AudioRouteChanged));
        let interruption_token = unsafe {
            add_notification_observer("AVAudioSessionInterruptionNotification", &interruption)
        };
        let route_token =
            unsafe { add_notification_observer("AVAudioSessionRouteChangeNotification", &route) };
        if !interruption_token.is_null() && !route_token.is_null() {
            *slot.borrow_mut() = Some(AudioObservers {
                _interruption_token: interruption_token,
                _route_token: route_token,
                _interruption: interruption,
                _route: route,
            });
        } else {
            let center = unsafe { msg!(class(c"NSNotificationCenter"), "defaultCenter", () -> Id) };
            for token in [interruption_token, route_token] {
                if !token.is_null() {
                    unsafe {
                        msg!(center, "removeObserver:", (token => Id) -> ());
                        msg!(token, "release", () -> ());
                    }
                }
            }
        }
    });
}

pub fn audio_active(active: bool) -> Result<(), String> {
    unsafe {
        if !main_thread() {
            return Err("Audio session must be configured on the main thread".into());
        }
        ensure_audio_observers();
        let session = msg!(class(c"AVAudioSession"), "sharedInstance", () -> Id);
        let mut error: Id = ptr::null_mut();
        if active
            && !msg!(session, "setCategory:error:", (string("AVAudioSessionCategoryAmbient") => Id, &mut error as *mut Id => *mut Id) -> bool)
        {
            return Err(text(msg!(error, "localizedDescription", () -> Id)));
        }
        if !msg!(session, "setActive:error:", (active => bool, &mut error as *mut Id => *mut Id) -> bool)
        {
            return Err(text(msg!(error, "localizedDescription", () -> Id)));
        }
        Ok(())
    }
}

pub struct BackgroundTask {
    id: Arc<AtomicUsize>,
}
impl BackgroundTask {
    pub fn begin() -> Self {
        unsafe {
            let id = Arc::new(AtomicUsize::new(usize::MAX));
            let expired = id.clone();
            let block = block2::RcBlock::new(move || {
                end_task(&expired);
            });
            let value = msg!(app(), "beginBackgroundTaskWithName:expirationHandler:", (string("Horizon world checkpoint") => Id, block2::RcBlock::as_ptr(&block) => *mut block2::Block<dyn Fn()>) -> usize);
            id.store(value, Ordering::SeqCst);
            Self { id }
        }
    }
}
fn end_task(id: &AtomicUsize) {
    let value = id.swap(usize::MAX, Ordering::SeqCst);
    if value != usize::MAX {
        unsafe {
            msg!(app(), "endBackgroundTask:", (value => usize) -> ());
        }
    }
}
impl Drop for BackgroundTask {
    fn drop(&mut self) {
        end_task(&self.id);
    }
}

#[derive(Debug, Clone)]
pub struct Metrics {
    pub resident_bytes: Option<u64>,
    pub thermal_state: i64,
    pub system_version: String,
}
pub fn metrics() -> Metrics {
    unsafe {
        let mut data = [0u32; 64];
        let mut count = data.len() as u32;
        let ok = task_info(mach_task_self_, 20, data.as_mut_ptr(), &mut count) == 0 && count >= 4;
        let process = msg!(class(c"NSProcessInfo"), "processInfo", () -> Id);
        let device = msg!(class(c"UIDevice"), "currentDevice", () -> Id);
        Metrics {
            resident_bytes: ok.then(|| u64::from(data[2]) | (u64::from(data[3]) << 32)),
            thermal_state: msg!(process, "thermalState", () -> isize) as i64,
            system_version: text(msg!(device, "systemVersion", () -> Id)),
        }
    }
}
