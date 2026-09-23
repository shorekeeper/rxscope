//! Practice session.
//!
//! Owns the whole loop between the material and the student: what is being sent,
//! what has been typed, when the answer is taken, what it scored, and where the
//! level goes next. The application feeds it time and keystrokes and reads a view
//! of it; nothing else knows the phases.
//!
//! ## Why the answer is taken after the group has sounded
//!
//! A student copying at speed types during the gaps, so keystrokes have to be
//! accepted while the group is still playing. But the answer cannot be taken
//! then: the last character has not been heard, and submitting on a count would
//! score a group the student was still listening to.
//!
//! So the group is sent, the keystrokes accumulate throughout, and the answer is
//! taken once the last character has finished sounding and the student has either
//! pressed enter, filled the group, or run out of time.
//!
//! ## Two units of one answer
//!
//! Ordinary material is scored character by character against one copy of what
//! was sent. A structured exchange is scored field by field, because that is what
//! a contact is judged on: an operator who logged the wrong serial made an error,
//! and one who did not hear the `TU` did not. The per character history is fed
//! from both, because a `K` missed inside a callsign is the same evidence about
//! that character as a `K` missed inside a group.
//!
//! ## What a repetition does to the answer
//!
//! With the repeat setting above one, the same material is sent again before the
//! answer is taken, which is what an operator asking for `AGN` receives. So the
//! answer covers material heard several times while remaining one copy long, and
//! that is the whole of what the setting changes: the count of characters reached
//! and the count of characters expected in the answer stop being the same number.
//!
//! Reaction times are not recorded across a repetition. The interval from a
//! character to the keystroke that answered it means something when the character
//! was heard once and nothing when it was heard three times.
//!
//! ## Where the level moves
//!
//! On the accuracy over the window rather than on the group just scored. One bad
//! group is noise, and a level that moved on it would oscillate. The withdrawal
//! matters as much as the advance: practising a set that cannot be copied
//! improves nothing, and a level that only rises turns a bad evening into a wall.

pub mod score;

use std::collections::VecDeque;

use crate::config::settings::{
    DrillMode, DrillUnit, MaterialSettings, MaterialSource, PracticeMode, Settings, TextCase,
};
use crate::lesson::{character_set, Material};
use crate::morse;
use crate::progress::Progress;
use crate::qso::Exchange;
use crate::synth::{Element, Keyer};

use score::{Op, Outcome};

/// Grace after the last character before the answer is taken on a count.
///
/// A student who filled the group as the last element sounded may still be about
/// to correct themselves, and taking the answer inside their own keystroke reads
/// as the trainer refusing the last character.
const FILLED_GRACE_S: f32 = 0.35;

/// Elements below which the queue is topped up.
///
/// A group of five characters is around forty elements, so this keeps roughly one
/// group queued behind the one playing. Deeper would delay a timing change by the
/// depth, because an element already queued keeps the speed it was made at.
const QUEUE_LOW: u32 = 48;

/// Characters the transcript holds in listening mode.
const TRANSCRIPT_CHARS: usize = 256;

/// Answers the log keeps.
///
/// A scored group vanishes at the moment it becomes worth reading, and nothing
/// else in this application holds one. Sixteen is a few minutes of practice,
/// which is as far back as a correction is still worth making.
const LOG_DEPTH: usize = 16;

/// One answer that has been scored.
#[derive(Debug, Clone)]
pub struct ScoredGroup {
    pub sent: String,
    pub typed: String,
    /// True when every character of it was copied.
    pub right: bool,
}

/// Longest interval that counts as a reaction, in milliseconds.
///
/// Beyond this the student looked away rather than answered slowly, and the
/// figure would dominate an average built from hundreds of answers of a few
/// hundred milliseconds each.
const REACTION_LIMIT_MS: f32 = 4000.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Idle,
    /// The material is being keyed.
    Sending,
    /// It has finished sounding and the answer is awaited.
    Answering,
    /// The answer has been scored and is being shown.
    Reveal,
}

impl Phase {
    fn key(self) -> &'static str {
        match self {
            Phase::Idle => "status.idle",
            Phase::Sending => "status.sending",
            Phase::Answering => "status.answering",
            Phase::Reveal => "status.reveal",
        }
    }
}

/// One character that has been keyed.
struct Heard {
    ch: char,
    /// Session clock at which it finished sounding.
    end: f32,
    /// Answer field it belongs to, nothing for material with no fields and for
    /// the wording of an exchange.
    field: Option<usize>,
}

/// One keystroke.
struct Typed {
    ch: char,
    at: f32,
}

/// Verdict of one sent character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Right,
    /// Another character was written for it.
    Wrong,
    Missed,
    /// Sent, and not part of the answer.
    ///
    /// Only an exchange produces these, and they are the point of the exchange
    /// being a separate source: the wording of a contact is heard and is not
    /// something the student is marked on.
    Unscored,
}

/// Verdict of one answer field.
#[derive(Debug, Clone, Copy)]
pub struct FieldMark {
    /// Localization key of what the field carries.
    pub key: &'static str,
    /// What was sent.
    pub sent: char,
    /// True when every character of it was copied.
    pub right: bool,
}

/// What the interface shows.
pub struct SessionView {
    pub running: bool,
    /// Localization key of the phase.
    pub phase: &'static str,
    pub elapsed: f32,
    pub remaining: f32,
    pub groups: u32,
    pub characters: u64,
    /// Share of sent characters copied this session.
    pub accuracy: f32,
    /// Mean reaction time this session, in milliseconds.
    ///
    /// Nought while nothing has been timed, which includes every session run with
    /// a repetition: an interval measured across three hearings is not a reaction.
    pub reaction_ms: f32,
    pub inserted: u32,
    /// What was sent, empty while it is being withheld.
    pub sent: String,
    /// What has been typed.
    pub typed: String,
    /// Per character verdict of the last scored answer, aligned to the sent text.
    pub marks: Vec<Mark>,
    /// True while the sent text is being shown as an answer rather than as a
    /// running transcript.
    pub scored: bool,

    /// True while the material being sent is a structured exchange.
    pub exchange: bool,
    /// What the token means, for the sources that carry a meaning.
    ///
    /// Shown beside the answer rather than before it: a gloss before the answer
    /// would be the answer.
    pub gloss: Option<&'static str>,
    /// Why the source produced something other than what it promises.
    pub fallback: Option<&'static str>,
    /// Character the student is expected to key next, while sending.
    pub next_char: Option<char>,
    /// Fields the exchange asks for, and what became of them.
    ///
    /// Empty for ordinary material. Present for an exchange whether or not it has
    /// been scored yet, so the student can see what they are being asked for.
    pub fields: Vec<FieldMark>,
    pub fields_right: u32,
    pub fields_total: u32,

    /// Copies of the material sent for this answer, and which one is playing.
    pub copy: u32,
    pub copies: u32,
    /// True when asking for the material again would do something, which needs
    /// material that is played and an answer that is asked for.
    pub repeatable: bool,

    /// Drill in force, which decides what the prompt may show.
    pub drill: DrillMode,
    /// Material of the answer in progress, one character or one word.
    pub target: String,
    /// True while the material is withheld, which the blind exercise is.
    pub target_hidden: bool,
    /// True while the pattern is withheld, which the recall exercise is.
    ///
    /// Held apart from the field above because the two exercises hide opposite
    /// halves: one shows the character and asks for the pattern, the other plays
    /// the pattern and asks for the character.
    pub pattern_hidden: bool,
    /// Answers scored this session, oldest first.
    pub log: Vec<ScoredGroup>,
}

pub struct Session {
    phase: Phase,
    keyer: Keyer,
    material: Material,
    exchanges: crate::qso::Generator,

    /// Characters keyed for the answer in progress.
    ///
    /// One copy only. A repetition is heard again and is not answered again, so a
    /// second copy here would double every sent character in the alignment.
    heard: Vec<Heard>,
    typed: Vec<Typed>,
    /// Text queued but not yet reached by the generator.
    queued: String,
    /// Answer field of every queued character, spaces excluded.
    field_of: VecDeque<Option<usize>>,
    /// Characters one copy of the answer holds.
    expecting: usize,
    /// Characters reached across every copy.
    ///
    /// Held apart from the length of the heard list, which stops at one copy.
    reached: usize,
    /// Session clock at which the last character reached finished sounding.
    last_end: f32,

    /// Text of the material, so a repetition sends the same thing.
    group: String,
    /// Answer field of every character of it.
    group_fields: Vec<Option<usize>>,
    /// Copies still to be queued.
    repeats_left: u32,
    /// Copies this answer covers, latched when the material was drawn.
    copies: u32,

    /// Fields the exchange in progress asks for.
    fields: Vec<FieldMark>,
    field_text: Vec<String>,
    fields_right: u32,
    is_exchange: bool,
    /// Meaning of the token being sent, when it has one.
    gloss: Option<&'static str>,

    clock: f32,
    /// Seconds left in the phase, for answering and revealing.
    phase_left: f32,
    /// Session clock at which the material finished sounding.
    finished_at: f32,

    groups: u32,
    characters: u64,
    correct: u32,
    wrong: u32,
    inserted: u32,
    scored_fields: u32,
    scored_fields_right: u32,
    reaction_sum: f32,
    reaction_n: u32,

    marks: Vec<Mark>,
    scored: bool,
    /// Answers begun, so a reader can tell one from the next.
    ///
    /// Read by the decoder, which accumulates elements towards a character: those
    /// belong to the answer they were keyed into, and carrying them across a
    /// scored group would make the first character of the next one begin with the
    /// tail of the last.
    epoch: u32,
    /// Running transcript, for listening mode where there is no answer to score.
    transcript: String,
    log: VecDeque<ScoredGroup>,
}

impl Session {
    pub fn new() -> Session {
        Session {
            phase: Phase::Idle,
            keyer: Keyer::new(),
            material: Material::new(),
            exchanges: crate::qso::Generator::new(),
            heard: Vec::with_capacity(48),
            typed: Vec::with_capacity(48),
            queued: String::with_capacity(96),
            field_of: VecDeque::with_capacity(96),
            expecting: 0,
            reached: 0,
            last_end: 0.0,
            group: String::with_capacity(96),
            group_fields: Vec::with_capacity(96),
            repeats_left: 0,
            copies: 1,
            fields: Vec::with_capacity(4),
            field_text: Vec::with_capacity(4),
            fields_right: 0,
            is_exchange: false,
            gloss: None,
            clock: 0.0,
            phase_left: 0.0,
            finished_at: 0.0,
            groups: 0,
            characters: 0,
            correct: 0,
            wrong: 0,
            inserted: 0,
            scored_fields: 0,
            scored_fields_right: 0,
            reaction_sum: 0.0,
            reaction_n: 0,
            marks: Vec::with_capacity(48),
            scored: false,
            epoch: 0,
            transcript: String::with_capacity(TRANSCRIPT_CHARS + 32),
            log: VecDeque::with_capacity(LOG_DEPTH),
        }
    }

    pub fn is_running(&self) -> bool {
        self.phase != Phase::Idle
    }

    /// Characters the generator is drawing from.
    pub fn pool(&self) -> String {
        self.material.pool()
    }

    pub fn start(&mut self, settings: &Settings) {
        self.phase = Phase::Sending;
        self.clock = 0.0;
        self.phase_left = 0.0;
        self.finished_at = 0.0;
        self.groups = 0;
        self.characters = 0;
        self.correct = 0;
        self.wrong = 0;
        self.inserted = 0;
        self.scored_fields = 0;
        self.scored_fields_right = 0;
        self.reaction_sum = 0.0;
        self.reaction_n = 0;
        self.transcript.clear();
        self.log.clear();
        self.clear_answer();

        crate::log_info!(
            "session",
            "started, {} characters at {:.0}/{:.0} wpm, {} s, repeat {}",
            character_set(&settings.lesson).chars().count(),
            settings.timing.char_wpm,
            if settings.timing.farnsworth {
                settings.timing.text_wpm
            } else {
                settings.timing.char_wpm
            },
            settings.lesson.session_seconds,
            settings.material.repeat
        );
    }

    /// Ends the session, records it, and moves the level.
    pub fn stop(&mut self, settings: &mut Settings, progress: &mut Progress) {
        if self.phase == Phase::Idle {
            return;
        }
        self.phase = Phase::Idle;
        self.record(settings, progress);
        crate::log_info!(
            "session",
            "ended, {:.0} s, {} characters, {:.0} percent",
            self.clock,
            self.characters,
            self.accuracy() * 100.0
        );
    }

    /// Writes the session down and moves the level.
    fn record(&mut self, settings: &mut Settings, progress: &mut Progress) {
        let accuracy = self.accuracy();
        let reaction = self.mean_reaction();
        progress.log_session(
            &settings.progress,
            settings.lesson.level,
            settings.timing.char_wpm,
            if settings.timing.farnsworth {
                settings.timing.text_wpm
            } else {
                settings.timing.char_wpm
            },
            self.characters,
            accuracy,
            reaction,
        );

        if settings.lesson.auto_advance && self.characters > 0 {
            self.move_level(settings, progress);
        }
        if let Err(e) = progress.save() {
            crate::log_warn!("session", "cannot save the history: {}", e);
        }
    }

    /// Raises or lowers the level on the windowed accuracy of the current set.
    fn move_level(&self, settings: &mut Settings, progress: &Progress) {
        // A stated set has no level, so there is nothing to move: the operator
        // wrote out what they wanted.
        if settings.lesson.method == crate::config::settings::LessonMethod::Custom {
            return;
        }
        // Nor has an exchange or a callsign, both of which draw from the whole
        // alphabet: the accuracy over the current set says nothing about material
        // that ignored it.
        if matches!(
            settings.material.source,
            MaterialSource::Qso | MaterialSource::Callsigns
        ) {
            return;
        }
        let set = character_set(&settings.lesson);
        let accuracy = match progress.set_accuracy(&set, settings.progress.window) {
            Some(a) => a,
            // Too little practice to judge, which is the ordinary case after a
            // very short session.
            None => return,
        };

        if accuracy >= settings.lesson.advance_accuracy {
            let ceiling = morse::KOCH_ORDER.chars().count() as u32;
            if settings.lesson.level < ceiling {
                settings.lesson.level += 1;
                crate::log_info!(
                    "session",
                    "level {} at {:.0} percent",
                    settings.lesson.level,
                    accuracy * 100.0
                );
            }
        } else if accuracy < settings.lesson.regress_accuracy && settings.lesson.level > 2 {
            settings.lesson.level -= 1;
            crate::log_info!(
                "session",
                "level {} at {:.0} percent",
                settings.lesson.level,
                accuracy * 100.0
            );
        }
    }

    /// A character has begun to sound.
    ///
    /// The application converts the sample index into a session time and calls
    /// this; the session pops the character from the front of the queue, because
    /// the generator plays what it was handed in the order it was handed.
    ///
    /// The end of the character rather than its start, because that is when the
    /// ear has heard enough to answer. Computed from the pattern rather than
    /// measured: the difference is the jitter, which is a few milliseconds
    /// against a reaction time of hundreds.
    ///
    /// The character is returned so the picture can be labelled: the caller has
    /// the sample index the burst began at and no way to know what it was, and
    /// this is the one place where the two are both in hand.
    pub fn on_character(&mut self, start: f32, settings: &Settings) -> Option<char> {
        while self.queued.starts_with(' ') {
            self.queued.remove(0);
            if self.wants_more() {
                self.heard.push(Heard { ch: ' ', end: start, field: None });
            }
            self.transcript.push(' ');
        }
        if self.queued.is_empty() {
            return None;
        }

        // A prosign travels as a token: one shape for the ear and one character
        // for the count.
        let (ch, pattern) = if self.queued.starts_with('<') {
            match self.queued.find('>') {
                Some(end) => {
                    let token: String = self.queued.drain(..=end).collect();
                    let name = &token[1..token.len() - 1];
                    // Represented by its first letter in the answer, because a
                    // student cannot type angle brackets while copying.
                    let ch = name.chars().next().unwrap_or('?');
                    (ch, morse::prosign(name).unwrap_or("."))
                }
                None => return None,
            }
        } else {
            let ch = self.queued.remove(0);
            (ch, morse::pattern_of(ch).unwrap_or("."))
        };

        let field = self.field_of.pop_front().unwrap_or(None);
        let end = start + character_seconds(pattern, settings);
        self.last_end = end;
        self.reached += 1;
        self.characters += 1;

        if self.wants_more() {
            self.heard.push(Heard { ch, end, field });
        }

        self.transcript.push(ch);
        while self.transcript.chars().count() > TRANSCRIPT_CHARS {
            self.transcript.remove(0);
        }
        Some(ch)
    }

    /// True while the answer still needs characters recorded against it.
    ///
    /// False once one copy has been heard, which is what keeps a repetition out
    /// of the alignment: the second hearing is the same characters and adding
    /// them would double every sent character in the comparison.
    fn wants_more(&self) -> bool {
        self.expecting == 0 || self.marks_heard() < self.expecting
    }

    fn marks_heard(&self) -> usize {
        self.heard.iter().filter(|h| h.ch != ' ').count()
    }

    /// One typed character.
    pub fn on_char(&mut self, ch: char, settings: &Settings) {
        if !matches!(self.phase, Phase::Sending | Phase::Answering) {
            return;
        }
        // A typed answer in a keying exercise scores an exercise that was
        // avoided rather than one that was passed.
        if settings.practice.keying() || settings.practice.listening() {
            return;
        }
        self.accept(ch, settings);
    }

    /// One character the paddle decoder produced.
    pub fn on_keyed(&mut self, ch: char, settings: &Settings) {
        if !settings.practice.keying() {
            return;
        }
        if !matches!(self.phase, Phase::Sending | Phase::Answering) {
            return;
        }
        self.accept(ch, settings);
    }

    fn accept(&mut self, ch: char, settings: &Settings) {
        let ch = match settings.practice.case {
            TextCase::Lower => ch.to_ascii_lowercase(),
            TextCase::Upper => ch.to_ascii_uppercase(),
        };
        // Only what could have been sent. A key that carries no pattern is a
        // mistake at the keyboard rather than a wrong answer, and counting it
        // would blame the ear for the hand.
        if morse::pattern_of(ch).is_none() && ch != ' ' {
            return;
        }
        if ch == ' ' {
            // A space means something only where the material has fields. In a
            // group it is a pause, and the paddle produces one from any silence
            // long enough to look like a word gap: counting it as an answer
            // scored a Q code after two of its three characters.
            if !self.is_exchange {
                return;
            }
            // A leading space would open an empty field, which for an exchange
            // means every answer after it lands one slot late.
            if self.typed.is_empty() || self.typed.last().map(|t| t.ch) == Some(' ') {
                return;
            }
        }
        self.typed.push(Typed { ch, at: self.clock });

        // An answer being written is not an answer abandoned. Without this a
        // student keying a long group by hand runs out of time in the middle of
        // it, which reads as the trainer refusing an answer they were giving.
        if self.phase == Phase::Answering {
            self.phase_left = settings.practice.answer_timeout_ms as f32 * 0.001;
        }
    }

    /// Removes the last keystroke, when the setting allows it.
    pub fn on_backspace(&mut self, settings: &Settings) {
        if settings.practice.allow_backspace {
            self.typed.pop();
        }
    }

    /// Takes the answer now.
    pub fn on_submit(&mut self, settings: &Settings, progress: &mut Progress) {
        if self.phase == Phase::Answering {
            self.score(settings, progress);
        }
    }

    /// Abandons the material and moves on without scoring it.
    pub fn skip(&mut self) {
        if self.phase == Phase::Idle {
            return;
        }
        self.next_group();
    }

    /// Sends the same material again before the answer is taken.
    ///
    /// What an operator receives after asking for `AGN`, on demand rather than by
    /// a setting. What has been typed is kept: a repetition is a second hearing
    /// of the same thing, and discarding a half written answer would punish the
    /// student for asking.
    ///
    /// Refused while the student is the one sending, where there is nothing to
    /// hear again, and while nothing is in flight.
    pub fn again(&mut self, settings: &Settings) {
        if self.expecting == 0 {
            return;
        }
        if !settings.practice.sounded() || settings.practice.listening() {
            return;
        }
        self.repeats_left += 1;
        self.copies += 1;
        if self.phase == Phase::Answering {
            self.phase = Phase::Sending;
        }
    }

    /// Character the student should key next.
    ///
    /// Counted from what has been typed rather than from a cursor, because the
    /// two would have to agree and one of them would be redundant. Nothing once
    /// the answer is as long as the material, which is the moment the student has
    /// finished rather than the moment they got it right.
    fn next_expected(&self) -> Option<char> {
        let sent: Vec<char> = self.heard.iter().filter(|h| h.ch != ' ').map(|h| h.ch).collect();
        let typed = self.typed.iter().filter(|t| t.ch != ' ').count();
        sent.get(typed).copied()
    }

    /// One frame.
    pub fn update(
        &mut self,
        dt: f32,
        stream: &crate::audio::OutputStream,
        settings: &Settings,
        progress: &mut Progress,
    ) {
        if self.phase == Phase::Idle {
            return;
        }
        self.clock += dt;

        // The session ends between answers rather than inside one, so the last
        // answer is scored rather than abandoned halfway.
        let over = self.clock >= settings.lesson.session_seconds as f32;

        match self.phase {
            Phase::Idle => {}
            Phase::Sending => {
                self.feed(stream, settings, progress);
                // With nothing to play the material is ready as soon as it
                // exists, so the phase waits for the operator instead of for
                // the loudspeaker.
                if !settings.practice.sounded() {
                    if self.expecting > 0 {
                        self.phase = Phase::Answering;
                        self.finished_at = self.clock;
                        self.phase_left = settings.practice.answer_timeout_ms as f32 * 0.001;
                    }
                    return;
                }
                // Every copy has been reached and the last character of the last
                // one has finished.
                let wanted = self.expecting * self.copies.max(1) as usize;
                let done = self.expecting > 0
                    && self.reached >= wanted
                    && self.clock >= self.last_end;
                if done {
                    if settings.practice.listening() {
                        self.next_group();
                        if over {
                            self.phase = Phase::Idle;
                        }
                    } else {
                        self.phase = Phase::Answering;
                        self.finished_at = self.clock;
                        self.phase_left = settings.practice.answer_timeout_ms as f32 * 0.001;
                    }
                }
            }
            Phase::Answering => {
                self.phase_left -= dt;

                // An exchange is filled when its last field has been started, not
                // when a character count is reached: the fields differ in length
                // and the student separates them.
                //
                // Everything else counts characters and nothing but characters. A
                // space is a pause, and a pause is not part of the answer.
                let answered = self.typed.iter().filter(|t| t.ch != ' ').count();
                let complete = if self.is_exchange {
                    let groups = self.typed_groups().len();
                    groups > self.field_text.len()
                        || (groups == self.field_text.len()
                            && self.typed.last().map(|t| t.ch) == Some(' '))
                } else {
                    answered >= self.expecting
                };

                // The grace runs from whichever came later, the material ending
                // or the last thing the student did. Measured from the material
                // alone it has already expired by the time a hand keyed answer
                // reaches its last character, and the answer is taken inside the
                // student's own keystroke.
                let touched = self
                    .typed
                    .last()
                    .map(|t| t.at)
                    .unwrap_or(self.finished_at)
                    .max(self.finished_at);
                let settled = self.clock - touched >= FILLED_GRACE_S;

                if self.phase_left <= 0.0 || (complete && settled) {
                    self.score(settings, progress);
                }
            }
            Phase::Reveal => {
                self.phase_left -= dt;
                if self.phase_left <= 0.0 {
                    if over {
                        self.phase = Phase::Idle;
                    } else {
                        self.next_group();
                    }
                }
            }
        }
    }

    /// True when the session ended on its own and has not been recorded.
    pub fn expired(&self) -> bool {
        self.phase == Phase::Idle && self.characters > 0
    }

    /// Records and clears, after an expiry.
    pub fn finish(&mut self, settings: &mut Settings, progress: &mut Progress) {
        self.record(settings, progress);
        crate::log_info!(
            "session",
            "session over, {} characters, {:.0} percent",
            self.characters,
            self.accuracy() * 100.0
        );
        // The counters are cleared so the expiry is not reported twice.
        self.characters = 0;
    }

    /// Tops up the element queue.
    fn feed(
        &mut self,
        stream: &crate::audio::OutputStream,
        settings: &Settings,
        progress: &Progress,
    ) {
        let listening = settings.practice.listening();
        // Whether anything is played, which the practice mode cannot answer on
        // its own: an echo drill sounds the material and still takes a key.
        let silent = !settings.practice.sounded();

        // Material is already in flight for this answer. Either a repetition is
        // owed, or there is nothing to do until it has been answered.
        if self.expecting > 0 && !listening {
            if self.repeats_left > 0 && !silent && stream.pending() < QUEUE_LOW {
                let text = self.group.clone();
                let fields = self.group_fields.clone();
                if self.queue(stream, &text, &fields, settings) {
                    self.repeats_left -= 1;
                }
            }
            return;
        }
        if !silent && stream.pending() >= QUEUE_LOW {
            return;
        }

        // A drill carries one unit per prompt, so the material section is
        // replaced for the draw rather than consulted: the section describes a
        // session of groups and the drill is not one.
        let drilled = drill_material(settings);
        let material = drilled.as_ref().unwrap_or(&settings.material);

        // The character the order introduced last. That is what the incremental
        // method means: the set grew by one and the session is for the one that
        // arrived, so an even draw spends a drill on what is already known.
        let focus = if drilled.is_some() && settings.practice.drill_unit == DrillUnit::Character
        {
            character_set(&settings.lesson).chars().last()
        } else {
            None
        };
        self.material.set_focus(focus, settings.practice.drill_focus);

        // The exchange is a different generator, because its unit is a field
        // rather than a character.
        let exchange = material.source == MaterialSource::Qso;
        let (text, fields) = if exchange {
            let built: Exchange = self.exchanges.next();
            let text = built.text();
            let answers = built.answers();
            self.field_text = answers.iter().map(|f| f.text.clone()).collect();
            self.fields = answers
                .iter()
                .map(|f| FieldMark {
                    key: f.kind.key(),
                    sent: f.text.chars().next().unwrap_or('?'),
                    right: false,
                })
                .collect();
            (text, built.character_fields())
        } else {
            let text = self.material.next(
                &settings.lesson,
                material,
                Some(progress),
                settings.progress.window,
                settings.lesson.weak_weight,
            );
            let counted = text.chars().filter(|&c| c != ' ').count();
            self.field_text.clear();
            self.fields.clear();
            self.gloss = self.material.gloss();
            (text, vec![None; counted])
        };
        if text.is_empty() {
            return;
        }
        self.is_exchange = exchange;

        // A repetition is a copy of the material rather than a second answer, so
        // the count is latched here and the answer stays one copy long.
        self.copies = if listening || silent {
            1
        } else {
            material.repeat.clamp(1, 5)
        };

        if silent {
            // The trainer keys nothing: the student does. The material is
            // recorded as though it had been heard, which is what lets the
            // alignment, the marks and the field verdicts be the same machinery
            // here as everywhere else rather than a second implementation.
            //
            // A prosign is read as a token here as well. Taken character by
            // character it would put an angle bracket into the answer, and an
            // angle bracket has no pattern, so the student would be asked to key
            // something that cannot be keyed.
            let mut counted = 0usize;
            let mut index = 0usize;
            let mut rest = text.as_str();
            while !rest.is_empty() {
                let (ch, len) = if let Some(stripped) = rest.strip_prefix('<') {
                    match stripped.find('>') {
                        Some(end) => {
                            let name = &stripped[..end];
                            (name.chars().next().unwrap_or('?'), end + 2)
                        }
                        None => break,
                    }
                } else {
                    let ch = rest.chars().next().unwrap_or(' ');
                    (ch, ch.len_utf8())
                };
                rest = &rest[len..];

                if ch == ' ' {
                    // Kept, because a space is a field boundary in an exchange
                    // and the student separates their answers with one.
                    self.heard.push(Heard { ch: ' ', end: self.clock, field: None });
                    continue;
                }
                let field = fields.get(index).copied().unwrap_or(None);
                index += 1;
                self.heard.push(Heard { ch, end: self.clock, field });
                counted += 1;
            }
            if counted == 0 {
                return;
            }
            self.characters += counted as u64;
            self.reached = counted;
            self.expecting = counted;
            self.group = text.trim().to_string();
            self.group_fields = fields;
            self.repeats_left = 0;
            return;
        }

        if !self.queue(stream, &text, &fields, settings) {
            return;
        }

        let counted = text.chars().filter(|&c| c != ' ').count();
        if listening {
            // Nothing is scored, so there is no answer: the count only serves to
            // notice that the material has been reached.
            self.expecting = 0;
            self.copies = 1;
        } else {
            self.expecting = counted;
            self.group = text.trim().to_string();
            self.group_fields = fields;
            self.repeats_left = self.copies.saturating_sub(1);
        }
    }

    /// Encodes one copy and hands it to the stream.
    ///
    /// False when the queue had no room, which leaves everything untouched: half
    /// a copy in flight would be answered as though the rest had been sent.
    fn queue(
        &mut self,
        stream: &crate::audio::OutputStream,
        text: &str,
        fields: &[Option<usize>],
        settings: &Settings,
    ) -> bool {
        let mut elements: Vec<Element> = Vec::with_capacity(128);
        // A word gap before the material rather than after it, so the last thing
        // of a session does not end with silence the student waits through.
        let spaced = if self.characters == 0 && self.queued.is_empty() {
            text.to_string()
        } else {
            format!(" {}", text)
        };
        let kept = self.keyer.encode(&spaced, &settings.timing, &mut elements);
        if elements.is_empty() || !stream.push(&elements) {
            return false;
        }

        self.queued.push_str(&kept);
        // The keyer drops anything the alphabet does not hold, so the markers are
        // taken from what survived rather than from what was offered: a dropped
        // character would shift every field boundary after it.
        let mut index = 0usize;
        for ch in kept.chars() {
            if ch == ' ' {
                continue;
            }
            self.field_of.push_back(fields.get(index).copied().unwrap_or(None));
            index += 1;
        }
        true
    }

    /// Index ranges of the whitespace separated groups of the answer.
    fn typed_groups(&self) -> Vec<Vec<usize>> {
        let mut out: Vec<Vec<usize>> = Vec::with_capacity(4);
        let mut current: Vec<usize> = Vec::new();
        for (index, entry) in self.typed.iter().enumerate() {
            if entry.ch == ' ' {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            } else {
                current.push(index);
            }
        }
        if !current.is_empty() {
            out.push(current);
        }
        out
    }

    /// Scores the answer and records it.
    fn score(&mut self, settings: &Settings, progress: &mut Progress) {
        if self.is_exchange {
            self.score_exchange(settings, progress);
        } else {
            self.score_group(settings, progress);
        }

        // Before the counters move, so the line describes the answer that was
        // just taken rather than the one after it.
        let right = !self
            .marks
            .iter()
            .any(|m| matches!(m, Mark::Wrong | Mark::Missed));
        self.log.push_back(ScoredGroup {
            sent: self.heard.iter().map(|h| h.ch).collect(),
            typed: self.typed.iter().map(|t| t.ch).collect(),
            right,
        });
        while self.log.len() > LOG_DEPTH {
            self.log.pop_front();
        }

        self.groups += 1;
        self.scored = true;
        self.phase = Phase::Reveal;
        self.phase_left = if settings.practice.reveal {
            settings.practice.reveal_delay_ms as f32 * 0.001
        } else {
            0.0
        };
    }

    /// Ordinary material, compared character by character against one copy.
    fn score_group(&mut self, settings: &Settings, progress: &mut Progress) {
        let sent: Vec<char> = self.heard.iter().filter(|h| h.ch != ' ').map(|h| h.ch).collect();
        let ends: Vec<f32> = self.heard.iter().filter(|h| h.ch != ' ').map(|h| h.end).collect();
        let typed: Vec<char> = self.typed.iter().filter(|t| t.ch != ' ').map(|t| t.ch).collect();
        let times: Vec<f32> = self.typed.iter().filter(|t| t.ch != ' ').map(|t| t.at).collect();

        self.marks.clear();
        self.marks.resize(sent.len(), Mark::Missed);

        let outcome = self.apply(&sent, &ends, &typed, &times, 0, progress);
        self.correct += outcome.correct;
        self.wrong += outcome.wrong;
        if settings.practice.strict {
            self.inserted += outcome.inserted;
        }
    }

    /// A structured exchange, compared field by field.
    fn score_exchange(&mut self, settings: &Settings, progress: &mut Progress) {
        let heard: Vec<(char, f32, Option<usize>)> = self
            .heard
            .iter()
            .filter(|h| h.ch != ' ')
            .map(|h| (h.ch, h.end, h.field))
            .collect();

        self.marks.clear();
        // Everything is unscored until a field claims it, which is the whole
        // statement the exchange makes: the wording of a contact is heard and is
        // not something the student is marked on.
        self.marks.resize(heard.len(), Mark::Unscored);

        let groups = self.typed_groups();
        self.fields_right = 0;

        for field in 0..self.field_text.len() {
            let positions: Vec<usize> = heard
                .iter()
                .enumerate()
                .filter(|(_, (_, _, f))| *f == Some(field))
                .map(|(index, _)| index)
                .collect();
            if positions.is_empty() {
                continue;
            }
            for &at in &positions {
                self.marks[at] = Mark::Missed;
            }

            let sent: Vec<char> = positions.iter().map(|&at| heard[at].0).collect();
            let ends: Vec<f32> = positions.iter().map(|&at| heard[at].1).collect();
            let typed: Vec<char> = groups
                .get(field)
                .map(|g| g.iter().map(|&i| self.typed[i].ch).collect())
                .unwrap_or_default();
            let times: Vec<f32> = groups
                .get(field)
                .map(|g| g.iter().map(|&i| self.typed[i].at).collect())
                .unwrap_or_default();

            let base = positions[0];
            let outcome = self.apply_at(&sent, &ends, &typed, &times, &positions, progress);
            let _ = base;

            self.correct += outcome.correct;
            self.wrong += outcome.wrong;
            if settings.practice.strict {
                self.inserted += outcome.inserted;
            }

            let right = outcome.wrong == 0 && outcome.inserted == 0 && outcome.correct > 0;
            if let Some(mark) = self.fields.get_mut(field) {
                mark.right = right;
            }
            self.scored_fields += 1;
            if right {
                self.fields_right += 1;
                self.scored_fields_right += 1;
            }
        }

        // Anything typed past the last field is an answer to a question nobody
        // asked, which in strict mode is a mistake and otherwise is not.
        if settings.practice.strict && groups.len() > self.field_text.len() {
            let extra: usize = groups[self.field_text.len()..].iter().map(|g| g.len()).sum();
            self.inserted += extra as u32;
        }
    }

    /// Aligns one answer and folds it into the marks and the history.
    fn apply(
        &mut self,
        sent: &[char],
        ends: &[f32],
        typed: &[char],
        times: &[f32],
        offset: usize,
        progress: &mut Progress,
    ) -> Outcome {
        let positions: Vec<usize> = (offset..offset + sent.len()).collect();
        self.apply_at(sent, ends, typed, times, &positions, progress)
    }

    /// The same, where the marks are not contiguous.
    fn apply_at(
        &mut self,
        sent: &[char],
        ends: &[f32],
        typed: &[char],
        times: &[f32],
        positions: &[usize],
        progress: &mut Progress,
    ) -> Outcome {
        let outcome = score::align(sent, typed);
        // An interval measured across several hearings is not a reaction time.
        let timed = self.copies <= 1;

        for op in &outcome.ops {
            match *op {
                Op::Match { sent: i, typed: j } => {
                    if let Some(&at) = positions.get(i) {
                        if let Some(slot) = self.marks.get_mut(at) {
                            *slot = Mark::Right;
                        }
                    }
                    // Only a confirmed match is timed. The interval before a
                    // wrong answer measures how long the student thought rather
                    // than how long they took to recognize.
                    let reaction = if timed {
                        let raw = ((times[j] - ends[i]) * 1000.0).max(0.0);
                        if raw <= REACTION_LIMIT_MS {
                            Some(raw)
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some(ms) = reaction {
                        self.reaction_sum += ms;
                        self.reaction_n += 1;
                    }
                    progress.record(sent[i], Some(typed[j]), reaction);
                }
                Op::Substitute { sent: i, typed: j } => {
                    if let Some(&at) = positions.get(i) {
                        if let Some(slot) = self.marks.get_mut(at) {
                            *slot = Mark::Wrong;
                        }
                    }
                    progress.record(sent[i], Some(typed[j]), None);
                }
                Op::Omit { sent: i } => {
                    if let Some(&at) = positions.get(i) {
                        if let Some(slot) = self.marks.get_mut(at) {
                            *slot = Mark::Missed;
                        }
                    }
                    progress.record(sent[i], None, None);
                }
                // An insertion has no sent character to blame, see the note in
                // the scoring module.
                Op::Insert { .. } => {}
            }
        }
        outcome
    }

    /// Clears the answer and returns to sending.
    fn next_group(&mut self) {
        self.clear_answer();
        self.phase = Phase::Sending;
    }

    fn clear_answer(&mut self) {
        self.heard.clear();
        self.typed.clear();
        self.marks.clear();
        self.group.clear();
        self.group_fields.clear();
        self.fields.clear();
        self.field_text.clear();
        self.gloss = None;
        self.queued.clear();
        self.field_of.clear();
        self.expecting = 0;
        self.reached = 0;
        self.last_end = 0.0;
        self.repeats_left = 0;
        self.copies = 1;
        self.fields_right = 0;
        self.is_exchange = false;
        self.scored = false;
        self.epoch = self.epoch.wrapping_add(1);
    }

    /// Answers begun since the session started.
    pub fn answer_epoch(&self) -> u32 {
        self.epoch
    }

    fn accuracy(&self) -> f32 {
        let total = self.correct + self.wrong;
        if total == 0 {
            0.0
        } else {
            self.correct as f32 / total as f32
        }
    }

    fn mean_reaction(&self) -> f32 {
        if self.reaction_n == 0 {
            0.0
        } else {
            self.reaction_sum / self.reaction_n as f32
        }
    }

    /// What the interface draws.
    pub fn view(&self, settings: &Settings) -> SessionView {
        let listening = settings.practice.listening();
        // Head copy withholds the text until the group ends, which a drill of
        // one unit cannot mean: the unit is the group.
        let head = settings.practice.mode == PracticeMode::HeadCopy
            && settings.practice.drill == DrillMode::Off;
        // A keyed answer needs its prompt from the first frame, because the
        // prompt is the thing being keyed. The blind drill is the exception, and
        // withholding the character is the whole of that exercise.
        let sending = settings.practice.keying();
        // The blind exercise withholds the character and the recall exercise
        // withholds its pattern, which are the two halves of one statement read
        // in opposite directions.
        let hidden = settings.practice.drill == DrillMode::Blind && !self.scored;
        let pattern_hidden = settings.practice.drill == DrillMode::Recall && !self.scored;
        // The spaces are kept. For a group there are none, and for an exchange
        // they are the field boundaries: dropping them turns a readable line into
        // one run of letters, which was the state this fixed.
        //
        // The marks are indexed by sounding character rather than by position in
        // this string, so the drawing walks them separately. That is why the two
        // are not one list: a space has no verdict and would need a fourth mark
        // meaning nothing.
        let sent = if listening {
            self.transcript.clone()
        } else if !hidden && (sending || self.scored || (!head && self.phase == Phase::Answering)) {
            self.heard.iter().map(|h| h.ch).collect()
        } else {
            String::new()
        };

        // Which copy is playing, counted from the reached characters rather than
        // held: a count kept beside them would be a second thing to keep correct.
        let copy = if self.expecting == 0 {
            0
        } else {
            ((self.reached / self.expecting) as u32 + 1).min(self.copies)
        };

        SessionView {
            running: self.phase != Phase::Idle,
            phase: self.phase.key(),
            elapsed: self.clock,
            remaining: (settings.lesson.session_seconds as f32 - self.clock).max(0.0),
            groups: self.groups,
            characters: self.characters,
            accuracy: self.accuracy(),
            reaction_ms: self.mean_reaction(),
            inserted: self.inserted,
            sent,
            typed: self.typed.iter().map(|t| t.ch).collect(),
            marks: self.marks.clone(),
            scored: self.scored,
            exchange: self.is_exchange,
            gloss: self.gloss,
            fallback: self.material.fallback().key(),
            next_char: self.next_expected(),
            fields: self.fields.clone(),
            fields_right: self.scored_fields_right,
            fields_total: self.scored_fields,
            copy,
            copies: self.copies,
            repeatable: settings.practice.sounded() && !listening,
            drill: settings.practice.drill,
            target: if hidden { String::new() } else { self.group.clone() },
            target_hidden: hidden,
            pattern_hidden,
            log: self.log.iter().cloned().collect(),
        }
    }
}

impl Default for Session {
    fn default() -> Session {
        Session::new()
    }
}

/// Material section a drill draws through, nothing when none is in force.
///
/// The stated section describes a session of groups: a length, a repetition, a
/// source. A drill contradicts all three, so it is overridden for the draw
/// rather than the operator being asked to set the two consistently.
fn drill_material(settings: &Settings) -> Option<MaterialSettings> {
    if settings.practice.drill == DrillMode::Off {
        return None;
    }
    let mut out = settings.material.clone();
    // A repetition is reachable from the transport, so the setting would only
    // be a second way to ask for the same thing.
    out.repeat = 1;
    match settings.practice.drill_unit {
        DrillUnit::Character => {
            // One character of the lesson set, and nothing the order has not
            // reached. A digit appended to the pool because a switch elsewhere
            // is on has nothing to do with the character being learned, and it
            // arrives as often as the character does.
            out.source = MaterialSource::Groups;
            out.min_group = 1;
            out.max_group = 1;
            out.include_numbers = false;
            out.include_punctuation = false;
            out.include_prosigns = false;
        }
        DrillUnit::Word => {
            // The token sources are left alone: a Q code drilled one at a time
            // is a Q code, and overriding it was why the setting appeared to do
            // nothing. Only the sources that assemble something out of the pool
            // are replaced, because a group of five is not a word and neither is
            // an exchange.
            if matches!(
                out.source,
                MaterialSource::Groups | MaterialSource::Numbers | MaterialSource::Qso
            ) {
                out.source = MaterialSource::Words;
            }
        }
    }
    Some(out)
}

/// Seconds one character occupies, gaps inside it included.
///
/// From the pattern rather than from what was keyed, because the difference is
/// the jitter: a few milliseconds against a reaction time of hundreds. Measuring
/// it would mean carrying a duration across the queue for a precision nothing
/// reads.
fn character_seconds(pattern: &str, settings: &Settings) -> f32 {
    let timing = &settings.timing;
    let dot = timing.dot_seconds();
    let (element_gap, _, _) = timing.gaps();
    let mut total = 0.0f32;
    for (index, symbol) in pattern.chars().enumerate() {
        if index > 0 {
            total += element_gap;
        }
        total += if symbol == '-' { dot * timing.weight } else { dot };
    }
    total
}