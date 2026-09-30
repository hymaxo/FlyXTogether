//! XPLM "modern" windows: floating, pop-out-able and VR-capable.

use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_int, c_void};

use crate::guard;
use crate::sys;
use crate::util::to_cstring;

/// Mouse button of a click event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
}

/// Phase of a click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseStatus {
    Down,
    Drag,
    Up,
}

/// A keyboard event while the window has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    /// ASCII character, if the key produces one.
    pub ch: Option<char>,
    /// XPLM virtual key code (`XPLM_VK_*`).
    pub virtual_key: u8,
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
    /// Key went down (or auto-repeated); `false` means released.
    pub down: bool,
}

/// Position and size of a window in its own coordinate space. For desktop
/// windows these are global desktop boxels; for VR windows the bottom-left
/// corner is (0, 0).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geometry {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Geometry {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.top - self.bottom
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y > self.bottom && y <= self.top
    }
}

/// Handle passed to delegate callbacks.
pub struct WindowContext {
    id: sys::XPLMWindowID,
    focus_request: Cell<Option<bool>>,
}

impl WindowContext {
    pub fn geometry(&self) -> Geometry {
        window_geometry(self.id)
    }

    pub fn is_in_vr(&self) -> bool {
        unsafe { sys::XPLMWindowIsInVR(self.id) != 0 }
    }

    /// Mouse position in window coordinates, for desktop windows. In VR the
    /// mouse is reported through [`WindowDelegate::cursor`] only.
    pub fn mouse_location(&self) -> Option<(i32, i32)> {
        if self.is_in_vr() {
            return None;
        }
        let (mut x, mut y) = (0, 0);
        unsafe { sys::XPLMGetMouseLocationGlobal(&mut x, &mut y) };
        Some((x, y))
    }

    pub fn has_keyboard_focus(&self) -> bool {
        unsafe { sys::XPLMHasKeyboardFocus(self.id) != 0 }
    }

    /// Asks for keyboard focus to be taken (`true`) or handed back to
    /// X-Plane (`false`). Applied after the callback returns, because XPLM
    /// may call back into the window while switching focus.
    pub fn request_keyboard_focus(&self, focus: bool) {
        self.focus_request.set(Some(focus));
    }
}

/// Receives a window's callbacks. Coordinates are in the window's space
/// (see [`Geometry`]).
pub trait WindowDelegate {
    fn draw(&mut self, ctx: &WindowContext);

    /// Returns whether the click was consumed.
    fn mouse(
        &mut self,
        _ctx: &WindowContext,
        _x: i32,
        _y: i32,
        _button: MouseButton,
        _status: MouseStatus,
    ) -> bool {
        true
    }

    fn cursor(&mut self, _ctx: &WindowContext, _x: i32, _y: i32) {}

    /// `axis` 0 is vertical, 1 horizontal. Returns whether it was consumed.
    fn wheel(&mut self, _ctx: &WindowContext, _x: i32, _y: i32, _axis: i32, _clicks: i32) -> bool {
        true
    }

    fn key(&mut self, _ctx: &WindowContext, _event: KeyEvent) {}

    /// Keyboard focus moved elsewhere (another window or the simulator).
    fn focus_lost(&mut self, _ctx: &WindowContext) {}
}

type DelegateCell = RefCell<Box<dyn WindowDelegate>>;

/// An owned XPLM window. Destroyed when dropped. Main thread only.
pub struct Window {
    id: sys::XPLMWindowID,
    _delegate: Box<DelegateCell>,
}

impl Window {
    /// Creates a floating, decorated window centred on the main screen.
    pub fn new(
        title: &str,
        width: i32,
        height: i32,
        visible: bool,
        delegate: impl WindowDelegate + 'static,
    ) -> Self {
        let delegate: Box<DelegateCell> = Box::new(RefCell::new(Box::new(delegate)));
        let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
        unsafe { sys::XPLMGetScreenBoundsGlobal(&mut l, &mut t, &mut r, &mut b) };
        let left = l + ((r - l) - width) / 2;
        let bottom = b + ((t - b) - height) / 2;
        let mut params = sys::XPLMCreateWindow_t {
            structSize: std::mem::size_of::<sys::XPLMCreateWindow_t>() as c_int,
            left,
            top: bottom + height,
            right: left + width,
            bottom,
            visible: visible as c_int,
            drawWindowFunc: Some(draw_cb),
            handleMouseClickFunc: Some(left_click_cb),
            handleKeyFunc: Some(key_cb),
            handleCursorFunc: Some(cursor_cb),
            handleMouseWheelFunc: Some(wheel_cb),
            refcon: delegate.as_ref() as *const DelegateCell as *mut c_void,
            decorateAsFloatingWindow: sys::xplm_WindowDecorationRoundRectangle as c_int,
            layer: sys::xplm_WindowLayerFloatingWindows as c_int,
            handleRightClickFunc: Some(right_click_cb),
        };
        let id = unsafe { sys::XPLMCreateWindowEx(&mut params) };
        let window = Self {
            id,
            _delegate: delegate,
        };
        window.set_title(title);
        unsafe {
            sys::XPLMSetWindowPositioningMode(id, sys::xplm_WindowPositionFree as c_int, -1);
        }
        window
    }

    pub fn set_title(&self, title: &str) {
        let title = to_cstring(title);
        unsafe { sys::XPLMSetWindowTitle(self.id, title.as_ptr()) }
    }

    pub fn set_resizing_limits(&self, min: (i32, i32), max: (i32, i32)) {
        unsafe { sys::XPLMSetWindowResizingLimits(self.id, min.0, min.1, max.0, max.1) }
    }

    pub fn is_visible(&self) -> bool {
        unsafe { sys::XPLMGetWindowIsVisible(self.id) != 0 }
    }

    pub fn set_visible(&self, visible: bool) {
        unsafe { sys::XPLMSetWindowIsVisible(self.id, visible as c_int) };
        if visible {
            unsafe { sys::XPLMBringWindowToFront(self.id) };
        }
    }

    pub fn is_in_vr(&self) -> bool {
        unsafe { sys::XPLMWindowIsInVR(self.id) != 0 }
    }

    /// Moves the window into VR (`true`) or back to a free desktop window.
    pub fn set_vr(&self, vr: bool) {
        let mode = if vr {
            sys::xplm_WindowVR
        } else {
            sys::xplm_WindowPositionFree
        };
        unsafe { sys::XPLMSetWindowPositioningMode(self.id, mode as c_int, -1) }
    }

    pub fn geometry(&self) -> Geometry {
        window_geometry(self.id)
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        unsafe {
            if sys::XPLMHasKeyboardFocus(self.id) != 0 {
                sys::XPLMTakeKeyboardFocus(std::ptr::null_mut());
            }
            sys::XPLMDestroyWindow(self.id);
        }
    }
}

fn window_geometry(id: sys::XPLMWindowID) -> Geometry {
    unsafe {
        if sys::XPLMWindowIsInVR(id) != 0 {
            let (mut w, mut h) = (0, 0);
            sys::XPLMGetWindowGeometryVR(id, &mut w, &mut h);
            Geometry {
                left: 0,
                top: h,
                right: w,
                bottom: 0,
            }
        } else {
            let (mut l, mut t, mut r, mut b) = (0, 0, 0, 0);
            sys::XPLMGetWindowGeometry(id, &mut l, &mut t, &mut r, &mut b);
            Geometry {
                left: l,
                top: t,
                right: r,
                bottom: b,
            }
        }
    }
}

/// Runs `f` with the delegate, then applies any keyboard-focus request.
/// Re-entrant callbacks (while the delegate is busy) are ignored.
fn with_delegate<R>(
    id: sys::XPLMWindowID,
    refcon: *mut c_void,
    fallback: R,
    f: impl FnOnce(&mut dyn WindowDelegate, &WindowContext) -> R,
) -> R {
    let cell = unsafe { &*(refcon as *const DelegateCell) };
    let ctx = WindowContext {
        id,
        focus_request: Cell::new(None),
    };
    let result = match cell.try_borrow_mut() {
        Ok(mut delegate) => f(delegate.as_mut(), &ctx),
        Err(_) => return fallback,
    };
    if let Some(focus) = ctx.focus_request.get() {
        let has = unsafe { sys::XPLMHasKeyboardFocus(id) != 0 };
        if focus && !has {
            unsafe { sys::XPLMTakeKeyboardFocus(id) };
        } else if !focus && has {
            unsafe { sys::XPLMTakeKeyboardFocus(std::ptr::null_mut()) };
        }
    }
    result
}

fn mouse_status(raw: sys::XPLMMouseStatus) -> MouseStatus {
    match raw as u32 {
        sys::xplm_MouseDown => MouseStatus::Down,
        sys::xplm_MouseDrag => MouseStatus::Drag,
        _ => MouseStatus::Up,
    }
}

// Windows keep working after a failure so the user can see what happened.

unsafe extern "C" fn draw_cb(id: sys::XPLMWindowID, refcon: *mut c_void) {
    guard::guard_always("window draw", (), || {
        with_delegate(id, refcon, (), |d, ctx| d.draw(ctx))
    })
}

unsafe extern "C" fn left_click_cb(
    id: sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    status: sys::XPLMMouseStatus,
    refcon: *mut c_void,
) -> c_int {
    guard::guard_always("window click", 1, || {
        with_delegate(id, refcon, 1, |d, ctx| {
            d.mouse(ctx, x, y, MouseButton::Left, mouse_status(status)) as c_int
        })
    })
}

unsafe extern "C" fn right_click_cb(
    id: sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    status: sys::XPLMMouseStatus,
    refcon: *mut c_void,
) -> c_int {
    guard::guard_always("window click", 1, || {
        with_delegate(id, refcon, 1, |d, ctx| {
            d.mouse(ctx, x, y, MouseButton::Right, mouse_status(status)) as c_int
        })
    })
}

unsafe extern "C" fn cursor_cb(
    id: sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    refcon: *mut c_void,
) -> sys::XPLMCursorStatus {
    guard::guard_always("window cursor", 0, || {
        with_delegate(id, refcon, (), |d, ctx| d.cursor(ctx, x, y));
        sys::xplm_CursorDefault as sys::XPLMCursorStatus
    })
}

unsafe extern "C" fn wheel_cb(
    id: sys::XPLMWindowID,
    x: c_int,
    y: c_int,
    wheel: c_int,
    clicks: c_int,
    refcon: *mut c_void,
) -> c_int {
    guard::guard_always("window wheel", 1, || {
        with_delegate(id, refcon, 1, |d, ctx| {
            d.wheel(ctx, x, y, wheel, clicks) as c_int
        })
    })
}

unsafe extern "C" fn key_cb(
    id: sys::XPLMWindowID,
    key: c_char,
    flags: sys::XPLMKeyFlags,
    virtual_key: c_char,
    refcon: *mut c_void,
    losing_focus: c_int,
) {
    guard::guard_always("window key", (), || {
        with_delegate(id, refcon, (), |d, ctx| {
            if losing_focus != 0 {
                d.focus_lost(ctx);
            } else {
                d.key(ctx, decode_key(key, flags, virtual_key));
            }
        })
    })
}

fn decode_key(key: c_char, flags: sys::XPLMKeyFlags, virtual_key: c_char) -> KeyEvent {
    let flags = flags as u32;
    let byte = key as u8;
    KeyEvent {
        ch: (0x20..0x7f).contains(&byte).then_some(byte as char),
        virtual_key: virtual_key as u8,
        shift: flags & sys::xplm_ShiftFlag != 0,
        alt: flags & sys::xplm_OptionAltFlag != 0,
        control: flags & sys::xplm_ControlFlag != 0,
        down: flags & sys::xplm_UpFlag == 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_printable_key_down() {
        let e = decode_key(b'a' as c_char, sys::xplm_DownFlag as c_int, 65);
        assert_eq!(e.ch, Some('a'));
        assert!(e.down && !e.shift && !e.control);
    }

    #[test]
    fn decode_control_and_non_printable() {
        let flags = (sys::xplm_DownFlag | sys::xplm_ControlFlag) as c_int;
        let e = decode_key(0x16, flags, sys::XPLM_VK_V as c_char);
        assert_eq!(e.ch, None);
        assert!(e.control);
        assert_eq!(e.virtual_key, sys::XPLM_VK_V as u8);
        // Virtual keys above 127 arrive as negative chars.
        let e = decode_key(
            0,
            sys::xplm_DownFlag as c_int,
            sys::XPLM_VK_ENTER as u8 as c_char,
        );
        assert_eq!(e.virtual_key, sys::XPLM_VK_ENTER as u8);
    }

    #[test]
    fn decode_key_up() {
        let e = decode_key(b'a' as c_char, sys::xplm_UpFlag as c_int, 65);
        assert!(!e.down);
    }

    #[test]
    fn geometry_contains_is_half_open() {
        let g = Geometry {
            left: 10,
            top: 110,
            right: 210,
            bottom: 10,
        };
        assert_eq!((g.width(), g.height()), (200, 100));
        assert!(g.contains(10, 110));
        assert!(!g.contains(210, 50));
        assert!(!g.contains(50, 10));
    }
}
