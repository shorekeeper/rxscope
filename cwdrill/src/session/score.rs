//! Answer comparison.
//!
//! ## Why alignment rather than position
//!
//! A student who missed one character has everything after it shifted by one.
//! Compared position by position, that reads as a group in which nothing was
//! right, and the statistics then blame every character in it for a failure that
//! belongs to one. It is the classic mistake of a trainer and it is the reason
//! the per character history is worth nothing in the trainers that make it.
//!
//! So the two strings are aligned first. Every operation costs one except a
//! match, which is free, and the cheapest path through the table is the reading
//! that assumes the fewest mistakes. A group of five characters is a table of
//! thirty six cells, which is nothing at the rate a group arrives.
//!
//! ## What each operation means for the statistics
//!
//! A substitution and an omission both count against the character that was
//! sent, because that is the character the ear failed on; which of the two it
//! was goes into the confusion matrix, where the pair says something the count
//! alone does not.
//!
//! An insertion counts against nothing. There is no sent character to blame, and
//! attributing it to a neighbour would put a mark against a character that was
//! copied correctly. It is tallied separately, because a student who inserts is
//! doing something specific: hearing an element as a character.

/// One step of the alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    /// The character was copied.
    Match { sent: usize, typed: usize },
    /// Another character was written in its place.
    Substitute { sent: usize, typed: usize },
    /// Nothing was written for it.
    Omit { sent: usize },
    /// A character was written that was never sent.
    Insert { typed: usize },
}

/// What one group produced.
pub struct Outcome {
    pub ops: Vec<Op>,
    pub correct: u32,
    /// Substitutions and omissions together, which is what the accuracy of a
    /// character is measured against.
    pub wrong: u32,
    pub inserted: u32,
}

impl Outcome {
    /// Share of sent characters that were copied.
    ///
    /// Insertions are absent from both sides of the ratio: they are not a sent
    /// character, so they cannot be a fraction of one.
    pub fn accuracy(&self) -> f32 {
        let total = self.correct + self.wrong;
        if total == 0 {
            return 0.0;
        }
        self.correct as f32 / total as f32
    }
}

/// Cheapest reading of one answer.
///
/// The traceback prefers a match, then a substitution, then an omission, then an
/// insertion. The order matters only for a tie and exists so the same pair of
/// strings always produces the same reading: a confusion matrix built from a
/// nondeterministic alignment would record a different pair on every run.
pub fn align(sent: &[char], typed: &[char]) -> Outcome {
    let n = sent.len();
    let m = typed.len();

    // One row longer in each direction, for the empty prefix.
    let stride = m + 1;
    let mut cost = vec![0u32; (n + 1) * stride];
    for i in 0..=n {
        cost[i * stride] = i as u32;
    }
    for j in 0..=m {
        cost[j] = j as u32;
    }
    for i in 1..=n {
        for j in 1..=m {
            let diagonal = cost[(i - 1) * stride + j - 1]
                + u32::from(sent[i - 1] != typed[j - 1]);
            let omit = cost[(i - 1) * stride + j] + 1;
            let insert = cost[i * stride + j - 1] + 1;
            cost[i * stride + j] = diagonal.min(omit).min(insert);
        }
    }

    let mut ops = Vec::with_capacity(n.max(m));
    let mut correct = 0u32;
    let mut wrong = 0u32;
    let mut inserted = 0u32;

    let mut i = n;
    let mut j = m;
    while i > 0 || j > 0 {
        let here = cost[i * stride + j];

        if i > 0 && j > 0 {
            let diagonal = cost[(i - 1) * stride + j - 1];
            if sent[i - 1] == typed[j - 1] && here == diagonal {
                ops.push(Op::Match { sent: i - 1, typed: j - 1 });
                correct += 1;
                i -= 1;
                j -= 1;
                continue;
            }
            if here == diagonal + 1 {
                ops.push(Op::Substitute { sent: i - 1, typed: j - 1 });
                wrong += 1;
                i -= 1;
                j -= 1;
                continue;
            }
        }
        if i > 0 && here == cost[(i - 1) * stride + j] + 1 {
            ops.push(Op::Omit { sent: i - 1 });
            wrong += 1;
            i -= 1;
            continue;
        }
        if j > 0 {
            ops.push(Op::Insert { typed: j - 1 });
            inserted += 1;
            j -= 1;
            continue;
        }
        // Unreachable while the table is consistent, and a break rather than a
        // panic because a wrong reading of one group is a worse thing to crash
        // over than to report.
        break;
    }

    ops.reverse();
    Outcome { ops, correct, wrong, inserted }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    #[test]
    fn an_exact_answer_is_all_matches() {
        let out = align(&chars("KMUR"), &chars("KMUR"));
        assert_eq!(out.correct, 4);
        assert_eq!(out.wrong, 0);
        assert_eq!(out.inserted, 0);
        assert!(out.ops.iter().all(|op| matches!(op, Op::Match { .. })));
    }

    #[test]
    fn one_substitution_blames_one_character() {
        let out = align(&chars("KMUR"), &chars("KRUR"));
        assert_eq!(out.correct, 3);
        assert_eq!(out.wrong, 1);
        assert!(out
            .ops
            .iter()
            .any(|op| matches!(op, Op::Substitute { sent: 1, typed: 1 })));
    }

    #[test]
    fn one_omission_does_not_cascade() {
        // The whole reason the alignment exists. Compared by position this reads
        // as three wrong out of four, and the three characters after the gap are
        // blamed for a mistake they had nothing to do with.
        let out = align(&chars("KMUR"), &chars("KUR"));
        assert_eq!(out.correct, 3);
        assert_eq!(out.wrong, 1);
        assert!(out.ops.iter().any(|op| matches!(op, Op::Omit { sent: 1 })));
    }

    #[test]
    fn an_insertion_counts_against_nothing() {
        let out = align(&chars("KMU"), &chars("KMEU"));
        assert_eq!(out.correct, 3);
        assert_eq!(out.wrong, 0);
        assert_eq!(out.inserted, 1);
        // Accuracy is a fraction of what was sent, so an insertion cannot move
        // it: there is no sent character it could be a fraction of.
        assert!((out.accuracy() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn an_empty_answer_omits_everything() {
        let out = align(&chars("KMUR"), &[]);
        assert_eq!(out.correct, 0);
        assert_eq!(out.wrong, 4);
        assert_eq!(out.accuracy(), 0.0);
    }

    #[test]
    fn the_reading_is_the_same_every_time() {
        // A confusion matrix built from a nondeterministic alignment would
        // record a different pair on every run, which is worse than recording
        // none: the numbers would look like data.
        let first = align(&chars("SHRD"), &chars("SRHD"));
        for _ in 0..8 {
            let again = align(&chars("SHRD"), &chars("SRHD"));
            assert_eq!(first.ops, again.ops);
        }
    }
}