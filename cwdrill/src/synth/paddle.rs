//! Paddle input and the element machine it drives.
//!
//! ## The two rules a keyer follows, and why they differ
//!
//! At the boundary of every element the machine asks what to send next, and it
//! asks two different questions of the two paddles.
//!
//! The paddle that sent the last element is read as a **level**: closed now means
//! send another, open now means stop. That is what makes a held lever repeat and
//! a released one stop at the element the operator meant. Reading it as a memory
//! instead is the classic mistake and it doubles everything: at twenty words a
//! minute an element and its gap occupy a hundred and twenty milliseconds, a
//! deliberate tap lasts a hundred, so a memory set during the tap is still
//! standing at the boundary and fires a second element the operator did not ask
//! for. There is no way to press briefly enough to avoid it.
//!
//! The other paddle is read as a **memory**: closed at any point during the
//! element means send it next. That is the whole of iambic keying, and it is
//! necessary because the hand releases the opposite lever long before the
//! boundary arrives.
//!
//! ## Why a press is also latched
//!
//! The audio thread does not run per sample. It wakes once per buffer, computes
//! twenty milliseconds of sound in a fraction of a millisecond, and sleeps. A
//! press and its release that both fall between two wakings are never observed
//! as a level at all.
//!
//! So a press sets a sticky bit as well as the level. The machine takes the bit
//! into a request of its own and clears the shared one at once, so the next press
//! can set it again; the request is spent at the next decision. A tap too short
//! to be seen is still a tap, and it produces exactly one element rather than one
//! per sample it was visible for.
//!
//! ## Why a straight key is a gate rather than an element
//!
//! With a paddle the machine decides the length and the operator decides only
//! which element and when, so an element can be handed to the renderer complete.
//! With a straight key the operator decides the length, and it is not known until
//! the contact opens. A gate driven per sample is the honest shape of that: the
//! tone follows the contact through the same edge the rest of the application
//! uses, and the length is measured rather than stated.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use crate::config::settings::{PaddleMode, TimingSettings};
use crate::synth::{Edge, Element, Envelope};

/// The contact is closed now.
const FLAG_DIT_CLOSED: u32 = 0x1;
const FLAG_DAH_CLOSED: u32 = 0x2;
/// The contact has closed since the machine last looked.
///
/// Set by the press and cleared by the machine rather than by the release, which
/// is what makes a tap shorter than a buffer survive.
const FLAG_DIT_LATCH: u32 = 0x4;
const FLAG_DAH_LATCH: u32 = 0x8;

const LATCHES: u32 = FLAG_DIT_LATCH | FLAG_DAH_LATCH;

/// Gap at which a character is taken to have ended, in dot units.
///
/// Two, which is the midpoint between the one unit inside a character and the
/// three between two. A hand that runs its characters together and a hand that
/// leaves gaps are both read correctly by splitting the difference.
const CHAR_GAP_UNITS: usize = 2;

/// Gap at which a word is taken to have ended, in dot units.
///
/// Five, the midpoint between three and seven.
const WORD_GAP_UNITS: usize = 5;

/// What the machine produced, as the decoder reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Keyed {
    #[default]
    Dit,
    Dah,
    CharGap,
    WordGap,
}

/// Contacts and the settings the machine reads.
pub struct Paddle {
    contacts: AtomicU32,
    sending: AtomicU32,
    mode: AtomicU32,
    dot_seconds: AtomicU32,
    weight: AtomicU32,
    /// Samples the mark being held has run, nought when none is.
    ///
    /// Only a hand key has one. An iambic element is decided and handed over
    /// whole, so there is nothing in progress to report about it; a hand keyed
    /// element has no length until the contact opens, and drawing nothing until
    /// then is what made the key look as though it judged a dot before the dot
    /// had finished being one.
    live_mark: AtomicU32,
    live_unit: AtomicU32,
}

#[derive(Debug, Clone, Copy)]
pub struct PaddleSnapshot {
    /// True while the sidetone is wanted, which is the send session running.
    pub sending: bool,
    pub mode: PaddleMode,
    pub dot_seconds: f32,
    pub weight: f32,
}

/// One reading of the two contacts.
#[derive(Debug, Clone, Copy, Default)]
pub struct Contacts {
    pub dit: bool,
    pub dah: bool,
    /// The contact closed since the machine last looked.
    pub dit_latch: bool,
    pub dah_latch: bool,
}

impl Paddle {
    pub fn new() -> Arc<Paddle> {
        Arc::new(Paddle {
            contacts: AtomicU32::new(0),
            sending: AtomicU32::new(0),
            mode: AtomicU32::new(2),
            dot_seconds: AtomicU32::new(0.06f32.to_bits()),
            weight: AtomicU32::new(3.0f32.to_bits()),
            live_mark: AtomicU32::new(0),
            live_unit: AtomicU32::new(1),
        })
    }

    /// States the mark being held, for the picture.
    ///
    /// Written once per buffer rather than per sample: the reader draws at the
    /// frame rate, and a store nine hundred and sixty times to satisfy a reader
    /// that looks sixty is work for nothing.
    pub fn set_live(&self, mark: u32, unit: u32) {
        self.live_mark.store(mark, Ordering::Relaxed);
        self.live_unit.store(unit.max(1), Ordering::Relaxed);
    }

    /// The mark being held and the length of one dot, in samples.
    pub fn progress(&self) -> (u32, u32) {
        (
            self.live_mark.load(Ordering::Relaxed),
            self.live_unit.load(Ordering::Relaxed).max(1),
        )
    }

    pub fn set_dit(&self, down: bool) {
        self.set(FLAG_DIT_CLOSED, FLAG_DIT_LATCH, down);
    }

    pub fn set_dah(&self, down: bool) {
        self.set(FLAG_DAH_CLOSED, FLAG_DAH_LATCH, down);
    }

    /// Opens both contacts and forgets what they were asking for.
    ///
    /// Called when the session ends, because a button held at that moment
    /// receives no release the paddle would see and the tone would stay on. The
    /// latches go with it: a request made before pressing stop is withdrawn.
    pub fn release(&self) {
        self.contacts.store(0, Ordering::Relaxed);
        self.live_mark.store(0, Ordering::Relaxed);
    }

    fn set(&self, closed: u32, latch: u32, down: bool) {
        if down {
            // Both at once, so a reader cannot see the latch without the level
            // and start an element it then believes was already released.
            self.contacts.fetch_or(closed | latch, Ordering::Relaxed);
        } else {
            // The latch survives the release. That is the whole mechanism: a tap
            // the audio thread never observed as a level is still a tap.
            self.contacts.fetch_and(!closed, Ordering::Relaxed);
        }
    }

    #[inline]
    pub fn sample(&self) -> Contacts {
        let v = self.contacts.load(Ordering::Relaxed);
        Contacts {
            dit: v & FLAG_DIT_CLOSED != 0,
            dah: v & FLAG_DAH_CLOSED != 0,
            dit_latch: v & FLAG_DIT_LATCH != 0,
            dah_latch: v & FLAG_DAH_LATCH != 0,
        }
    }

    /// Clears both latches, the machine having taken them.
    ///
    /// Cleared as soon as they are read rather than when they are acted on, so a
    /// second press during the same element sets a fresh one instead of finding
    /// the bit already up.
    #[inline]
    pub fn consume(&self) {
        self.contacts.fetch_and(!LATCHES, Ordering::Relaxed);
    }

    /// State of the two contacts, as the picture draws them.
    ///
    /// The levels alone. A lever lit by a latch would stay lit after the release
    /// until the machine got round to it, which reads as a switch that sticks.
    pub fn contacts(&self) -> (bool, bool) {
        let v = self.contacts.load(Ordering::Relaxed);
        (v & FLAG_DIT_CLOSED != 0, v & FLAG_DAH_CLOSED != 0)
    }

    pub fn publish(&self, mode: PaddleMode, timing: &TimingSettings, sending: bool) {
        let code = match mode {
            PaddleMode::Straight => 0u32,
            PaddleMode::IambicA => 1,
            PaddleMode::IambicB => 2,
        };
        self.mode.store(code, Ordering::Relaxed);
        self.dot_seconds
            .store(timing.dot_seconds().to_bits(), Ordering::Relaxed);
        self.weight.store(timing.weight.to_bits(), Ordering::Relaxed);
        // Released last, so a reader that saw the flag saw the settings that go
        // with it rather than the previous ones.
        self.sending.store(u32::from(sending), Ordering::Release);
    }

    pub fn read(&self) -> PaddleSnapshot {
        let sending = self.sending.load(Ordering::Acquire) != 0;
        let mode = match self.mode.load(Ordering::Relaxed) {
            0 => PaddleMode::Straight,
            1 => PaddleMode::IambicA,
            _ => PaddleMode::IambicB,
        };
        PaddleSnapshot {
            sending,
            mode,
            dot_seconds: f32::from_bits(self.dot_seconds.load(Ordering::Relaxed)),
            weight: f32::from_bits(self.weight.load(Ordering::Relaxed)),
        }
    }
}

/// What one sample of the machine produced.
///
/// The gap and the mark are separate, because both can be due on one sample: the
/// machine can be idle long enough for a character to end at the very moment the
/// next press arrives. One field would drop whichever was written second.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tick {
    /// An element the renderer should begin now.
    pub element: Option<Element>,
    pub edge: Option<Edge>,
    /// A character or word boundary that has just passed.
    pub gap: Option<Keyed>,
    /// An element that has just been decided or measured.
    pub mark: Option<Keyed>,
    /// True when a latch was read, so the caller can clear it.
    pub consumed: bool,
}

pub struct PaddleKeyer {
    rate: f32,
    mode: PaddleMode,
    dot: usize,
    dash: usize,
    gap: usize,

    /// Samples left in the mark being rendered.
    mark_left: usize,
    /// Samples left in the space that follows it.
    space_left: usize,
    /// Which element was sent last, so a squeeze alternates.
    last: Option<bool>,

    /// The opposite paddle closed during the element in progress.
    ///
    /// Memory, and only for the opposite paddle. The one that sent the last
    /// element is read as a level at the boundary, see the note at the head of
    /// the file.
    seen_dit: bool,
    seen_dah: bool,
    /// Both closed at once during it, which is the only thing mode A discards.
    squeezed: bool,

    /// A press taken from the shared latch and not yet spent.
    ///
    /// One deep, like every hardware keyer: two taps inside one element are one
    /// request, because the second has nowhere to go that is not the third
    /// element.
    pending_dit: bool,
    pending_dah: bool,

    /// Straight key contact, and how long it has been closed.
    down: bool,
    mark: usize,
    mark_at: u64,
    /// Samples the gate is held open for a tap too short to be observed.
    hold: usize,

    /// Silence since the last mark ended.
    silence: usize,
    /// True while a character is being assembled.
    in_char: bool,
    char_done: bool,
    word_done: bool,
}

impl PaddleKeyer {
    pub fn new(rate: f32) -> PaddleKeyer {
        let mut keyer = PaddleKeyer {
            rate: rate.max(1.0),
            mode: PaddleMode::IambicB,
            dot: 1,
            dash: 3,
            gap: 1,
            mark_left: 0,
            space_left: 0,
            last: None,
            seen_dit: false,
            seen_dah: false,
            squeezed: false,
            pending_dit: false,
            pending_dah: false,
            down: false,
            mark: 0,
            mark_at: 0,
            hold: 0,
            silence: 0,
            in_char: false,
            char_done: true,
            word_done: true,
        };
        keyer.configure(PaddleMode::IambicB, 0.06, 3.0);
        keyer
    }

    /// Applies the timing, when nothing is in flight.
    ///
    /// Refused mid element on purpose: shortening a dash that has already started
    /// would produce an element of a length nobody asked for.
    pub fn configure(&mut self, mode: PaddleMode, dot_seconds: f32, weight: f32) {
        if self.mark_left > 0 || self.space_left > 0 || self.down || self.hold > 0 {
            return;
        }
        self.mode = mode;
        self.dot = ((dot_seconds * self.rate).round() as usize).max(1);
        self.dash = ((dot_seconds * weight.max(1.0) * self.rate).round() as usize).max(1);
        self.gap = self.dot;
    }

    pub fn reset(&mut self) {
        self.mark_left = 0;
        self.space_left = 0;
        self.last = None;
        self.seen_dit = false;
        self.seen_dah = false;
        self.squeezed = false;
        self.pending_dit = false;
        self.pending_dah = false;
        self.down = false;
        self.mark = 0;
        self.hold = 0;
        self.silence = 0;
        self.in_char = false;
        self.char_done = true;
        self.word_done = true;
    }

    #[inline]
    pub fn tick(&mut self, contacts: Contacts, at: u64) -> Tick {
        match self.mode {
            PaddleMode::Straight => self.tick_straight(contacts, at),
            _ => self.tick_iambic(contacts, at),
        }
    }

    /// True while a hand keyed contact is closed.
    #[inline]
    pub fn gated(&self) -> bool {
        self.down || self.hold > 0
    }

    /// Samples the mark being held has run, nought when none is.
    ///
    /// Hand key only, see the note on the shared field: an iambic element is
    /// already decided by the time anything could draw it.
    #[inline]
    pub fn live_mark(&self) -> u32 {
        if self.mode == PaddleMode::Straight && self.down {
            self.mark as u32
        } else {
            0
        }
    }

    /// Samples one dot occupies.
    #[inline]
    pub fn dot_samples(&self) -> u32 {
        self.dot as u32
    }

    fn tick_straight(&mut self, contacts: Contacts, at: u64) -> Tick {
        let mut out = Tick::default();

        let closed = contacts.dit || contacts.dah;
        let latch = contacts.dit_latch || contacts.dah_latch;
        if latch {
            out.consumed = true;
            // A tap the buffer boundary swallowed. The contact really did close,
            // so a minimum mark is sent rather than nothing: an operator who
            // pressed and heard silence cannot tell the key from the trainer.
            if !closed && !self.down && self.hold == 0 {
                self.hold = self.dot;
            }
        }

        let down = closed || self.hold > 0;
        if self.hold > 0 {
            self.hold -= 1;
        }

        if down && !self.down {
            self.down = true;
            self.mark = 0;
            self.mark_at = at;
            self.silence = 0;
        } else if !down && self.down {
            self.down = false;
            // The midpoint between one unit and three, which is where a hand that
            // is slightly long and a hand that is slightly short are both read as
            // what they meant.
            let dah = self.mark * 2 >= self.dot + self.dash;
            let ideal = if dah { self.dash } else { self.dot };
            out.edge = Some(Edge {
                at: self.mark_at,
                ideal_samples: ideal as u32,
                sync: !self.in_char,
            });
            out.mark = Some(if dah { Keyed::Dah } else { Keyed::Dit });
            self.in_char = true;
            self.char_done = false;
            self.word_done = false;
            self.silence = 0;
        }

        if self.down {
            self.mark += 1;
        } else {
            self.silence += 1;
            out.gap = self.gap_event();
        }
        out
    }

    fn tick_iambic(&mut self, contacts: Contacts, at: u64) -> Tick {
        let mut out = Tick::default();

        // The shared latch is taken into a request of our own and released at
        // once, so a second press during the same element sets a fresh one.
        if contacts.dit_latch {
            self.pending_dit = true;
            out.consumed = true;
        }
        if contacts.dah_latch {
            self.pending_dah = true;
            out.consumed = true;
        }

        // Memory, and only for the paddle that is not sending. The one that is
        // gets read as a level at the boundary, which is what stops a hundred
        // millisecond tap from producing two elements.
        if contacts.dit && self.last != Some(true) {
            self.seen_dit = true;
        }
        if contacts.dah && self.last != Some(false) {
            self.seen_dah = true;
        }
        if contacts.dit && contacts.dah {
            self.squeezed = true;
        }

        if self.mark_left > 0 {
            self.mark_left -= 1;
            self.silence = 0;
            return out;
        }
        if self.space_left > 0 {
            self.space_left -= 1;
            self.silence += 1;
            if self.space_left > 0 {
                return out;
            }
            // The space has just run out, so the decision is due on this sample.
        } else {
            self.silence += 1;
            out.gap = self.gap_event();
        }

        let next = self.choose(contacts);
        // Spent whatever the decision was, including a decision to send nothing:
        // a request left standing would fire on the next boundary unasked.
        self.pending_dit = false;
        self.pending_dah = false;
        self.seen_dit = false;
        self.seen_dah = false;
        self.squeezed = false;

        match next {
            Some(is_dit) => {
                let length = if is_dit { self.dot } else { self.dash };
                self.mark_left = length;
                self.space_left = self.gap;
                self.last = Some(is_dit);

                let seconds = length as f32 / self.rate;
                out.element = Some(Element {
                    on: true,
                    seconds,
                    // The machine keeps time, so the two are the same number. The
                    // picture then shows the ideal marks landing on the edges,
                    // which is what says the keyer is doing its job.
                    ideal_seconds: seconds,
                    sync: !self.in_char,
                });
                out.edge = Some(Edge {
                    at,
                    ideal_samples: length as u32,
                    sync: !self.in_char,
                });
                out.mark = Some(if is_dit { Keyed::Dit } else { Keyed::Dah });
                self.in_char = true;
                self.char_done = false;
                self.word_done = false;
                self.silence = 0;
            }
            None => self.last = None,
        }
        out
    }

    /// Which element follows, if any.
    ///
    /// The opposite paddle first, which is what makes a squeeze alternate. Then
    /// the same one, by level or by an unspent request: held means another,
    /// released means stop.
    ///
    /// The two modes differ in one place. With both levers open at the boundary
    /// after a squeeze, mode A drops the element the squeeze would have added and
    /// mode B sends it. A deliberate tap survives in both, because a tap is a
    /// request rather than the tail of a squeeze.
    fn choose(&mut self, contacts: Contacts) -> Option<bool> {
        if self.mode == PaddleMode::IambicA
            && self.squeezed
            && !contacts.dit
            && !contacts.dah
        {
            self.seen_dit = false;
            self.seen_dah = false;
        }

        let want_dit = contacts.dit || self.pending_dit;
        let want_dah = contacts.dah || self.pending_dah;

        match self.last {
            Some(true) => {
                if self.seen_dah || want_dah {
                    Some(false)
                } else if want_dit {
                    Some(true)
                } else {
                    None
                }
            }
            Some(false) => {
                if self.seen_dit || want_dit {
                    Some(true)
                } else if want_dah {
                    Some(false)
                } else {
                    None
                }
            }
            None => {
                // From rest a squeeze starts with the dit, which is the
                // convention every keyer follows.
                if want_dit {
                    Some(true)
                } else if want_dah {
                    Some(false)
                } else {
                    None
                }
            }
        }
    }

    fn gap_event(&mut self) -> Option<Keyed> {
        if self.in_char && !self.char_done && self.silence >= self.dot * CHAR_GAP_UNITS {
            self.char_done = true;
            self.in_char = false;
            return Some(Keyed::CharGap);
        }
        if self.char_done && !self.word_done && self.silence >= self.dot * WORD_GAP_UNITS {
            self.word_done = true;
            return Some(Keyed::WordGap);
        }
        None
    }
}

/// Tone gate for a hand key.
///
/// A position along the edge rather than an amplitude, so the shape the rest of
/// the application uses applies here as well.
pub struct Gate {
    phase: f32,
}

impl Gate {
    pub fn new() -> Gate {
        Gate { phase: 0.0 }
    }

    pub fn reset(&mut self) {
        self.phase = 0.0;
    }

    #[inline]
    pub fn advance(&mut self, down: bool, envelope: &Envelope) -> f32 {
        let edge = if down { envelope.rise() } else { envelope.fall() };
        let step = if edge > 0 { 1.0 / edge as f32 } else { 1.0 };
        if down {
            self.phase = (self.phase + step).min(1.0);
        } else {
            self.phase = (self.phase - step).max(0.0);
        }
        if self.phase <= 0.0 {
            0.0
        } else {
            envelope.shape(self.phase)
        }
    }
}

impl Default for Gate {
    fn default() -> Gate {
        Gate::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Drives the machine the way the real pair of threads does.
    ///
    /// The latch is set by a closure and cleared when the machine says it took
    /// it, so a test presses and releases rather than describing the bits.
    struct Bench {
        keyer: PaddleKeyer,
        at: u64,
        dit: bool,
        dah: bool,
        dit_latch: bool,
        dah_latch: bool,
        events: Vec<Keyed>,
    }

    impl Bench {
        fn new(mode: PaddleMode) -> Bench {
            // A thousand samples a second and a dot of ten milliseconds makes a
            // dot ten samples, which keeps the arithmetic readable.
            let mut keyer = PaddleKeyer::new(1000.0);
            keyer.configure(mode, 0.01, 3.0);
            Bench {
                keyer,
                at: 0,
                dit: false,
                dah: false,
                dit_latch: false,
                dah_latch: false,
                events: Vec::new(),
            }
        }

        fn set(&mut self, dit: bool, dah: bool) {
            if dit && !self.dit {
                self.dit_latch = true;
            }
            if dah && !self.dah {
                self.dah_latch = true;
            }
            self.dit = dit;
            self.dah = dah;
        }

        fn run(&mut self, samples: usize) {
            for _ in 0..samples {
                let contacts = Contacts {
                    dit: self.dit,
                    dah: self.dah,
                    dit_latch: self.dit_latch,
                    dah_latch: self.dah_latch,
                };
                let tick = self.keyer.tick(contacts, self.at);
                if tick.consumed {
                    self.dit_latch = false;
                    self.dah_latch = false;
                }
                if let Some(e) = tick.gap {
                    self.events.push(e);
                }
                if let Some(e) = tick.mark {
                    self.events.push(e);
                }
                self.at += 1;
            }
        }

        fn hold(&mut self, dit: bool, dah: bool, samples: usize) {
            self.set(dit, dah);
            self.run(samples);
        }

        fn marks(&self) -> usize {
            self.events
                .iter()
                .filter(|&&e| e == Keyed::Dit || e == Keyed::Dah)
                .count()
        }

        fn dits(&self) -> usize {
            self.events.iter().filter(|&&e| e == Keyed::Dit).count()
        }
    }

    #[test]
    fn a_press_of_one_element_sends_one_element() {
        // The failure that made the key unusable. An element and its gap are
        // twenty samples here; a press of fifteen is a deliberate single tap and
        // has to produce one element, not two.
        let mut b = Bench::new(PaddleMode::IambicB);
        b.hold(true, false, 15);
        b.hold(false, false, 80);
        assert_eq!(b.marks(), 1, "the press doubled: {:?}", b.events);
    }

    #[test]
    fn a_press_just_short_of_the_boundary_sends_one() {
        // The worst case of the same failure: released one sample before the
        // decision. A memory would still be standing; a level is not.
        let mut b = Bench::new(PaddleMode::IambicB);
        b.hold(true, false, 19);
        b.hold(false, false, 80);
        assert_eq!(b.marks(), 1, "{:?}", b.events);
    }

    #[test]
    fn a_held_lever_repeats() {
        let mut b = Bench::new(PaddleMode::IambicB);
        // Four cycles of twenty samples.
        b.hold(true, false, 80);
        assert_eq!(b.marks(), 4, "{:?}", b.events);
        assert!(b.events.iter().all(|&e| e == Keyed::Dit));
    }

    #[test]
    fn a_squeeze_alternates() {
        let mut b = Bench::new(PaddleMode::IambicB);
        // A dit of ten, a space of ten, a dash of thirty, a space of ten.
        b.hold(true, true, 60);
        assert_eq!(b.events, vec![Keyed::Dit, Keyed::Dah], "{:?}", b.events);
    }

    #[test]
    fn two_taps_on_one_lever_send_two_elements() {
        // The second half of the original complaint. A tap landing inside the
        // element already sending is a request for another of the same.
        let mut b = Bench::new(PaddleMode::IambicB);
        b.hold(true, false, 2);
        b.hold(false, false, 3);
        b.hold(true, false, 2);
        b.hold(false, false, 80);
        assert_eq!(b.dits(), 2, "{:?}", b.events);
    }

    #[test]
    fn a_run_of_taps_produces_a_run_of_elements() {
        let mut b = Bench::new(PaddleMode::IambicB);
        for _ in 0..4 {
            b.hold(true, false, 2);
            b.hold(false, false, 22);
        }
        b.hold(false, false, 100);
        assert_eq!(b.dits(), 4, "{:?}", b.events);
    }

    #[test]
    fn a_tap_shorter_than_a_reading_still_sends_once() {
        // The buffer boundary case: the interface saw a press and a release, and
        // the audio thread woke after both. Without the latch the tap is gone;
        // with a latch that is never spent it would repeat forever.
        let mut b = Bench::new(PaddleMode::IambicB);
        b.set(true, false);
        b.set(false, false);
        b.run(100);
        assert_eq!(b.marks(), 1, "{:?}", b.events);
    }

    #[test]
    fn the_memory_honours_a_tap_on_the_other_lever() {
        let mut b = Bench::new(PaddleMode::IambicB);
        // The dash lever alone, then a two sample tap on the dit inside it.
        b.hold(false, true, 5);
        b.hold(true, true, 2);
        b.hold(false, false, 80);
        assert!(
            b.events.starts_with(&[Keyed::Dah, Keyed::Dit]),
            "the tap was lost: {:?}",
            b.events
        );
    }

    #[test]
    fn the_two_modes_differ_by_one_element() {
        // The documented difference and the only one.
        for (mode, expected) in [
            (PaddleMode::IambicA, 2usize),
            (PaddleMode::IambicB, 3usize),
        ] {
            let mut b = Bench::new(mode);
            // Squeezed for a dit and most of the dash that follows, then let go
            // well before the dash ends.
            b.hold(true, true, 35);
            b.hold(false, false, 100);
            assert_eq!(b.marks(), expected, "{:?} sent {:?}", mode, b.events);
        }
    }

    #[test]
    fn mode_a_keeps_a_plain_tap() {
        // What mode A discards is the tail of a squeeze. A tap on one lever is a
        // request and survives in both modes.
        let mut b = Bench::new(PaddleMode::IambicA);
        b.hold(true, false, 2);
        b.hold(false, false, 3);
        b.hold(true, false, 2);
        b.hold(false, false, 80);
        assert_eq!(b.dits(), 2, "{:?}", b.events);
    }

    #[test]
    fn a_gap_ends_the_character_and_then_the_word() {
        let mut b = Bench::new(PaddleMode::IambicB);
        b.hold(true, false, 10);
        b.hold(false, false, 200);
        assert_eq!(b.events, vec![Keyed::Dit, Keyed::CharGap, Keyed::WordGap]);
    }

    #[test]
    fn a_press_landing_on_a_gap_loses_neither() {
        // Both can be due on one sample, which is why they travel in separate
        // fields.
        let mut b = Bench::new(PaddleMode::IambicB);
        b.hold(true, false, 10);
        b.hold(false, false, 20);
        b.hold(true, false, 2);
        b.hold(false, false, 80);
        assert_eq!(b.dits(), 2, "{:?}", b.events);
        assert!(
            b.events.iter().any(|&e| e == Keyed::CharGap),
            "{:?}",
            b.events
        );
    }

    #[test]
    fn a_hand_key_is_classified_by_what_it_measured() {
        let mut b = Bench::new(PaddleMode::Straight);
        // Eight samples is nearer one unit than three, thirty is a dash.
        b.hold(true, false, 8);
        b.hold(false, false, 25);
        b.hold(true, false, 30);
        b.hold(false, false, 60);
        assert!(
            b.events.starts_with(&[Keyed::Dit, Keyed::CharGap, Keyed::Dah]),
            "{:?}",
            b.events
        );
    }

    #[test]
    fn a_hand_key_tap_shorter_than_a_reading_still_sends() {
        let mut b = Bench::new(PaddleMode::Straight);
        b.set(true, false);
        b.set(false, false);
        b.run(60);
        assert_eq!(b.marks(), 1, "{:?}", b.events);
    }

    #[test]
    fn the_speed_does_not_move_under_an_element() {
        let mut b = Bench::new(PaddleMode::IambicB);
        b.hold(false, true, 1);
        let before = b.keyer.dash;
        b.keyer.configure(PaddleMode::IambicB, 0.02, 3.0);
        assert_eq!(b.keyer.dash, before, "the dash changed length mid element");
    }
}