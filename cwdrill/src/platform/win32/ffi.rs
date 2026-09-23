//! Raw Win32 declarations used by RXScope.
//!
//! Only what the project needs is declared. Names, types and layouts follow
//! the SDK so the structures can be compared against MSDN directly.
//! Functions that do not exist on older Windows builds are resolved at
//! runtime through GetProcAddress instead of being linked statically.

#![allow(non_snake_case, non_camel_case_types, dead_code)]

use std::ffi::c_void;

pub type BOOL = i32;
pub type WORD = u16;
pub type DWORD = u32;
pub type LONG = i32;
pub type UINT = u32;
pub type WPARAM = usize;
pub type LPARAM = isize;
pub type LRESULT = isize;
pub type LONG_PTR = isize;
pub type HANDLE = *mut c_void;
pub type HWND = HANDLE;
pub type HINSTANCE = HANDLE;
pub type HMODULE = HANDLE;
pub type HICON = HANDLE;
pub type HCURSOR = HANDLE;
pub type HBRUSH = HANDLE;
pub type HMENU = HANDLE;
pub type ATOM = WORD;
pub type LPCWSTR = *const u16;
pub type WNDPROC = Option<unsafe extern "system" fn(HWND, UINT, WPARAM, LPARAM) -> LRESULT>;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct POINT {
    pub x: LONG,
    pub y: LONG,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct RECT {
    pub left: LONG,
    pub top: LONG,
    pub right: LONG,
    pub bottom: LONG,
}

impl RECT {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct MSG {
    pub hwnd: HWND,
    pub message: UINT,
    pub wParam: WPARAM,
    pub lParam: LPARAM,
    pub time: DWORD,
    pub pt: POINT,
}

impl Default for MSG {
    fn default() -> Self {
        MSG {
            hwnd: std::ptr::null_mut(),
            message: 0,
            wParam: 0,
            lParam: 0,
            time: 0,
            pt: POINT::default(),
        }
    }
}

#[repr(C)]
pub struct WNDCLASSEXW {
    pub cbSize: UINT,
    pub style: UINT,
    pub lpfnWndProc: WNDPROC,
    pub cbClsExtra: i32,
    pub cbWndExtra: i32,
    pub hInstance: HINSTANCE,
    pub hIcon: HICON,
    pub hCursor: HCURSOR,
    pub hbrBackground: HBRUSH,
    pub lpszMenuName: LPCWSTR,
    pub lpszClassName: LPCWSTR,
    pub hIconSm: HICON,
}

#[repr(C)]
pub struct CREATESTRUCTW {
    pub lpCreateParams: *mut c_void,
    pub hInstance: HINSTANCE,
    pub hMenu: HMENU,
    pub hwndParent: HWND,
    pub cy: i32,
    pub cx: i32,
    pub y: i32,
    pub x: i32,
    pub style: LONG,
    pub lpszName: LPCWSTR,
    pub lpszClass: LPCWSTR,
    pub dwExStyle: DWORD,
}

#[repr(C)]
pub struct MINMAXINFO {
    pub ptReserved: POINT,
    pub ptMaxSize: POINT,
    pub ptMaxPosition: POINT,
    pub ptMinTrackSize: POINT,
    pub ptMaxTrackSize: POINT,
}

#[repr(C)]
pub struct TRACKMOUSEEVENT {
    pub cbSize: DWORD,
    pub dwFlags: DWORD,
    pub hwndTrack: HWND,
    pub dwHoverTime: DWORD,
}

/// Frame calculation parameters.
///
/// Only the first rectangle matters: on entry it holds the proposed window
/// rectangle, on exit the client rectangle. Returning it unchanged is what
/// makes the client area cover the whole window.
#[repr(C)]
pub struct NCCALCSIZE_PARAMS {
    pub rgrc: [RECT; 3],
    pub lppos: *mut c_void,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct MONITORINFO {
    pub cbSize: DWORD,
    pub rcMonitor: RECT,
    pub rcWork: RECT,
    pub dwFlags: DWORD,
}

// Window class styles.
pub const CS_VREDRAW: UINT = 0x0001;
pub const CS_HREDRAW: UINT = 0x0002;
pub const CS_DBLCLKS: UINT = 0x0008;
pub const CS_OWNDC: UINT = 0x0020;

// Window styles.
pub const WS_OVERLAPPED: DWORD = 0x0000_0000;
pub const WS_CAPTION: DWORD = 0x00C0_0000;
pub const WS_SYSMENU: DWORD = 0x0008_0000;
pub const WS_THICKFRAME: DWORD = 0x0004_0000;
pub const WS_MINIMIZEBOX: DWORD = 0x0002_0000;
pub const WS_MAXIMIZEBOX: DWORD = 0x0001_0000;
pub const WS_OVERLAPPEDWINDOW: DWORD =
    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_THICKFRAME | WS_MINIMIZEBOX | WS_MAXIMIZEBOX;
pub const WS_VISIBLE: DWORD = 0x1000_0000;
pub const WS_EX_APPWINDOW: DWORD = 0x0004_0000;

pub const CW_USEDEFAULT: i32 = 0x8000_0000_u32 as i32;

// ShowWindow commands.
pub const SW_HIDE: i32 = 0;
pub const SW_SHOWNORMAL: i32 = 1;
pub const SW_MAXIMIZE: i32 = 3;
pub const SW_SHOW: i32 = 5;
pub const SW_RESTORE: i32 = 9;
pub const SW_MINIMIZE: i32 = 6;

// Messages.
pub const WM_CREATE: UINT = 0x0001;
pub const WM_DESTROY: UINT = 0x0002;
pub const WM_SIZE: UINT = 0x0005;
pub const WM_SETFOCUS: UINT = 0x0007;
pub const WM_KILLFOCUS: UINT = 0x0008;
pub const WM_PAINT: UINT = 0x000F;
pub const WM_CLOSE: UINT = 0x0010;
pub const WM_QUIT: UINT = 0x0012;
pub const WM_ERASEBKGND: UINT = 0x0014;
pub const WM_SETCURSOR: UINT = 0x0020;
pub const WM_GETMINMAXINFO: UINT = 0x0024;
pub const WM_NCCREATE: UINT = 0x0081;
pub const WM_KEYDOWN: UINT = 0x0100;
pub const WM_KEYUP: UINT = 0x0101;
pub const WM_CHAR: UINT = 0x0102;
pub const WM_SYSKEYDOWN: UINT = 0x0104;
pub const WM_SYSKEYUP: UINT = 0x0105;
pub const WM_SYSCHAR: UINT = 0x0106;
pub const WM_MOUSEMOVE: UINT = 0x0200;
pub const WM_LBUTTONDOWN: UINT = 0x0201;
pub const WM_LBUTTONUP: UINT = 0x0202;
pub const WM_LBUTTONDBLCLK: UINT = 0x0203;
pub const WM_RBUTTONDOWN: UINT = 0x0204;
pub const WM_RBUTTONUP: UINT = 0x0205;
pub const WM_RBUTTONDBLCLK: UINT = 0x0206;
pub const WM_MBUTTONDOWN: UINT = 0x0207;
pub const WM_MBUTTONUP: UINT = 0x0208;
pub const WM_MOUSEWHEEL: UINT = 0x020A;
pub const WM_XBUTTONDOWN: UINT = 0x020B;
pub const WM_XBUTTONUP: UINT = 0x020C;
pub const WM_MOUSEHWHEEL: UINT = 0x020E;
pub const WM_ENTERSIZEMOVE: UINT = 0x0231;
pub const WM_EXITSIZEMOVE: UINT = 0x0232;
pub const WM_MOUSELEAVE: UINT = 0x02A3;
pub const WM_DPICHANGED: UINT = 0x02E0;
// Non-client messages, needed only for the custom frame.
pub const WM_NCCALCSIZE: UINT = 0x0083;
pub const WM_NCHITTEST: UINT = 0x0084;
pub const WM_NCACTIVATE: UINT = 0x0086;
pub const WM_NCLBUTTONDOWN: UINT = 0x00A1;
pub const WM_SYSCOMMAND: UINT = 0x0112;

pub const SIZE_RESTORED: WPARAM = 0;
pub const SIZE_MINIMIZED: WPARAM = 1;
pub const SIZE_MAXIMIZED: WPARAM = 2;

pub const PM_REMOVE: UINT = 0x0001;

pub const GWLP_USERDATA: i32 = -21;

pub const HTCLIENT: isize = 1;
pub const HTNOWHERE: isize = 0;
pub const HTCAPTION: isize = 2;
pub const HTLEFT: isize = 10;
pub const HTRIGHT: isize = 11;
pub const HTTOP: isize = 12;
pub const HTTOPLEFT: isize = 13;
pub const HTTOPRIGHT: isize = 14;
pub const HTBOTTOM: isize = 15;
pub const HTBOTTOMLEFT: isize = 16;
pub const HTBOTTOMRIGHT: isize = 17;

/// Width of the resize grab band, in logical units.
///
/// Wider than the visible border because the border is not the target: a
/// pointer aimed at the edge of a frameless window lands a pixel or two inside
/// it, and a band narrower than that cannot be hit at all.
pub const RESIZE_GRAB: f32 = 6.0;

pub const TME_LEAVE: DWORD = 0x0000_0002;

pub const SWP_NOSIZE: UINT = 0x0001;
pub const SWP_NOMOVE: UINT = 0x0002;
pub const SWP_NOZORDER: UINT = 0x0004;
pub const SWP_NOACTIVATE: UINT = 0x0010;
pub const SWP_FRAMECHANGED: UINT = 0x0020;

/// Nearest monitor, for the maximized bounds.
pub const MONITOR_DEFAULTTONEAREST: DWORD = 0x0000_0002;

pub const MB_OK: UINT = 0x0000_0000;
pub const MB_ICONERROR: UINT = 0x0000_0010;

// Standard cursors, passed as pseudo pointers.
pub const IDC_ARROW: LPCWSTR = 32512 as LPCWSTR;
pub const IDC_IBEAM: LPCWSTR = 32513 as LPCWSTR;
pub const IDC_SIZEWE: LPCWSTR = 32644 as LPCWSTR;
pub const IDC_SIZENS: LPCWSTR = 32645 as LPCWSTR;
pub const IDC_HAND: LPCWSTR = 32649 as LPCWSTR;
pub const IDI_APPLICATION: LPCWSTR = 32512 as LPCWSTR;

/// Clipboard format for a null terminated wide string.
///
/// The only format written. The narrow one is a lossy copy of it and the
/// system synthesizes it on demand for a consumer that asks, so writing both
/// would be writing the same text twice.
pub const CF_UNICODETEXT: UINT = 13;

/// The clipboard takes ownership of the block and may relocate it, which is
/// what a moveable allocation permits and a fixed one does not.
pub const GMEM_MOVEABLE: UINT = 0x0002;

/// Identifier of the icon group build.rs writes into the executable.
///
/// An integer resource is passed where a string is expected, which is what the
/// resource macro in the platform headers does: the low word is the identifier
/// and the high word being nought is what marks it as one.
pub const ICON_RESOURCE_ID: LPCWSTR = 1 as LPCWSTR;

pub const IMAGE_ICON: UINT = 1;
pub const LR_DEFAULTCOLOR: UINT = 0x0000_0000;

// Icon metrics. The large one is what Alt-Tab shows, the small one what the
// taskbar and the window menu use.
pub const SM_CXICON: i32 = 11;
pub const SM_CYICON: i32 = 12;
pub const SM_CXSMICON: i32 = 49;
pub const SM_CYSMICON: i32 = 50;

// Virtual keys used by the GUI layer.
pub const VK_BACK: u32 = 0x08;
pub const VK_TAB: u32 = 0x09;
pub const VK_RETURN: u32 = 0x0D;
pub const VK_SHIFT: u32 = 0x10;
pub const VK_CONTROL: u32 = 0x11;
pub const VK_MENU: u32 = 0x12;
pub const VK_ESCAPE: u32 = 0x1B;
pub const VK_SPACE: u32 = 0x20;
pub const VK_PRIOR: u32 = 0x21;
pub const VK_NEXT: u32 = 0x22;
pub const VK_END: u32 = 0x23;
pub const VK_HOME: u32 = 0x24;
pub const VK_LEFT: u32 = 0x25;
pub const VK_UP: u32 = 0x26;
pub const VK_RIGHT: u32 = 0x27;
pub const VK_DOWN: u32 = 0x28;
pub const VK_INSERT: u32 = 0x2D;
pub const VK_DELETE: u32 = 0x2E;
pub const VK_F1: u32 = 0x70;

pub const WHEEL_DELTA: f32 = 120.0;

// Per monitor aware v2, passed to SetProcessDpiAwarenessContext.
pub const DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2: isize = -4;
pub const USER_DEFAULT_SCREEN_DPI: u32 = 96;

/// Broken down local time, used for decode timestamps.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SYSTEMTIME {
    pub wYear: WORD,
    pub wMonth: WORD,
    pub wDayOfWeek: WORD,
    pub wDay: WORD,
    pub wHour: WORD,
    pub wMinute: WORD,
    pub wSecond: WORD,
    pub wMilliseconds: WORD,
}

#[link(name = "kernel32")]
extern "system" {
    pub fn GetModuleHandleW(lpModuleName: LPCWSTR) -> HMODULE;
    pub fn GetLastError() -> DWORD;
    pub fn GetCurrentThreadId() -> DWORD;
    pub fn OutputDebugStringW(lpOutputString: LPCWSTR);
    pub fn Sleep(dwMilliseconds: DWORD);
    pub fn QueryPerformanceCounter(lpPerformanceCount: *mut i64) -> BOOL;
    pub fn QueryPerformanceFrequency(lpFrequency: *mut i64) -> BOOL;
    pub fn LoadLibraryW(lpLibFileName: LPCWSTR) -> HMODULE;
    pub fn GetProcAddress(hModule: HMODULE, lpProcName: *const i8) -> *const c_void;
    pub fn GetLocalTime(lpSystemTime: *mut SYSTEMTIME);
    pub fn GetSystemTime(lpSystemTime: *mut SYSTEMTIME);
    pub fn GlobalAlloc(uFlags: UINT, dwBytes: usize) -> HANDLE;
    pub fn GlobalFree(hMem: HANDLE) -> HANDLE;
    pub fn GlobalLock(hMem: HANDLE) -> *mut c_void;
    pub fn GlobalUnlock(hMem: HANDLE) -> BOOL;
}

#[link(name = "user32")]
extern "system" {
    pub fn RegisterClassExW(lpwcx: *const WNDCLASSEXW) -> ATOM;
    pub fn CreateWindowExW(
        dwExStyle: DWORD,
        lpClassName: LPCWSTR,
        lpWindowName: LPCWSTR,
        dwStyle: DWORD,
        x: i32,
        y: i32,
        nWidth: i32,
        nHeight: i32,
        hWndParent: HWND,
        hMenu: HMENU,
        hInstance: HINSTANCE,
        lpParam: *mut c_void,
    ) -> HWND;
    pub fn DestroyWindow(hWnd: HWND) -> BOOL;
    pub fn DefWindowProcW(hWnd: HWND, msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn PostQuitMessage(nExitCode: i32);
    pub fn PeekMessageW(
        lpMsg: *mut MSG,
        hWnd: HWND,
        wMsgFilterMin: UINT,
        wMsgFilterMax: UINT,
        wRemoveMsg: UINT,
    ) -> BOOL;
    pub fn TranslateMessage(lpMsg: *const MSG) -> BOOL;
    pub fn DispatchMessageW(lpMsg: *const MSG) -> LRESULT;
    pub fn ShowWindow(hWnd: HWND, nCmdShow: i32) -> BOOL;
    pub fn SetWindowTextW(hWnd: HWND, lpString: LPCWSTR) -> BOOL;
    pub fn GetClientRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    pub fn GetWindowRect(hWnd: HWND, lpRect: *mut RECT) -> BOOL;
    pub fn AdjustWindowRectEx(lpRect: *mut RECT, dwStyle: DWORD, bMenu: BOOL, dwExStyle: DWORD) -> BOOL;
    pub fn SetWindowLongPtrW(hWnd: HWND, nIndex: i32, dwNewLong: LONG_PTR) -> LONG_PTR;
    pub fn GetWindowLongPtrW(hWnd: HWND, nIndex: i32) -> LONG_PTR;
    pub fn SetWindowPos(
        hWnd: HWND,
        hWndInsertAfter: HWND,
        X: i32,
        Y: i32,
        cx: i32,
        cy: i32,
        uFlags: UINT,
    ) -> BOOL;
    pub fn GetWindowsDirectoryW(lpBuffer: *mut u16, uSize: UINT) -> UINT;
    pub fn LoadCursorW(hInstance: HINSTANCE, lpCursorName: LPCWSTR) -> HCURSOR;
    pub fn LoadIconW(hInstance: HINSTANCE, lpIconName: LPCWSTR) -> HICON;
    pub fn LoadImageW(
        hInst: HINSTANCE,
        name: LPCWSTR,
        type_: UINT,
        cx: i32,
        cy: i32,
        fuLoad: UINT,
    ) -> HANDLE;
    pub fn GetSystemMetrics(nIndex: i32) -> i32;
    pub fn SetCursor(hCursor: HCURSOR) -> HCURSOR;
    pub fn TrackMouseEvent(lpEventTrack: *mut TRACKMOUSEEVENT) -> BOOL;
    pub fn SetCapture(hWnd: HWND) -> HWND;
    pub fn ReleaseCapture() -> BOOL;
    pub fn GetKeyState(nVirtKey: i32) -> i16;
    pub fn ScreenToClient(hWnd: HWND, lpPoint: *mut POINT) -> BOOL;
    pub fn MessageBoxW(hWnd: HWND, lpText: LPCWSTR, lpCaption: LPCWSTR, uType: UINT) -> i32;
    pub fn SetProcessDPIAware() -> BOOL;
    pub fn IsIconic(hWnd: HWND) -> BOOL;
    pub fn CreateEventW(
        lpEventAttributes: *mut c_void,
        bManualReset: BOOL,
        bInitialState: BOOL,
        lpName: LPCWSTR,
    ) -> HANDLE;
    pub fn SetEvent(hEvent: HANDLE) -> BOOL;
    pub fn ResetEvent(hEvent: HANDLE) -> BOOL;
    pub fn CloseHandle(hObject: HANDLE) -> BOOL;
    pub fn WaitForSingleObject(hHandle: HANDLE, dwMilliseconds: DWORD) -> DWORD;
    pub fn GetCurrentThread() -> HANDLE;
    pub fn SetThreadPriority(hThread: HANDLE, nPriority: i32) -> BOOL;
    pub fn IsZoomed(hWnd: HWND) -> BOOL;
    pub fn SendMessageW(hWnd: HWND, msg: UINT, wParam: WPARAM, lParam: LPARAM) -> LRESULT;
    pub fn PostMessageW(hWnd: HWND, msg: UINT, wParam: WPARAM, lParam: LPARAM) -> BOOL;
    pub fn MonitorFromWindow(hWnd: HWND, dwFlags: DWORD) -> HANDLE;
    pub fn GetMonitorInfoW(hMonitor: HANDLE, lpmi: *mut MONITORINFO) -> BOOL;
    pub fn OpenClipboard(hWndNewOwner: HWND) -> BOOL;
    pub fn CloseClipboard() -> BOOL;
    pub fn EmptyClipboard() -> BOOL;
    pub fn SetClipboardData(uFormat: UINT, hMem: HANDLE) -> HANDLE;
}

#[link(name = "winmm")]
extern "system" {
    pub fn timeBeginPeriod(uPeriod: UINT) -> UINT;
    pub fn timeEndPeriod(uPeriod: UINT) -> UINT;
}

/// Resolves an export at runtime. Used for APIs missing on older Windows.
pub unsafe fn proc_address(module: &str, name: &[u8]) -> *const c_void {
    let wide: Vec<u16> = module.encode_utf16().chain(std::iter::once(0)).collect();
    let h = LoadLibraryW(wide.as_ptr());
    if h.is_null() {
        return std::ptr::null();
    }
    GetProcAddress(h, name.as_ptr() as *const i8)
}

/// SetProcessDpiAwarenessContext exists since Windows 10 1607. Falls back to
/// the legacy system wide call, otherwise the swapchain would be scaled by
/// the compositor and the waterfall would look blurred.
pub fn enable_per_monitor_dpi() {
    unsafe {
        type Fn1 = unsafe extern "system" fn(isize) -> BOOL;
        let p = proc_address("user32.dll", b"SetProcessDpiAwarenessContext\0");
        if !p.is_null() {
            let f: Fn1 = std::mem::transmute(p);
            if f(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) != 0 {
                return;
            }
        }
        SetProcessDPIAware();
    }
}

/// GetDpiForWindow exists since Windows 10 1607, returns 96 as a fallback.
pub fn dpi_for_window(hwnd: HWND) -> u32 {
    unsafe {
        type Fn1 = unsafe extern "system" fn(HWND) -> UINT;
        let p = proc_address("user32.dll", b"GetDpiForWindow\0");
        if p.is_null() {
            return USER_DEFAULT_SCREEN_DPI;
        }
        let f: Fn1 = std::mem::transmute(p);
        let dpi = f(hwnd);
        if dpi == 0 {
            USER_DEFAULT_SCREEN_DPI
        } else {
            dpi
        }
    }
}

#[inline]
pub fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}

#[inline]
pub fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

#[inline]
pub fn lparam_x(lp: LPARAM) -> i32 {
    (lp & 0xFFFF) as i16 as i32
}

#[inline]
pub fn lparam_y(lp: LPARAM) -> i32 {
    ((lp >> 16) & 0xFFFF) as i16 as i32
}

pub const WAIT_OBJECT_0: DWORD = 0;
pub const WAIT_TIMEOUT: DWORD = 0x0000_0102;
pub const INFINITE: DWORD = 0xFFFF_FFFF;

/// Capture threads run above normal so a busy interface cannot starve them.
/// Time critical is deliberately avoided: a stuck capture loop at that
/// priority can lock out the whole session.
pub const THREAD_PRIORITY_HIGHEST: i32 = 2;