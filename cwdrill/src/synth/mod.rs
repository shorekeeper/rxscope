//! Tone generation.
//!
//! ## What runs where
//!
//! The keyer that turns text into elements runs on the interface thread; this
//! runs on the audio thread and produces samples. The split is at the one place
//! where the two halves have nothing to say to each other: an element is a
//! duration, and rendering it needs no knowledge of the alphabet, the lesson or
//! the material.
//!
//! The paddle machine is the exception and lives here, because what it decides
//! is decided at an element boundary and a boundary is a sample index. See the
//! note at the head of the paddle module.
//!
//! Five queues cross the boundary. Elements go down for the station being copied
//! and for the interfering one. The envelope of the first goes up, decimated,
//! because the keying picture wants the shape rather than the carrier. Element
//! boundaries go up as events. And what the paddle produced goes up as
//! classifications, because the machine decided the element and publishing the
//! decision is cheaper and more truthful than publishing a duration for the
//! other side to threshold again.
//!
//! ## Why two voices rather than one plus noise
//!
//! Interference is a station, not a texture. What a student has to learn is to
//! hear one rhythm through another at a different pitch, and random keying does
//! not train it: the ear separates a pattern from noise far more easily than a
//! pattern from a pattern. So the second voice is fed real material through a
//! queue of its own, and everything about it except the pitch and the level is
//! the same machinery as the first.

pub mod conditions;
pub mod envelope;
pub mod keyer;
pub mod paddle;

use std::sync::atomic::{AtomicU32, Ordering};

use crate::config::settings::{ConditionsSettings, EnvelopeShape, PaddleMode, ToneSettings};
use crate::core::ring::{Consumer, Producer};

pub use conditions::{ConditionSnapshot, Conditions};
pub use envelope::Envelope;
pub use keyer::{Element, Keyer};
pub use paddle::{Contacts, Gate, Keyed, Paddle, PaddleKeyer};

/// Envelope entries published per second.
///
/// One millisecond resolves a five millisecond edge into five points and a twenty
/// millisecond dot, which is sixty words a minute, into twenty. Finer would draw
/// detail no display column can hold; coarser would render the edge shape as a
/// single step, which is the one thing the picture exists to show.
pub const SCOPE_RATE: f32 = 1000.0;

/// One element boundary, as the picture reads it.
///
/// Three facts, and the two obvious omissions are deliberate. How long the
/// element actually was is the difference between this position and the next one,
/// and whether the tone was on is what the envelope says: publishing either would
/// be publishing a second copy of something already crossing the same queue.
#[derive(Debug, Clone, Copy, Default)]
pub struct Edge {
    /// Sample index the element began at, counted from the start of the stream.
    pub at: u64,
    /// Samples the element would have occupied with no jitter and no swing.
    pub ideal_samples: u32,
    /// True on the first element of a character.
    pub sync: bool,
}

/// Sample layout of the endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleKind {
    F32,
    I16,
    I32,
}

#[derive(Debug, Clone, Copy)]
pub struct Format {
    pub kind: SampleKind,
    pub rate: u32,
    pub channels: usize,
    /// Bytes one sample of one channel occupies.
    pub bytes: usize,
}

impl Format {
    pub fn stride(&self) -> usize {
        self.channels * self.bytes
    }

    pub fn describe(&self) -> String {
        let kind = match self.kind {
            SampleKind::F32 => "float32",
            SampleKind::I16 => "int16",
            SampleKind::I32 => "int32",
        };
        format!("{} {} Hz {} ch", kind, self.rate, self.channels)
    }
}

/// Everything the sound is, published from the interface thread.
///
/// Packed into atomics rather than held behind a lock, because the reader is a
/// thread that must not wait: a lock the interface holds while it lays out a
/// panel would be a lock the audio thread waits on, and the buffer would run dry.
/// Each field is read independently, so a change that spans two of them is
/// briefly a mixture of old and new; for a pitch and a level that is inaudible,
/// and for a condition it is one buffer of a slightly different band.
pub struct Live {
    pitch: AtomicU32,
    gain: AtomicU32,
    pan: AtomicU32,
    rise: AtomicU32,
    fall: AtomicU32,
    shape: AtomicU32,

    /// Conditions, packed one per word for the same reason.
    flags: AtomicU32,
    snr: AtomicU32,
    qsb_rate: AtomicU32,
    qsb_depth: AtomicU32,
    qrm_offset: AtomicU32,
    qrm_level: AtomicU32,
    qrn_rate: AtomicU32,
    drift: AtomicU32,
}

const FLAG_NOISE: u32 = 0x1;
const FLAG_QSB: u32 = 0x2;
const FLAG_QRM: u32 = 0x4;
const FLAG_QRN: u32 = 0x8;

#[derive(Debug, Clone, Copy)]
pub struct ToneSnapshot {
    pub pitch_hz: f32,
    pub gain: f32,
    pub pan: f32,
    pub rise_ms: f32,
    pub fall_ms: f32,
    pub shape: EnvelopeShape,
}

impl Live {
    pub fn new(tone: &ToneSettings, conditions: &ConditionsSettings) -> std::sync::Arc<Live> {
        let live = std::sync::Arc::new(Live {
            pitch: AtomicU32::new(0),
            gain: AtomicU32::new(0),
            pan: AtomicU32::new(0),
            rise: AtomicU32::new(0),
            fall: AtomicU32::new(0),
            shape: AtomicU32::new(0),
            flags: AtomicU32::new(0),
            snr: AtomicU32::new(0),
            qsb_rate: AtomicU32::new(0),
            qsb_depth: AtomicU32::new(0),
            qrm_offset: AtomicU32::new(0),
            qrm_level: AtomicU32::new(0),
            qrn_rate: AtomicU32::new(0),
            drift: AtomicU32::new(0),
        });
        live.publish(tone, conditions);
        live
    }

    pub fn publish(&self, tone: &ToneSettings, conditions: &ConditionsSettings) {
        self.pitch.store(tone.pitch_hz.to_bits(), Ordering::Relaxed);
        self.gain.store(tone.gain().to_bits(), Ordering::Relaxed);
        self.pan.store(tone.pan.to_bits(), Ordering::Relaxed);
        self.rise.store(tone.rise_ms.to_bits(), Ordering::Relaxed);
        self.fall.store(tone.fall_ms.to_bits(), Ordering::Relaxed);

        self.snr.store(conditions.snr_db.to_bits(), Ordering::Relaxed);
        self.qsb_rate.store(conditions.qsb_rate_hz.to_bits(), Ordering::Relaxed);
        self.qsb_depth.store(conditions.qsb_depth_db.to_bits(), Ordering::Relaxed);
        self.qrm_offset.store(conditions.qrm_offset_hz.to_bits(), Ordering::Relaxed);
        self.qrm_level.store(conditions.qrm_level_db.to_bits(), Ordering::Relaxed);
        self.qrn_rate.store(conditions.qrn_per_minute.to_bits(), Ordering::Relaxed);
        self.drift.store(conditions.drift_hz_per_min.to_bits(), Ordering::Relaxed);

        let mut flags = 0u32;
        if conditions.noise {
            flags |= FLAG_NOISE;
        }
        if conditions.qsb {
            flags |= FLAG_QSB;
        }
        if conditions.qrm {
            flags |= FLAG_QRM;
        }
        if conditions.qrn {
            flags |= FLAG_QRN;
        }
        self.flags.store(flags, Ordering::Relaxed);

        let code = match tone.shape {
            EnvelopeShape::Hard => 0,
            EnvelopeShape::RaisedCosine => 1,
            EnvelopeShape::Gaussian => 2,
        };
        // Released last, so a reader that saw the shape saw everything before it.
        self.shape.store(code, Ordering::Release);
    }

    fn read_tone(&self) -> ToneSnapshot {
        let shape = match self.shape.load(Ordering::Acquire) {
            0 => EnvelopeShape::Hard,
            2 => EnvelopeShape::Gaussian,
            _ => EnvelopeShape::RaisedCosine,
        };
        ToneSnapshot {
            pitch_hz: f32::from_bits(self.pitch.load(Ordering::Relaxed)),
            gain: f32::from_bits(self.gain.load(Ordering::Relaxed)),
            pan: f32::from_bits(self.pan.load(Ordering::Relaxed)),
            rise_ms: f32::from_bits(self.rise.load(Ordering::Relaxed)),
            fall_ms: f32::from_bits(self.fall.load(Ordering::Relaxed)),
            shape,
        }
    }

    fn read_conditions(&self) -> ConditionSnapshot {
        let flags = self.flags.load(Ordering::Relaxed);
        ConditionSnapshot {
            noise: flags & FLAG_NOISE != 0,
            snr_db: f32::from_bits(self.snr.load(Ordering::Relaxed)),
            qsb: flags & FLAG_QSB != 0,
            qsb_rate_hz: f32::from_bits(self.qsb_rate.load(Ordering::Relaxed)),
            qsb_depth_db: f32::from_bits(self.qsb_depth.load(Ordering::Relaxed)),
            qrm: flags & FLAG_QRM != 0,
            qrm_offset_hz: f32::from_bits(self.qrm_offset.load(Ordering::Relaxed)),
            qrm_level_db: f32::from_bits(self.qrm_level.load(Ordering::Relaxed)),
            qrn: flags & FLAG_QRN != 0,
            qrn_per_minute: f32::from_bits(self.qrn_rate.load(Ordering::Relaxed)),
            drift_hz_per_min: f32::from_bits(self.drift.load(Ordering::Relaxed)),
        }
    }
}

/// Elements read from a queue in one go.
///
/// A batch removes one atomic pair per element in exchange for a small array.
/// Sixty four covers a whole group of five characters, so a fill of one buffer
/// normally touches the queue once.
const STAGE: usize = 64;

/// Envelope entries held before they are published.
const SCOPE_BATCH: usize = 256;

/// One station: an element cursor, an envelope and a phase.
///
/// Used twice. The interference is the same machinery at another pitch, which is
/// what makes it interference rather than a texture: a second copy of the same
/// code would be a second place for the keying to be wrong.
struct Voice {
    envelope: Envelope,
    /// Queue this voice plays, absent for the one the key drives.
    ///
    /// What the key produces is decided a sample at a time by the machine, so a
    /// channel would be storage nothing ever writes into.
    elements: Option<Consumer<Element>>,
    stage: [Element; STAGE],
    staged: usize,
    stage_pos: usize,
    /// An element handed in directly rather than through the queue.
    ///
    /// What the paddle machine writes. Held apart from the queue because the two
    /// are different sources with different lifetimes: the queue holds material
    /// the session decided minutes of frames ago, and this holds an element that
    /// begins on this very sample.
    pending: Option<Element>,

    on: bool,
    length: usize,
    position: usize,
    phase: f64,
    /// Element that has just started, so the caller can publish a boundary.
    started: Option<(u32, bool)>,
}

impl Voice {
    fn new(elements: Option<Consumer<Element>>) -> Voice {
        Voice {
            envelope: Envelope::new(),
            elements,
            stage: [Element::default(); STAGE],
            staged: 0,
            stage_pos: 0,
            pending: None,
            on: false,
            length: 0,
            position: 0,
            phase: 0.0,
            started: None,
        }
    }

    fn pending_count(&self) -> usize {
        let queued = self.elements.as_ref().map(|c| c.len()).unwrap_or(0);
        self.staged - self.stage_pos + queued
    }

    fn flush(&mut self) {
        self.staged = 0;
        self.stage_pos = 0;
        self.pending = None;
        if let Some(queue) = self.elements.as_ref() {
            let mut sink = [Element::default(); STAGE];
            while queue.read(&mut sink) > 0 {}
        }
        self.on = false;
        self.length = 0;
        self.position = 0;
        self.started = None;
    }

    /// Envelope amplitude of one sample.
    ///
    /// The queue is consulted only when the caller allows it. A send session
    /// renders what the paddle produced, and reaching into the queue there would
    /// play material the session had already abandoned.
    #[inline]
    fn advance(&mut self, rate: f32, use_queue: bool) -> f32 {
        self.started = None;
        if self.position >= self.length && !self.pop(rate, use_queue) {
            return 0.0;
        }
        let amplitude = if self.on {
            self.envelope.amplitude(self.position, self.length)
        } else {
            0.0
        };
        self.position += 1;
        amplitude
    }

    #[inline]
    fn pop(&mut self, rate: f32, use_queue: bool) -> bool {
        let element = match self.pending.take() {
            Some(e) => e,
            None => {
                if !use_queue {
                    self.on = false;
                    self.length = 0;
                    self.position = 0;
                    return false;
                }
                if self.stage_pos >= self.staged {
                    self.staged = match self.elements.as_ref() {
                        Some(queue) => queue.read(&mut self.stage),
                        None => 0,
                    };
                    self.stage_pos = 0;
                    if self.staged == 0 {
                        self.on = false;
                        self.length = 0;
                        self.position = 0;
                        return false;
                    }
                }
                let e = self.stage[self.stage_pos];
                self.stage_pos += 1;
                e
            }
        };

        // Rounded rather than truncated, and floored at one: an element the
        // rounding took to nought would be skipped silently and the character
        // would come out one element short.
        let length = ((element.seconds * rate).round() as usize).max(1);
        let ideal = ((element.ideal_seconds * rate).round() as usize).max(1);

        self.on = element.on;
        self.length = length;
        self.position = 0;
        self.started = Some((ideal as u32, element.sync));
        true
    }

    /// Advances the carrier and returns it.
    #[inline]
    fn tone(&mut self, step: f64) -> f32 {
        // The phase runs whether or not the tone is on, and is wrapped rather
        // than left to grow: a session of an hour at six hundred hertz would
        // otherwise reach two million radians, where a double has lost the
        // precision the sine needs.
        self.phase += step;
        if self.phase > std::f64::consts::TAU {
            self.phase -= std::f64::consts::TAU;
        }
        self.phase.sin() as f32
    }
}

pub struct Generator {
    rate: f32,
    live: std::sync::Arc<Live>,
    tone: ToneSnapshot,

    primary: Voice,
    interference: Voice,
    /// What the key produces, beside the material rather than instead of it.
    ///
    /// ## Why the two cannot share a voice
    ///
    /// One voice makes the key and the material exclusive, which was true while
    /// the only keying exercise withheld the sound. An exercise that plays a
    /// character and takes it back on the key is both at once, and with a single
    /// voice the second use silences the first: the material is queued, the key
    /// claims the voice, and nothing is ever heard.
    sidetone: Voice,
    conditions: Conditions,

    paddle: std::sync::Arc<Paddle>,
    keyer: PaddleKeyer,
    gate: Gate,
    /// True while the previous buffer rendered the paddle rather than the queue.
    was_sending: bool,

    scope: Producer<f32>,
    edges: Producer<Edge>,
    keyed: Producer<Keyed>,
    scope_batch: Vec<f32>,
    /// Samples one envelope entry covers.
    scope_step: f32,
    scope_accum: f32,
    /// Largest envelope value inside the entry being accumulated.
    ///
    /// The peak rather than the mean, because an element shorter than one entry
    /// would otherwise average away and the picture would lose the very dot that
    /// was too short.
    scope_peak: f32,

    frames: u64,
}

impl Generator {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        rate: u32,
        live: std::sync::Arc<Live>,
        paddle: std::sync::Arc<Paddle>,
        primary: Consumer<Element>,
        interference: Consumer<Element>,
        scope: Producer<f32>,
        edges: Producer<Edge>,
        keyed: Producer<Keyed>,
    ) -> Generator {
        let rate = rate.max(1) as f32;
        let tone = live.read_tone();
        let mut generator = Generator {
            rate,
            live,
            tone,
            primary: Voice::new(Some(primary)),
            interference: Voice::new(Some(interference)),
            sidetone: Voice::new(None),
            conditions: Conditions::new(rate as u32),
            paddle,
            keyer: PaddleKeyer::new(rate),
            gate: Gate::new(),
            was_sending: false,
            scope,
            edges,
            keyed,
            scope_batch: Vec::with_capacity(SCOPE_BATCH),
            scope_step: rate / SCOPE_RATE,
            scope_accum: 0.0,
            scope_peak: 0.0,
            frames: 0,
        };
        generator
            .primary
            .envelope
            .configure(tone.shape, tone.rise_ms, tone.fall_ms, rate);
        generator
            .interference
            .envelope
            .configure(tone.shape, tone.rise_ms, tone.fall_ms, rate);
        generator
            .sidetone
            .envelope
            .configure(tone.shape, tone.rise_ms, tone.fall_ms, rate);
        generator
    }

    /// Elements waiting for the station being copied.
    pub fn pending(&self) -> u32 {
        self.primary.pending_count() as u32
    }

    /// Elements waiting for the interfering station.
    ///
    /// Read so the interface knows whether to feed it: a queue that empties would
    /// leave the interference silent, which is a condition that switched itself
    /// off rather than one the operator turned down.
    pub fn interference_pending(&self) -> u32 {
        self.interference.pending_count() as u32
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Discards everything queued and stops the elements in progress.
    ///
    /// The tone is cut rather than faded, which is what a stop is: an operator
    /// who pressed stop meant now. The click that follows is the one place in this
    /// application where a hard edge is correct, because it marks the end of the
    /// exercise rather than the end of an element.
    pub fn flush(&mut self) {
        self.primary.flush();
        self.interference.flush();
        self.sidetone.flush();
        self.conditions.reset();
        self.keyer.reset();
        self.gate.reset();
    }

    /// Fills one device buffer.
    pub fn render(&mut self, dst: &mut [u8], frames: usize, format: Format) {
        let tone = self.live.read_tone();
        if tone.shape != self.tone.shape
            || tone.rise_ms != self.tone.rise_ms
            || tone.fall_ms != self.tone.fall_ms
        {
            self.primary
                .envelope
                .configure(tone.shape, tone.rise_ms, tone.fall_ms, self.rate);
            self.interference
                .envelope
                .configure(tone.shape, tone.rise_ms, tone.fall_ms, self.rate);
            self.sidetone
                .envelope
                .configure(tone.shape, tone.rise_ms, tone.fall_ms, self.rate);
        }
        self.tone = tone;

        let paddle = self.paddle.read();
        let sending = paddle.sending;
        if sending {
            self.keyer
                .configure(paddle.mode, paddle.dot_seconds, paddle.weight);
        }
        if sending != self.was_sending {
            // The key alone. Flushing the material here was correct while the
            // two were exclusive and is wrong now: in an echo exercise the
            // character and the answer are one exercise, and cutting the first
            // when the second begins is cutting it in half.
            self.sidetone.flush();
            self.keyer.reset();
            self.gate.reset();
            self.was_sending = sending;
        }
        let straight = sending && paddle.mode == PaddleMode::Straight;

        let snap = self.live.read_conditions();
        self.conditions.configure(snap, tone.pitch_hz, tone.gain);
        self.conditions.advance(frames as f32 / self.rate);

        // Linear rather than constant power. At the centre both channels are at
        // unity, so moving the balance changes where the note is and not how loud
        // it is; a constant power law would drop the centre by three decibels and
        // the operator would hear the pan control as a volume control.
        let left = (1.0 + tone.pan).min(1.0);
        let right = (1.0 - tone.pan).min(1.0);
        // The interference and the noise go to the other ear. Binaural separation
        // is the technique the pan control exists for, and it is only a technique
        // if the two are on opposite sides: both in the same ear would be a
        // quieter version of the same exercise.
        let other_left = right;
        let other_right = left;

        // The conditions describe a path. Material arrives down one and a
        // sidetone does not, so the drift, the fading and the second station
        // apply to the first, and the key is heard exactly as it was keyed.
        let drift = self.conditions.drift_hz();
        let pitch = tone.pitch_hz + drift;
        let step = std::f64::consts::TAU * pitch as f64 / self.rate as f64;
        let side_step = std::f64::consts::TAU * tone.pitch_hz as f64 / self.rate as f64;
        let qrm_step = std::f64::consts::TAU
            * (pitch + self.conditions.qrm_offset_hz()) as f64
            / self.rate as f64;
        let qrm_gain = self.conditions.qrm_gain(tone.gain);

        // The band is silenced while the operator is keying into an empty queue,
        // which is the whole of a recall exercise: there is no path for the
        // conditions to describe, and noise under a sidetone is noise under
        // something that never travelled.
        let receiving = !sending || self.primary.pending_count() > 0;

        let stride = format.stride();
        for i in 0..frames {
            // The material, always. The key no longer displaces it, so a
            // character can be played and then keyed back inside one exercise.
            let envelope = self.primary.advance(self.rate, true);
            if let Some((ideal, sync)) = self.primary.started {
                self.edges.write(&[Edge {
                    at: self.frames,
                    ideal_samples: ideal,
                    sync,
                }]);
            }

            let side = if sending {
                let contacts = self.paddle.sample();
                let tick = self.keyer.tick(contacts, self.frames);
                if tick.consumed {
                    // The latch is cleared by whoever acted on it, which is what
                    // lets a tap shorter than a buffer survive the gap between
                    // two wakings of this thread.
                    self.paddle.consume();
                }
                if let Some(element) = tick.element {
                    self.sidetone.pending = Some(element);
                }
                if let Some(edge) = tick.edge {
                    // The same queue the material writes into, so the picture
                    // shows the character and the answer on one timeline.
                    self.edges.write(&[edge]);
                }
                // The gap first, because it belongs to the character that has
                // just ended and the mark to the one beginning.
                if let Some(event) = tick.gap {
                    self.keyed.write(&[event]);
                }
                if let Some(event) = tick.mark {
                    self.keyed.write(&[event]);
                }
                if straight {
                    // The gate rather than an element: with a hand key the length
                    // is not known until the contact opens.
                    self.gate.advance(self.keyer.gated(), &self.sidetone.envelope)
                } else {
                    self.sidetone.advance(self.rate, false)
                }
            } else {
                0.0
            };

            let fade = self.conditions.signal_gain();
            let signal = if envelope > 0.0 {
                self.primary.tone(step) * envelope * fade * tone.gain
            } else {
                // The phase still advances, so a gap does not restart the
                // carrier and the next element continues rather than clicking.
                self.primary.tone(step);
                0.0
            };
            let keyed_tone = if side > 0.0 {
                self.sidetone.tone(side_step) * side * tone.gain
            } else {
                self.sidetone.tone(side_step);
                0.0
            };
            let voice = signal + keyed_tone;

            let interference = if qrm_gain > 0.0 && receiving {
                let e = self.interference.advance(self.rate, true);
                let carrier = self.interference.tone(qrm_step);
                if e > 0.0 {
                    carrier * e * qrm_gain
                } else {
                    0.0
                }
            } else {
                0.0
            };
            let background = if receiving {
                interference + self.conditions.additive()
            } else {
                0.0
            };

            let base = i * stride;
            let frame = &mut dst[base..base + stride];
            write_sample(frame, format, 0, voice * left + background * other_left);
            if format.channels > 1 {
                write_sample(frame, format, 1, voice * right + background * other_right);
            }
            // A surround endpoint is left silent beyond the pair. Replicating
            // would put the note in five places at once, which is louder rather
            // than wider.
            for channel in 2..format.channels {
                write_sample(frame, format, channel, 0.0);
            }

            self.frames += 1;
            // The louder of the two, so the picture carries the character and
            // the answer to it as one trace: an echo exercise is read by
            // comparing them, and two pictures would not overlay.
            self.accumulate(envelope.max(side));
        }

        // Once per buffer rather than per sample. The reader draws at the frame
        // rate, so a store per sample would be nine hundred and sixty writes to
        // satisfy sixty reads.
        self.paddle.set_live(
            if sending { self.keyer.live_mark() } else { 0 },
            self.keyer.dot_samples(),
        );

        self.publish_scope();
    }

    /// Folds one sample into the envelope entry being built.
    ///
    /// The envelope of the station being copied and nothing else: the picture is
    /// about the keying, and adding the noise to it would be drawing a spectrum
    /// analyser with one bin.
    #[inline]
    fn accumulate(&mut self, amplitude: f32) {
        if amplitude > self.scope_peak {
            self.scope_peak = amplitude;
        }
        self.scope_accum += 1.0;
        if self.scope_accum < self.scope_step {
            return;
        }
        self.scope_accum -= self.scope_step;
        if self.scope_batch.len() < SCOPE_BATCH {
            self.scope_batch.push(self.scope_peak);
        }
        self.scope_peak = 0.0;
    }

    fn publish_scope(&mut self) {
        if self.scope_batch.is_empty() {
            return;
        }
        // A batch that does not fit is dropped whole rather than in part: a
        // partial write would leave the picture with a gap it cannot see, which
        // reads as an element that was never sent.
        self.scope.write(&self.scope_batch);
        self.scope_batch.clear();
    }
}

/// Writes one channel of one frame in the device format.
#[inline]
fn write_sample(frame: &mut [u8], format: Format, channel: usize, value: f32) {
    let at = channel * format.bytes;
    // Clipped rather than wrapped. A wrap turns an overload into full scale noise
    // of the opposite sign, which is far louder than the overload and through
    // headphones is unpleasant enough to matter.
    let clamped = value.clamp(-1.0, 1.0);
    match format.kind {
        SampleKind::F32 => {
            frame[at..at + 4].copy_from_slice(&clamped.to_le_bytes());
        }
        SampleKind::I16 => {
            let v = (clamped * 32767.0) as i16;
            frame[at..at + 2].copy_from_slice(&v.to_le_bytes());
        }
        SampleKind::I32 => {
            // A container of twenty four valid bits is filled to the top of the
            // container, which is what every endpoint that reports one expects:
            // the low bits are ignored rather than misread.
            let v = (clamped as f64 * 2_147_483_647.0) as i32;
            frame[at..at + 4].copy_from_slice(&v.to_le_bytes());
        }
    }
}