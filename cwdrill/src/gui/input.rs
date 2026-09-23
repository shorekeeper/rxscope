//! Input accumulation.
//!
//! Platform events arrive at any point between frames, so they are folded
//! into a snapshot that the widget code samples once. Edge flags such as
//! pressed and released are cleared at the end of the frame that consumed
//! them, not at the start, otherwise events collected during the message pump
//! would be discarded before any widget saw them.

#![allow(dead_code)]

use crate::platform::{Event, Key, Modifiers, MouseButton};

/// Number of tracked buttons: left, right, middle and the two side buttons.
const BUTTONS: usize = 5;

#[derive(Debug, Clone, Copy)]
pub struct KeyPress {
    pub key: Key,
    pub repeat: bool,
}

pub struct InputState {
    pub mouse: (f32, f32),
    /// Movement since the previous frame, used by drag handlers.
    pub delta: (f32, f32),
    pub down: [bool; BUTTONS],
    pub pressed: [bool; BUTTONS],
    pub released: [bool; BUTTONS],
    pub double_click: bool,
    /// Notches, positive away from the operator.
    pub wheel: f32,
    pub wheel_h: f32,
    pub mods: Modifiers,
    /// Composed characters ready for insertion.
    pub text: Vec<char>,
    pub keys: Vec<KeyPress>,
    /// False after the pointer left the client area.
    pub inside: bool,
    pub window_focused: bool,
}

impl InputState {
    pub fn new() -> InputState {
        InputState {
            mouse: (-1.0, -1.0),
            delta: (0.0, 0.0),
            down: [false; BUTTONS],
            pressed: [false; BUTTONS],
            released: [false; BUTTONS],
            double_click: false,
            wheel: 0.0,
            wheel_h: 0.0,
            mods: Modifiers::default(),
            text: Vec::with_capacity(8),
            keys: Vec::with_capacity(8),
            inside: false,
            window_focused: true,
        }
    }

    pub fn on_event(&mut self, ev: &Event) {
        match *ev {
            Event::MouseMove { x, y, mods } => {
                // The delta accumulates across several moves inside one frame
                // so a fast drag does not lose distance.
                if self.inside {
                    self.delta.0 += x - self.mouse.0;
                    self.delta.1 += y - self.mouse.1;
                }
                self.mouse = (x, y);
                self.mods = mods;
                self.inside = true;
            }

            Event::MouseButton { button, pressed, x, y, mods } => {
                let i = button_index(button);
                self.mouse = (x, y);
                self.mods = mods;
                self.inside = true;
                if pressed {
                    self.down[i] = true;
                    self.pressed[i] = true;
                } else {
                    self.down[i] = false;
                    self.released[i] = true;
                }
            }

            Event::MouseDoubleClick { button, x, y } => {
                if button == MouseButton::Left {
                    self.double_click = true;
                }
                self.mouse = (x, y);
            }

            Event::MouseWheel { delta_y, delta_x, x, y, mods } => {
                self.wheel += delta_y;
                self.wheel_h += delta_x;
                self.mouse = (x, y);
                self.mods = mods;
            }

            Event::MouseLeave => {
                self.inside = false;
                // Buttons are released implicitly: the capture is gone, so no
                // further release message will arrive for this press.
                for i in 0..BUTTONS {
                    if self.down[i] {
                        self.down[i] = false;
                        self.released[i] = true;
                    }
                }
            }

            Event::Key { key, pressed, repeat, mods } => {
                self.mods = mods;
                if pressed {
                    self.keys.push(KeyPress { key, repeat });
                }
            }

            Event::Text(c) => self.text.push(c),

            Event::Focus(state) => {
                self.window_focused = state;
                if !state {
                    // Losing focus cancels every held button, otherwise a
                    // slider would keep tracking after an alt tab.
                    self.down = [false; BUTTONS];
                }
            }

            _ => {}
        }
    }

    /// Clears the edge triggered part of the snapshot.
    pub fn end_frame(&mut self) {
        self.pressed = [false; BUTTONS];
        self.released = [false; BUTTONS];
        self.double_click = false;
        self.wheel = 0.0;
        self.wheel_h = 0.0;
        self.delta = (0.0, 0.0);
        self.text.clear();
        self.keys.clear();
    }

    pub fn key_pressed(&self, key: Key) -> bool {
        self.keys.iter().any(|k| k.key == key)
    }

    pub fn left_down(&self) -> bool {
        self.down[0]
    }
    pub fn left_pressed(&self) -> bool {
        self.pressed[0]
    }
    pub fn left_released(&self) -> bool {
        self.released[0]
    }

    /// State of an arbitrary button, for the widgets that are not bound to the
    /// left one.
    pub fn button_down(&self, button: MouseButton) -> bool {
        self.down[button_index(button)]
    }

    pub fn button_pressed(&self, button: MouseButton) -> bool {
        self.pressed[button_index(button)]
    }

    pub fn button_released(&self, button: MouseButton) -> bool {
        self.released[button_index(button)]
    }

    /// Discards the current left button state.
    ///
    /// Used by a modal element that already acted on the press: without this
    /// the same press would still be visible to whatever widget is declared
    /// afterwards, and a click that dismisses a list would also operate the
    /// control it landed on.
    pub fn consume_left(&mut self) {
        self.down[0] = false;
        self.pressed[0] = false;
        self.released[0] = false;
        self.double_click = false;
    }

    /// Discards the press edge of one button, leaving the held state alone.
    ///
    /// The edge is what starts a gesture, so clearing it stops a new one from
    /// beginning under an element that already acted on the press. The held
    /// state is left because a gesture that began elsewhere is still running,
    /// and cancelling it would cut a drag that merely wandered across.
    pub fn consume_press(&mut self, button: MouseButton) {
        let i = button_index(button);
        self.pressed[i] = false;
        self.released[i] = false;
    }

    /// Discards the wheel movement, so a scroll aimed at an open list does not
    /// also scroll the panel underneath it.
    pub fn consume_wheel(&mut self) {
        self.wheel = 0.0;
        self.wheel_h = 0.0;
    }
}

impl Default for InputState {
    fn default() -> Self {
        InputState::new()
    }
}

fn button_index(b: MouseButton) -> usize {
    match b {
        MouseButton::Left => 0,
        MouseButton::Right => 1,
        MouseButton::Middle => 2,
        MouseButton::X1 => 3,
        MouseButton::X2 => 4,
    }
}