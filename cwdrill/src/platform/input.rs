//! OS independent input and window event vocabulary.
//!
//! Coordinates are physical pixels relative to the client area top left.
//! The GUI layer divides them by the DPI scale to get layout units.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    X1,
    X2,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Modifiers {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

impl Modifiers {
    pub fn none(&self) -> bool {
        !self.shift && !self.ctrl && !self.alt
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    Left,
    Right,
    Up,
    Down,
    Space,
    Shift,
    Ctrl,
    Alt,
    /// Function keys, 1 based.
    F(u8),
    /// ASCII digit 0..9.
    Digit(u8),
    /// ASCII uppercase letter code.
    Letter(u8),
    Unknown(u32),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Event {
    /// Client area size in physical pixels. Always sent once after creation.
    Resized { width: u32, height: u32 },
    /// User pressed the close button or Alt+F4.
    CloseRequested,
    /// Window went to or came back from the minimized state. The renderer
    /// stops submitting work while minimized.
    Minimized(bool),
    Focus(bool),
    /// Interactive move or resize started or ended. Used to throttle the
    /// heavy waterfall path during a drag.
    ModalResize(bool),
    MouseMove { x: f32, y: f32, mods: Modifiers },
    MouseButton { button: MouseButton, pressed: bool, x: f32, y: f32, mods: Modifiers },
    MouseDoubleClick { button: MouseButton, x: f32, y: f32 },
    /// Positive delta means scroll away from the user, one notch is 1.0.
    MouseWheel { delta_y: f32, delta_x: f32, x: f32, y: f32, mods: Modifiers },
    MouseLeave,
    Key { key: Key, pressed: bool, repeat: bool, mods: Modifiers },
    /// Already composed text input, surrogate pairs are joined.
    Text(char),
    /// New DPI scale, 1.0 equals 96 dpi.
    DpiChanged { scale: f32 },
}