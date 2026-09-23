//! Window creation, message pump and event translation.
//!
//! The window proc does not run application logic. It only converts messages
//! into platform::Event values and pushes them into a queue owned by the
//! window; the application drains the queue once per frame. This keeps the
//! reentrancy surface small: Windows can call the proc from inside
//! DispatchMessageW at any time, and the only shared state is the queue.

use std::cell::RefCell;
use std::ffi::c_void;
use std::sync::OnceLock;

use super::ffi::*;
use super::{from_wide, wide};
use crate::core::{Error, Result};
use crate::platform::input::{Event, Key, Modifiers, MouseButton};

const CLASS_NAME: &str = "RXScopeMainWindow";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorKind {
    Arrow,
    Text,
    ResizeHorizontal,
    ResizeVertical,
    Hand,
}

#[derive(Debug, Clone)]
pub struct WindowConfig {
    pub title: String,
    /// Client area size in logical units, scaled by DPI at creation time.
    pub width: u32,
    pub height: u32,
    /// Position in screen pixels, None centers on the primary display.
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub min_width: u32,
    pub min_height: u32,
    pub maximized: bool,
    /// Replaces the system caption and border with one the application draws.
    pub custom_frame: bool,
}

impl Default for WindowConfig {
    fn default() -> Self {
        WindowConfig {
            title: "RXScope".to_string(),
            width: 1280,
            height: 800,
            x: None,
            y: None,
            min_width: 800,
            min_height: 500,
            custom_frame: true,
            maximized: false,
        }
    }
}

/// State shared between the window proc and the owner. Single threaded: the
/// proc always runs on the thread that created the window.
struct WindowState {
    events: Vec<Event>,
    width: u32,
    height: u32,
    dpi_scale: f32,
    min_width: u32,
    min_height: u32,
    mouse_x: f32,
    mouse_y: f32,
    tracking_mouse: bool,
    capture_count: u32,
    /// Pending UTF-16 high surrogate from a previous WM_CHAR.
    high_surrogate: u16,
    minimized: bool,
    /// Live copy of the frame setting. Held here because the window proc has to
    /// answer WM_NCCALCSIZE, which arrives long before the application has a
    /// chance to consult its own configuration.
    custom_frame: bool,
    cursor: HCURSOR,
    quit: bool,
}

pub struct Window {
    hwnd: HWND,
    hinstance: HINSTANCE,
    /// Boxed so the address handed to Windows stays valid on moves.
    state: Box<RefCell<WindowState>>,
    cursors: [HCURSOR; 5],
}

impl Window {
    pub fn new(cfg: &WindowConfig) -> Result<Window> {
        enable_per_monitor_dpi();

        let hinstance = unsafe { GetModuleHandleW(std::ptr::null()) };
        register_class(hinstance)?;

        let class = wide(CLASS_NAME);
        let title = wide(&cfg.title);

        let state = Box::new(RefCell::new(WindowState {
            events: Vec::with_capacity(64),
            width: cfg.width,
            height: cfg.height,
            dpi_scale: 1.0,
            min_width: cfg.min_width,
            min_height: cfg.min_height,
            mouse_x: 0.0,
            mouse_y: 0.0,
            tracking_mouse: false,
            capture_count: 0,
            high_surrogate: 0,
            minimized: false,
            custom_frame: cfg.custom_frame,
            cursor: unsafe { LoadCursorW(std::ptr::null_mut(), IDC_ARROW) },
            quit: false,
        }));
        let state_ptr = &*state as *const RefCell<WindowState> as *mut c_void;

        let style = WS_OVERLAPPEDWINDOW;
        let ex_style = WS_EX_APPWINDOW;
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: cfg.width as i32,
            bottom: cfg.height as i32,
        };
        // A frameless window has no non-client area, so the requested client
        // size is already the window size and adding a frame would enlarge it.
        if !cfg.custom_frame {
            unsafe { AdjustWindowRectEx(&mut rect, style, 0, ex_style) };
        }

        let (x, y) = match (cfg.x, cfg.y) {
            (Some(x), Some(y)) => (x, y),
            _ => (CW_USEDEFAULT, CW_USEDEFAULT),
        };

        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class.as_ptr(),
                title.as_ptr(),
                style,
                x,
                y,
                rect.width(),
                rect.height(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                hinstance,
                state_ptr,
            )
        };
        if hwnd.is_null() {
            return Err(Error::with_code(
                crate::core::error::Category::Platform,
                "CreateWindowExW failed",
                super::last_error() as i64,
            ));
        }

        let cursors = unsafe {
            [
                LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                LoadCursorW(std::ptr::null_mut(), IDC_IBEAM),
                LoadCursorW(std::ptr::null_mut(), IDC_SIZEWE),
                LoadCursorW(std::ptr::null_mut(), IDC_SIZENS),
                LoadCursorW(std::ptr::null_mut(), IDC_HAND),
            ]
        };

        let window = Window { hwnd, hinstance, state, cursors };

        // Query the real DPI now that the window has a monitor assigned.
        let dpi = dpi_for_window(hwnd);
        {
            let mut s = window.state.borrow_mut();
            s.dpi_scale = dpi as f32 / USER_DEFAULT_SCREEN_DPI as f32;
        }

        // The frame metrics are computed once at creation and cached, by the
        // compositor as well as by the window manager, and the compositor draws
        // the caption into a surface of its own. The handler that removes the
        // frame therefore answers a calculation that has already happened, so
        // the caption survives until one is requested. Requesting it here is
        // what makes the frame correct on the first frame rather than on the
        // first time the setting is touched.
        if cfg.custom_frame {
            unsafe {
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                );
            }
        }

        unsafe {
            ShowWindow(hwnd, if cfg.maximized { SW_MAXIMIZE } else { SW_SHOWNORMAL });
        }

        // Guarantee that the consumer sees an initial size event even if
        // Windows did not send WM_SIZE before ShowWindow returned.
        let (w, h) = window.query_client_size();
        {
            let mut s = window.state.borrow_mut();
            s.width = w;
            s.height = h;
            s.events.push(Event::Resized { width: w, height: h });
            s.events.push(Event::DpiChanged { scale: dpi as f32 / USER_DEFAULT_SCREEN_DPI as f32 });
        }

        crate::log_info!(
            "window",
            "created {}x{} dpi {} scale {:.2}",
            w,
            h,
            dpi,
            dpi as f32 / USER_DEFAULT_SCREEN_DPI as f32
        );
        Ok(window)
    }

    /// Native handles for the Vulkan surface.
    pub fn hwnd(&self) -> *mut c_void {
        self.hwnd
    }
    pub fn hinstance(&self) -> *mut c_void {
        self.hinstance
    }

    pub fn client_size(&self) -> (u32, u32) {
        let s = self.state.borrow();
        (s.width, s.height)
    }

    pub fn dpi_scale(&self) -> f32 {
        self.state.borrow().dpi_scale
    }

    pub fn is_minimized(&self) -> bool {
        self.state.borrow().minimized
    }

    pub fn set_title(&self, title: &str) {
        let w = wide(title);
        unsafe { SetWindowTextW(self.hwnd, w.as_ptr()) };
    }

    pub fn set_cursor(&self, kind: CursorKind) {
        let h = self.cursors[kind as usize];
        let mut s = self.state.borrow_mut();
        if s.cursor != h {
            s.cursor = h;
            unsafe { SetCursor(h) };
        }
    }

    pub fn custom_frame(&self) -> bool {
        self.state.borrow().custom_frame
    }

    /// Switches the frame at run time.
    ///
    /// The frame is recomputed lazily, so the change only becomes visible after
    /// the window is told its frame changed. That call reenters the proc with
    /// WM_NCCALCSIZE, hence the borrow is released first.
    pub fn set_custom_frame(&self, on: bool) {
        {
            let mut s = self.state.borrow_mut();
            if s.custom_frame == on {
                return;
            }
            s.custom_frame = on;
        }
        unsafe {
            SetWindowPos(
                self.hwnd,
                std::ptr::null_mut(),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
        }
    }

    pub fn is_maximized(&self) -> bool {
        unsafe { IsZoomed(self.hwnd) != 0 }
    }

    /// Hands the pointer to the system move loop.
    ///
    /// The caption is client area under a custom frame, so the move cannot be
    /// started by the default handler. Releasing the capture first is required:
    /// the loop takes its own, and a capture already held would make it exit at
    /// once.
    pub fn begin_move(&self) {
        unsafe {
            ReleaseCapture();
            SendMessageW(self.hwnd, WM_NCLBUTTONDOWN, HTCAPTION as WPARAM, 0);
        }
    }

    pub fn toggle_maximize(&self) {
        unsafe {
            let cmd = if IsZoomed(self.hwnd) != 0 { SW_RESTORE } else { SW_MAXIMIZE };
            ShowWindow(self.hwnd, cmd);
        }
    }

    pub fn minimize(&self) {
        unsafe { ShowWindow(self.hwnd, SW_MINIMIZE) };
    }

    pub fn close(&self) {
        unsafe { PostMessageW(self.hwnd, WM_CLOSE, 0, 0) };
    }

    /// Outer window rectangle in screen coordinates, used to persist geometry.
    pub fn outer_rect(&self) -> (i32, i32, u32, u32) {
        let mut r = RECT::default();
        unsafe { GetWindowRect(self.hwnd, &mut r) };
        (r.left, r.top, r.width().max(0) as u32, r.height().max(0) as u32)
    }

    fn query_client_size(&self) -> (u32, u32) {
        let mut r = RECT::default();
        unsafe { GetClientRect(self.hwnd, &mut r) };
        (r.width().max(0) as u32, r.height().max(0) as u32)
    }

    /// Drains the OS queue and moves translated events into out.
    /// Returns false when a quit message was received.
    pub fn pump(&self, out: &mut Vec<Event>) -> bool {
        let mut msg = MSG::default();
        unsafe {
            while PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                if msg.message == WM_QUIT {
                    self.state.borrow_mut().quit = true;
                    break;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        // Borrow only after the pump finished, never across DispatchMessageW.
        let mut s = self.state.borrow_mut();
        out.append(&mut s.events);
        !s.quit
    }

    pub fn request_close(&self) {
        unsafe { PostQuitMessage(0) };
    }

    /// Puts text on the clipboard.
    ///
    /// Returns false when the clipboard is held by another application, which
    /// is ordinary and transient: it is opened for the duration of one paste.
    /// Retrying is left to the operator, because a loop here would block the
    /// frame on another process for as long as that process cared to hold it.
    pub fn set_clipboard_text(&self, text: &str) -> bool {
        if text.is_empty() {
            return false;
        }
        let wide = wide(text);
        let bytes = wide.len() * std::mem::size_of::<u16>();

        unsafe {
            if OpenClipboard(self.hwnd) == 0 {
                crate::log_warn!("app", "the clipboard is held by another application");
                return false;
            }

            let handle = GlobalAlloc(GMEM_MOVEABLE, bytes);
            if handle.is_null() {
                CloseClipboard();
                return false;
            }
            let target = GlobalLock(handle);
            if target.is_null() {
                GlobalFree(handle);
                CloseClipboard();
                return false;
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, target as *mut u8, bytes);
            GlobalUnlock(handle);

            EmptyClipboard();
            let placed = SetClipboardData(CF_UNICODETEXT, handle);
            CloseClipboard();

            // The system owns the block once it has been accepted, so releasing
            // it here would release memory the next paste is about to read.
            // Only the refusal path is ours to clean up.
            if placed.is_null() {
                GlobalFree(handle);
                return false;
            }
        }
        true
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        if !self.hwnd.is_null() {
            // Detach the state pointer first so a late message cannot touch
            // freed memory during teardown.
            unsafe {
                SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0);
                DestroyWindow(self.hwnd);
            }
            self.hwnd = std::ptr::null_mut();
        }
    }
}

/// Loads the embedded icon at the size the system asks for.
///
/// LoadIconW is not used because it only ever returns the large metric, and the
/// class carries two slots for a reason: a thirty two pixel image scaled down to
/// sixteen loses the strokes that distinguish one application from another in a
/// row of taskbar buttons.
///
/// The handle is never released. The class outlives every window and is
/// destroyed with the process, so freeing it would mean freeing it at a point
/// where nothing can observe either outcome.
fn load_app_icon(hinstance: HINSTANCE, small: bool) -> HICON {
    unsafe {
        let (cx, cy) = if small {
            (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON))
        } else {
            (GetSystemMetrics(SM_CXICON), GetSystemMetrics(SM_CYICON))
        };
        let icon = LoadImageW(
            hinstance,
            ICON_RESOURCE_ID,
            IMAGE_ICON,
            cx,
            cy,
            LR_DEFAULTCOLOR,
        ) as HICON;
        if icon.is_null() {
            // No resource, which is the case for a build made without the icon
            // file. The system default is a worse picture and not a fault.
            LoadIconW(std::ptr::null_mut(), IDI_APPLICATION)
        } else {
            icon
        }
    }
}

fn register_class(hinstance: HINSTANCE) -> Result<()> {
    static ATOM_ONCE: OnceLock<u16> = OnceLock::new();
    let mut failed = 0u32;
    ATOM_ONCE.get_or_init(|| {
        let class = wide(CLASS_NAME);
        // hbrBackground is null: the Vulkan swapchain owns every pixel and a
        // background brush would flash on resize.
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as UINT,
            style: CS_HREDRAW | CS_VREDRAW | CS_DBLCLKS | CS_OWNDC,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            //hIcon: unsafe { LoadIconW(std::ptr::null_mut(), IDI_APPLICATION) },
            hIcon: load_app_icon(hinstance, false),
            hCursor: std::ptr::null_mut(),
            hbrBackground: std::ptr::null_mut(),
            lpszMenuName: std::ptr::null(),
            lpszClassName: class.as_ptr(),
            //hIconSm: std::ptr::null_mut(),
            hIconSm: load_app_icon(hinstance, true),
        };
        let atom = unsafe { RegisterClassExW(&wc) };
        if atom == 0 {
            failed = super::last_error();
        }
        atom
    });
    if failed != 0 {
        return Err(Error::with_code(
            crate::core::error::Category::Platform,
            "RegisterClassExW failed",
            failed as i64,
        ));
    }
    Ok(())
}

#[inline]
fn modifiers() -> Modifiers {
    // GetKeyState reflects the state at the time the current message was
    // generated, which is what the event consumer expects.
    unsafe {
        Modifiers {
            shift: (GetKeyState(VK_SHIFT as i32) as u16 & 0x8000) != 0,
            ctrl: (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0,
            alt: (GetKeyState(VK_MENU as i32) as u16 & 0x8000) != 0,
        }
    }
}

fn map_key(vk: u32) -> Key {
    match vk {
        VK_ESCAPE => Key::Escape,
        VK_RETURN => Key::Enter,
        VK_TAB => Key::Tab,
        VK_BACK => Key::Backspace,
        VK_DELETE => Key::Delete,
        VK_INSERT => Key::Insert,
        VK_HOME => Key::Home,
        VK_END => Key::End,
        VK_PRIOR => Key::PageUp,
        VK_NEXT => Key::PageDown,
        VK_LEFT => Key::Left,
        VK_RIGHT => Key::Right,
        VK_UP => Key::Up,
        VK_DOWN => Key::Down,
        VK_SPACE => Key::Space,
        VK_SHIFT => Key::Shift,
        VK_CONTROL => Key::Ctrl,
        VK_MENU => Key::Alt,
        0x30..=0x39 => Key::Digit((vk - 0x30) as u8),
        0x41..=0x5A => Key::Letter(vk as u8),
        v if (VK_F1..VK_F1 + 24).contains(&v) => Key::F((v - VK_F1 + 1) as u8),
        other => Key::Unknown(other),
    }
}

/// Capture bookkeeping deferred until the state borrow is released.
/// SetCapture and ReleaseCapture send WM_CAPTURECHANGED synchronously, so
/// calling them while the RefCell is borrowed would make the nested
/// invocation fail its try_borrow_mut and drop the message.
enum CaptureAction {
    None,
    Acquire,
    Release,
}

unsafe extern "system" fn wnd_proc(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    // The state pointer is installed on WM_NCCREATE, which is the first
    // message a window receives.
    if msg == WM_NCCREATE {
        let cs = lparam as *const CREATESTRUCTW;
        if !cs.is_null() {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, (*cs).lpCreateParams as LONG_PTR);
        }
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }

    let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<WindowState>;
    if ptr.is_null() {
        return DefWindowProcW(hwnd, msg, wparam, lparam);
    }
    let cell = &*ptr;

    // Nested messages can still arrive in paths that were not converted to
    // the deferred pattern. Dropping such an event is better than panicking
    // on a double borrow.
    let mut state = match cell.try_borrow_mut() {
        Ok(s) => s,
        Err(_) => return DefWindowProcW(hwnd, msg, wparam, lparam),
    };

    match msg {
        WM_NCCALCSIZE => {
            // Returning the proposed rectangle unchanged makes the client area
            // cover the whole window, which removes the caption and the border
            // in one step. The maximized case is corrected in WM_GETMINMAXINFO,
            // where the bounds are clamped to the work area so the window does
            // not cover the taskbar.
            if !state.custom_frame || wparam == 0 {
                drop(state);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            drop(state);
            return 0;
        }

        WM_NCHITTEST => {
            // The whole window is client area, so every resize edge has to be
            // reported here. Corners are tested first: a corner is inside two
            // edge bands at once and the diagonal is the one an operator meant.
            if !state.custom_frame {
                drop(state);
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            let grab = (RESIZE_GRAB * state.dpi_scale).round().max(2.0) as i32;
            drop(state);

            if IsZoomed(hwnd) != 0 {
                return HTCLIENT;
            }
            let mut r = RECT::default();
            GetWindowRect(hwnd, &mut r);
            let x = lparam_x(lparam);
            let y = lparam_y(lparam);

            let left = x < r.left + grab;
            let right = x >= r.right - grab;
            let top = y < r.top + grab;
            let bottom = y >= r.bottom - grab;

            return match (left, right, top, bottom) {
                (true, _, true, _) => HTTOPLEFT,
                (_, true, true, _) => HTTOPRIGHT,
                (true, _, _, true) => HTBOTTOMLEFT,
                (_, true, _, true) => HTBOTTOMRIGHT,
                (true, _, _, _) => HTLEFT,
                (_, true, _, _) => HTRIGHT,
                (_, _, true, _) => HTTOP,
                (_, _, _, true) => HTBOTTOM,
                _ => HTCLIENT,
            };
        }

        WM_NCACTIVATE => {
            // A negative region argument suppresses the non-client repaint the
            // default handler would otherwise perform on every focus change.
            let custom = state.custom_frame;
            drop(state);
            if custom {
                return DefWindowProcW(hwnd, msg, wparam, -1);
            }
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_ERASEBKGND => {
            // Claim the erase so Windows does not paint over the swapchain.
            return 1;
        }

        WM_PAINT => {
            // Rendering is driven by the main loop, not by paint messages.
            // The region still has to be validated, DefWindowProc does it.
            drop(state);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_SIZE => {
            let w = loword(lparam as usize) as u32;
            let h = hiword(lparam as usize) as u32;
            match wparam {
                SIZE_MINIMIZED => {
                    if !state.minimized {
                        state.minimized = true;
                        state.events.push(Event::Minimized(true));
                    }
                }
                _ => {
                    if state.minimized {
                        state.minimized = false;
                        state.events.push(Event::Minimized(false));
                    }
                    if w != state.width || h != state.height {
                        state.width = w;
                        state.height = h;
                        state.events.push(Event::Resized { width: w, height: h });
                    }
                }
            }
            return 0;
        }

        WM_GETMINMAXINFO => {
            // Enforce a usable minimum so the layout engine never receives a
            // degenerate viewport. Reads are copied out first to keep the
            // borrow checker happy and the intent obvious.
            let scale = state.dpi_scale.max(0.5);
            let min_w = state.min_width;
            let min_h = state.min_height;
            let custom = state.custom_frame;
            drop(state);

            let mmi = lparam as *mut MINMAXINFO;
            if !mmi.is_null() {
                let mut r = RECT {
                    left: 0,
                    top: 0,
                    right: (min_w as f32 * scale) as i32,
                    bottom: (min_h as f32 * scale) as i32,
                };
                if !custom {
                    AdjustWindowRectEx(&mut r, WS_OVERLAPPEDWINDOW, 0, WS_EX_APPWINDOW);
                }
                (*mmi).ptMinTrackSize = POINT { x: r.width(), y: r.height() };

                // A window with no non-client area is maximized to the full
                // monitor rectangle by default, which covers the taskbar. The
                // work area is the same bound the default frame would apply.
                if custom {
                    let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
                    let mut mi = MONITORINFO {
                        cbSize: std::mem::size_of::<MONITORINFO>() as DWORD,
                        ..Default::default()
                    };
                    if !monitor.is_null() && GetMonitorInfoW(monitor, &mut mi) != 0 {
                        (*mmi).ptMaxPosition = POINT {
                            x: mi.rcWork.left - mi.rcMonitor.left,
                            y: mi.rcWork.top - mi.rcMonitor.top,
                        };
                        (*mmi).ptMaxSize =
                            POINT { x: mi.rcWork.width(), y: mi.rcWork.height() };
                    }
                }
            }
            return 0;
        }

        WM_ENTERSIZEMOVE => {
            state.events.push(Event::ModalResize(true));
            return 0;
        }
        WM_EXITSIZEMOVE => {
            state.events.push(Event::ModalResize(false));
            return 0;
        }

        WM_DPICHANGED => {
            let dpi = loword(wparam) as u32;
            let scale = dpi as f32 / USER_DEFAULT_SCREEN_DPI as f32;
            state.dpi_scale = scale;
            state.events.push(Event::DpiChanged { scale });

            // Windows suggests a new rectangle that keeps the physical size.
            // RECT is Copy, so the value is taken before the borrow is
            // released and the pointer becomes uninteresting.
            let suggested = {
                let r = lparam as *const RECT;
                if r.is_null() { None } else { Some(*r) }
            };
            drop(state);

            if let Some(r) = suggested {
                // Reenters this proc with WM_SIZE, which now finds the cell
                // free and can record the new client size.
                SetWindowPos(
                    hwnd,
                    std::ptr::null_mut(),
                    r.left,
                    r.top,
                    r.width(),
                    r.height(),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
            return 0;
        }

        WM_SETFOCUS => {
            state.events.push(Event::Focus(true));
            return 0;
        }
        WM_KILLFOCUS => {
            state.events.push(Event::Focus(false));
            return 0;
        }

        WM_SETCURSOR => {
            if (lparam & 0xFFFF) == HTCLIENT {
                let cursor = state.cursor;
                drop(state);
                SetCursor(cursor);
                return 1;
            }
            drop(state);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }

        WM_MOUSEMOVE => {
            let x = lparam_x(lparam) as f32;
            let y = lparam_y(lparam) as f32;
            state.mouse_x = x;
            state.mouse_y = y;
            let mods = modifiers();
            state.events.push(Event::MouseMove { x, y, mods });

            // WM_MOUSELEAVE is only delivered after an explicit request, and
            // the request is one shot. TrackMouseEvent posts, never sends, so
            // it cannot reenter, but the borrow is released anyway for
            // uniformity.
            let need_tracking = !state.tracking_mouse;
            if need_tracking {
                state.tracking_mouse = true;
            }
            drop(state);
            if need_tracking {
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as DWORD,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                TrackMouseEvent(&mut tme);
            }
            return 0;
        }

        WM_MOUSELEAVE => {
            state.tracking_mouse = false;
            state.events.push(Event::MouseLeave);
            return 0;
        }

        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN | WM_LBUTTONUP
        | WM_RBUTTONUP | WM_MBUTTONUP | WM_XBUTTONUP => {
            let pressed = matches!(
                msg,
                WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN
            );
            let button = match msg {
                WM_LBUTTONDOWN | WM_LBUTTONUP => MouseButton::Left,
                WM_RBUTTONDOWN | WM_RBUTTONUP => MouseButton::Right,
                WM_MBUTTONDOWN | WM_MBUTTONUP => MouseButton::Middle,
                _ => {
                    if hiword(wparam) == 1 {
                        MouseButton::X1
                    } else {
                        MouseButton::X2
                    }
                }
            };
            let x = lparam_x(lparam) as f32;
            let y = lparam_y(lparam) as f32;
            state.mouse_x = x;
            state.mouse_y = y;

            // Capture keeps drag operations alive outside the client area,
            // which matters for splitters and waterfall band edges. The
            // counter tracks chorded presses so one extra button does not
            // release the capture early.
            let action = if pressed {
                let first = state.capture_count == 0;
                state.capture_count += 1;
                if first { CaptureAction::Acquire } else { CaptureAction::None }
            } else if state.capture_count > 0 {
                state.capture_count -= 1;
                if state.capture_count == 0 { CaptureAction::Release } else { CaptureAction::None }
            } else {
                CaptureAction::None
            };

            let mods = modifiers();
            state.events.push(Event::MouseButton { button, pressed, x, y, mods });
            drop(state);

            match action {
                CaptureAction::Acquire => {
                    SetCapture(hwnd);
                }
                CaptureAction::Release => {
                    ReleaseCapture();
                }
                CaptureAction::None => {}
            }
            return 0;
        }

        WM_LBUTTONDBLCLK | WM_RBUTTONDBLCLK => {
            let button = if msg == WM_LBUTTONDBLCLK { MouseButton::Left } else { MouseButton::Right };
            state.events.push(Event::MouseDoubleClick {
                button,
                x: lparam_x(lparam) as f32,
                y: lparam_y(lparam) as f32,
            });
            return 0;
        }

        WM_MOUSEWHEEL | WM_MOUSEHWHEEL => {
            // Wheel coordinates are in screen space, unlike every other
            // mouse message.
            let mut pt = POINT { x: lparam_x(lparam), y: lparam_y(lparam) };
            ScreenToClient(hwnd, &mut pt);
            let raw = hiword(wparam) as i16 as f32 / WHEEL_DELTA;
            let (dy, dx) = if msg == WM_MOUSEWHEEL { (raw, 0.0) } else { (0.0, raw) };
            let mods = modifiers();
            state.events.push(Event::MouseWheel {
                delta_y: dy,
                delta_x: dx,
                x: pt.x as f32,
                y: pt.y as f32,
                mods,
            });
            return 0;
        }

        WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP => {
            let pressed = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
            // Bit 30 of lParam is the previous key state.
            let repeat = pressed && (lparam & (1 << 30)) != 0;
            let mods = modifiers();
            state.events.push(Event::Key { key: map_key(wparam as u32), pressed, repeat, mods });

            // Alt combinations still go to DefWindowProc so system commands
            // such as Alt+F4 keep working.
            let system = msg == WM_SYSKEYDOWN || msg == WM_SYSKEYUP;
            drop(state);
            if system {
                return DefWindowProcW(hwnd, msg, wparam, lparam);
            }
            return 0;
        }

        WM_SYSCHAR => {
            // Swallowed to avoid the menu beep on Alt shortcuts.
            return 0;
        }

        WM_CHAR => {
            let unit = wparam as u16;
            if (0xD800..0xDC00).contains(&unit) {
                state.high_surrogate = unit;
                return 0;
            }
            let ch = if (0xDC00..0xE000).contains(&unit) && state.high_surrogate != 0 {
                let hi = state.high_surrogate as u32;
                state.high_surrogate = 0;
                let cp = 0x10000 + ((hi - 0xD800) << 10) + (unit as u32 - 0xDC00);
                char::from_u32(cp)
            } else {
                state.high_surrogate = 0;
                char::from_u32(unit as u32)
            };
            if let Some(c) = ch {
                // Control codes are handled through Event::Key instead.
                if c >= ' ' && c != '\u{7f}' {
                    state.events.push(Event::Text(c));
                }
            }
            return 0;
        }

        WM_CLOSE => {
            state.events.push(Event::CloseRequested);
            // Do not destroy here; the app decides when to quit.
            return 0;
        }

        WM_DESTROY => {
            drop(state);
            PostQuitMessage(0);
            return 0;
        }

        _ => {}
    }

    drop(state);
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

/// Diagnostic helper for the window title bar.
pub fn class_name() -> String {
    from_wide(&wide(CLASS_NAME))
}