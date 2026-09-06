//! The user-answer grammar.
//!
//! This is the only place where a user message becomes a decision, so the strictness here is a
//! safety property and not a UX preference. "ok", "sounds good", "yes do it", or a bare
//! `CONFIRM PLAN` with no challenge must all be rejected: accepting any of them would let Hwahap
//! record a confirmation the user never made, and the challenge exists precisely so that
//! confirming cannot be done by reflex.
//!
//! The parser therefore recognises one closed grammar, one directive per line, and reports every
//! line it did not understand instead of guessing at intent. A line that is nearly right — wrong
//! case, a leading zero, a bullet in front of it, a word after it — is not an answer.
//!
//! Invisible characters get the same treatment. Once a line has been trimmed, a tab, a carriage
//! return, an escape or any other control character left inside it rejects the whole line: what the
//! user sees is not what a parser that quietly absorbed those characters would act on, and a
//! grammar that guesses which of the two was meant is deciding on the user's behalf.
//!
//! Nothing here consults a plan. A challenge is carried verbatim and matched against the current
//! plan digest by the caller, so that "was this the right challenge" stays a question about state
//! rather than a question about text.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::canonical::CHALLENGE_LEN;
use crate::plan::{Selection, Surface};

/// The `CONFIRM PLAN` keyword, which is also its conflict label.
const CONFIRM_PLAN: &str = "CONFIRM PLAN";

/// The `SHIP` keyword, which is also its conflict label.
const SHIP: &str = "SHIP";

/// One recognized instruction from the user's message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Directive {
    /// `C<n>=…`: an answer to one decision.
    Decision {
        /// The `C<n>` the answer belongs to.
        id: String,
        selection: Selection,
    },
    /// `S<n>=NA`: the user, and only the user, closing a surface.
    Surface {
        /// The `S<n>` being closed.
        id: String,
    },
    /// `CONFIRM PLAN <challenge>`: the freeze.
    ConfirmPlan {
        /// The challenge as typed. Whether it is the *current* plan's challenge is the caller's
        /// question, not this module's.
        challenge: String,
    },
    /// `SHIP <challenge>`: the ship.
    Ship {
        /// The challenge as typed, on the same terms as [`Directive::ConfirmPlan`].
        challenge: String,
    },
}

impl Directive {
    /// What this directive answers, used to detect two answers to the same thing.
    ///
    /// `CONFIRM PLAN` and `SHIP` are single targets rather than per-challenge ones: two confirmations
    /// in one message are a contradiction even when the challenges agree, because only one of them
    /// can have been meant.
    fn target(&self) -> String {
        match self {
            Directive::Decision { id, .. } | Directive::Surface { id } => id.clone(),
            Directive::ConfirmPlan { .. } => CONFIRM_PLAN.to_string(),
            Directive::Ship { .. } => SHIP.to_string(),
        }
    }
}

/// The outcome of reading one user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedMessage {
    /// The directives, in the order they appeared.
    pub directives: Vec<Directive>,
    /// Lines that carry content but match no rule, verbatim and in order.
    pub unrecognized: Vec<String>,
    /// Targets answered more than once in the same message, sorted.
    pub conflicts: Vec<String>,
}

impl ParsedMessage {
    /// True when the message contributed at least one directive and nothing was ambiguous.
    pub fn is_clean(&self) -> bool {
        !self.directives.is_empty() && self.unrecognized.is_empty() && self.conflicts.is_empty()
    }

    /// A message for the user naming every line that was not understood.
    ///
    /// `None` means there is nothing to complain about; a message that simply said nothing is not an
    /// error, because the user is allowed to talk to Hwahap without answering anything.
    pub fn rejection_message(&self) -> Option<String> {
        if self.unrecognized.is_empty() && self.conflicts.is_empty() {
            return None;
        }
        let mut out = String::from("Hwahap could not use every line of that message.\n");
        if !self.unrecognized.is_empty() {
            out.push_str("\nNot an answer:\n");
            for line in &self.unrecognized {
                // Debug quoting, so a control character in the user's line cannot escape into the
                // rendered message and change what the rest of it appears to say. It also shows the
                // user the invisible character that cost them the line.
                let _ = writeln!(out, "  {line:?}");
            }
        }
        if !self.conflicts.is_empty() {
            out.push_str("\nAnswered more than once, so left unanswered:\n");
            for target in &self.conflicts {
                let _ = writeln!(out, "  {target}");
            }
        }
        out.push('\n');
        out.push_str(&accepted_forms());
        Some(out)
    }
}

/// Reads one user message.
///
/// Never fails: every possible input maps to some [`ParsedMessage`], and it is the caller that
/// decides what an empty or rejected message means for the current state.
pub fn parse_message(text: &str) -> ParsedMessage {
    let mut candidates: Vec<(String, Directive)> = Vec::new();
    let mut unrecognized: Vec<String> = Vec::new();
    // Sorted by construction, which is what makes `conflicts` deterministic.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();

    for raw in text.split('\n') {
        // Trimming ASCII whitespace also strips the `\r` of a CRLF line ending.
        let line = raw.trim_ascii();
        if line.is_empty() {
            continue;
        }
        match parse_line(line) {
            Some(directive) => {
                let target = directive.target();
                *seen.entry(target.clone()).or_insert(0) += 1;
                candidates.push((target, directive));
            }
            None => unrecognized.push(line.to_string()),
        }
    }

    let conflicts: Vec<String> = seen
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(target, _)| target.clone())
        .collect();
    let directives = candidates
        .into_iter()
        // A contested target contributes nothing: Hwahap cannot know which of the two lines the user
        // meant, and picking either one would be inventing an answer.
        .filter(|(target, _)| seen.get(target) == Some(&1))
        .map(|(_, directive)| directive)
        .collect();

    ParsedMessage {
        directives,
        unrecognized,
        conflicts,
    }
}

/// Parses one already-trimmed, non-empty line.
fn parse_line(line: &str) -> Option<Directive> {
    // Checked before any keyword is: a control character surviving the trim is invisible to the user
    // but not to the parser, so a grammar that absorbed it would judge a different line than the one
    // that was typed. It is also what makes "only a space may surround the `=`" true of every other
    // ASCII whitespace character, all of which are control characters.
    if line.chars().any(char::is_control) {
        return None;
    }

    // Keyword plus exactly one space: `CONFIRM  PLAN …` and `CONFIRM PLAN\t…` are not the phrase.
    if let Some(rest) = line
        .strip_prefix(CONFIRM_PLAN)
        .and_then(|r| r.strip_prefix(' '))
    {
        return challenge_of(rest).map(|challenge| Directive::ConfirmPlan { challenge });
    }
    if let Some(rest) = line.strip_prefix(SHIP).and_then(|r| r.strip_prefix(' ')) {
        return challenge_of(rest).map(|challenge| Directive::Ship { challenge });
    }

    // Splitting on the first `=` is what lets an OTHER value contain one.
    let (target, value) = line.split_once('=')?;
    // Only the space is loosened around the `=`, exactly as the grammar says.
    let (target, value) = (target.trim_matches(' '), value.trim_matches(' '));

    if let Some(surface) = Surface::parse(target) {
        // `Surface::parse` is the single source of truth for the 1..=12 range.
        return (value == "NA").then(|| Directive::Surface {
            id: surface.id().to_string(),
        });
    }
    match target.strip_prefix('C') {
        Some(index) if is_index(index) => {
            parse_selection(value).map(|selection| Directive::Decision {
                id: target.to_string(),
                selection,
            })
        }
        _ => None,
    }
}

/// Parses the right-hand side of a `C<n>=` line.
fn parse_selection(value: &str) -> Option<Selection> {
    if value == "REC" {
        return Some(Selection::Recommendation);
    }
    if value == "UNKNOWN" {
        return Some(Selection::Unknown);
    }
    if let Some(other) = value.strip_prefix("OTHER:") {
        // The value is free text rather than grammar, so every kind of whitespace is trimmed off its
        // ends rather than only the ASCII kind: padding is not part of an answer, and an answer made
        // of nothing but padding — a lone U+00A0, say — is not an answer at all.
        let other = other.trim();
        return (!other.is_empty()).then(|| Selection::Other {
            value: other.to_string(),
        });
    }
    if let Some(index) = value.strip_prefix("ALT") {
        return is_index(index).then(|| Selection::Alternative {
            id: value.to_string(),
        });
    }
    None
}

/// The challenge token, if it is exactly what `Digest::challenge` emits.
fn challenge_of(text: &str) -> Option<String> {
    // Byte length is safe here only because the check beside it rejects every non-ASCII byte.
    let exact = text.len() == CHALLENGE_LEN
        && text
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase());
    exact.then(|| text.to_string())
}

/// True when `text` is an index: decimal, at least 1, no leading zero, and inside `u32`.
///
/// The `u32` bound is what stops `C4294967296=REC` from wrapping into some other decision; an index
/// Hwahap could not have emitted is not an answer to anything. `parse` also rejects the empty index
/// of `C=REC`, and the digit test above it rejects what `parse` would otherwise take: `+1`.
fn is_index(text: &str) -> bool {
    !text.starts_with('0')
        && text.bytes().all(|b| b.is_ascii_digit())
        && text.parse::<u32>().is_ok()
}

/// The whole grammar, repeated back whenever a line was rejected.
///
/// Shown in full every time: a user who typed something close to right needs the exact forms, not a
/// complaint about the one line that failed.
fn accepted_forms() -> String {
    format!(
        "Answer with one directive per line, exactly: C<n>=REC, C<n>=ALT<m>, C<n>=OTHER: <value>, \
         C<n>=UNKNOWN, S<n>=NA (n is 1-12), CONFIRM PLAN <{CHALLENGE_LEN} uppercase hex characters>, \
         SHIP <{CHALLENGE_LEN} uppercase hex characters>. Keywords are uppercase, indices have no \
         leading zero, only a space may surround the =, nothing else may appear on the line, and a \
         tab or any other invisible control character makes the line unusable."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canonical::Digest;
    use crate::plan::Plan;

    /// The one directive a message must have produced, cleanly.
    fn only(text: &str) -> Directive {
        let parsed = parse_message(text);
        assert!(parsed.is_clean(), "{text:?} was not clean: {parsed:?}");
        assert_eq!(parsed.directives.len(), 1, "{text:?} -> {parsed:?}");
        parsed.directives[0].clone()
    }

    /// Asserts that a one-line message contributed nothing and named that line back.
    fn rejects(line: &str) {
        let parsed = parse_message(line);
        assert!(
            parsed.directives.is_empty() && parsed.conflicts.is_empty(),
            "{line:?} should have been rejected outright: {parsed:?}"
        );
        assert_eq!(
            parsed.unrecognized,
            vec![line.trim_ascii().to_string()],
            "{line:?} should have been reported verbatim"
        );
        assert!(!parsed.is_clean(), "{line:?}");
        assert!(parsed.rejection_message().is_some(), "{line:?}");
    }

    fn decision(id: &str, selection: Selection) -> Directive {
        Directive::Decision {
            id: id.to_string(),
            selection,
        }
    }

    fn alternative(id: &str) -> Selection {
        Selection::Alternative { id: id.to_string() }
    }

    fn other(value: &str) -> Selection {
        Selection::Other {
            value: value.to_string(),
        }
    }

    fn surface(id: &str) -> Directive {
        Directive::Surface { id: id.to_string() }
    }

    /// The canonical line for a directive, used for round-trip properties.
    fn render(directive: &Directive) -> String {
        match directive {
            Directive::Decision { id, selection } => match selection {
                Selection::Recommendation => format!("{id}=REC"),
                Selection::Alternative { id: alt } => format!("{id}={alt}"),
                Selection::Other { value } => format!("{id}=OTHER: {value}"),
                Selection::Unknown => format!("{id}=UNKNOWN"),
                Selection::NotApplicable => unreachable!("C<n> never yields NotApplicable"),
            },
            Directive::Surface { id } => format!("{id}=NA"),
            Directive::ConfirmPlan { challenge } => format!("{CONFIRM_PLAN} {challenge}"),
            Directive::Ship { challenge } => format!("{SHIP} {challenge}"),
        }
    }

    #[test]
    fn the_four_decision_forms_map_to_their_selections() {
        assert_eq!(only("C1=REC"), decision("C1", Selection::Recommendation));
        assert_eq!(only("C2=ALT3"), decision("C2", alternative("ALT3")));
        assert_eq!(
            only("C3=OTHER: fail after 10s"),
            decision("C3", other("fail after 10s"))
        );
        assert_eq!(only("C4=UNKNOWN"), decision("C4", Selection::Unknown));
    }

    #[test]
    fn every_surface_from_one_to_twelve_can_be_closed() {
        for n in 1..=12 {
            assert_eq!(only(&format!("S{n}=NA")), surface(&format!("S{n}")));
        }
    }

    #[test]
    fn an_index_may_be_any_number_that_fits_a_u32() {
        for n in ["1", "2", "9", "10", "12", "13", "100", "4294967295"] {
            assert_eq!(
                only(&format!("C{n}=REC")),
                decision(&format!("C{n}"), Selection::Recommendation)
            );
        }
    }

    #[test]
    fn a_confirmation_and_a_ship_carry_their_challenge_verbatim() {
        assert_eq!(
            only("CONFIRM PLAN 7F3A91C2"),
            Directive::ConfirmPlan {
                challenge: "7F3A91C2".into()
            }
        );
        assert_eq!(
            only("SHIP 00000000"),
            Directive::Ship {
                challenge: "00000000".into()
            }
        );
    }

    #[test]
    fn every_challenge_a_digest_can_produce_is_accepted() {
        for seed in 0u8..64 {
            let challenge = Digest::of_bytes(&[seed]).challenge();
            assert_eq!(
                only(&format!("CONFIRM PLAN {challenge}")),
                Directive::ConfirmPlan {
                    challenge: challenge.clone()
                },
                "rejected the challenge {challenge} of seed {seed}"
            );
            assert_eq!(
                only(&format!("SHIP {challenge}")),
                Directive::Ship {
                    challenge: challenge.clone()
                }
            );
        }
    }

    #[test]
    fn the_challenge_of_a_real_plan_passes_through_the_grammar_unchanged() {
        let challenge = Plan::new("2026-09-04-dry-run", "main", "add dry-run")
            .challenge()
            .expect("a new plan has a challenge");
        assert_eq!(
            only(&format!("CONFIRM PLAN {challenge}")),
            Directive::ConfirmPlan {
                challenge: challenge.clone()
            }
        );
        assert_eq!(
            only(&format!("SHIP {challenge}")),
            Directive::Ship { challenge }
        );
    }

    #[test]
    fn only_a_space_may_surround_the_equals_sign() {
        for text in [
            "C1 = ALT1",
            "C1= ALT1",
            "C1 =ALT1",
            "C1   =   ALT1",
            "  C1 = ALT1  ",
        ] {
            assert_eq!(only(text), decision("C1", alternative("ALT1")), "{text:?}");
        }
        assert_eq!(only("  S3 = NA  "), surface("S3"));
    }

    #[test]
    fn ascii_whitespace_other_than_a_space_around_the_equals_sign_is_not_absorbed() {
        for line in [
            "C1\t=REC",
            "C1=\tREC",
            "C1\t=\tREC",
            "C1\u{c}=REC",
            "C1=\u{c}REC",
            "C1 =\u{c} REC",
            "S1\t=\tNA",
            "C1=\tOTHER: x",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn a_carriage_return_inside_a_line_is_not_absorbed() {
        for line in [
            "C1=\rREC",
            "C1\r=REC",
            "C1=RE\rC",
            "C1=REC\rC2=REC",
            "SHIP\r1A2B3C4D",
            "CONFIRM PLAN\r1A2B3C4D",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn ascii_whitespace_at_the_edges_of_a_line_is_trimmed_rather_than_rejected() {
        for text in [
            "\tC1=REC",
            "C1=REC\t",
            " \t\u{c}C1=REC\u{c}\t ",
            "\rC1=REC\r",
        ] {
            assert_eq!(
                only(text),
                decision("C1", Selection::Recommendation),
                "{text:?}"
            );
        }
    }

    #[test]
    fn plain_language_confirmation_is_never_accepted() {
        for line in [
            "ok",
            "yes",
            "confirm",
            "CONFIRM",
            "CONFIRM PLAN",
            "CONFIRM PLAN ",
            "CONFIRM THE PLAN",
            "ship it",
            "SHIP IT",
            "SHIP",
            "sounds good",
            "yes do it",
            "LGTM",
            "추천대로 해줘",
            "그대로 진행해 주세요",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn keywords_are_uppercase_only() {
        for line in [
            "c1=REC",
            "C1=rec",
            "C1=Rec",
            "C1=alt1",
            "C1=Alt1",
            "C1=ALt1",
            "C1=unknown",
            "C1=Unknown",
            "C1=other: x",
            "C1=Other: x",
            "s1=NA",
            "S1=na",
            "S1=Na",
            "confirm plan 7F3A91C2",
            "Confirm Plan 7F3A91C2",
            "ship 7F3A91C2",
            "Ship 7F3A91C2",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn a_lowercase_challenge_is_not_the_challenge() {
        rejects("CONFIRM PLAN 7f3a91c2");
        rejects("SHIP 7f3a91C2");
        assert_eq!(
            only("SHIP 7F3A91C2"),
            Directive::Ship {
                challenge: "7F3A91C2".into()
            }
        );
    }

    #[test]
    fn a_challenge_must_be_exactly_the_documented_length() {
        let short: String = "A".repeat(CHALLENGE_LEN - 1);
        let long: String = "A".repeat(CHALLENGE_LEN + 1);
        rejects(&format!("SHIP {short}"));
        rejects(&format!("SHIP {long}"));
        rejects(&format!("CONFIRM PLAN {short}"));
        rejects(&format!("CONFIRM PLAN {long}"));
        assert_eq!(
            only(&format!("SHIP {}", "A".repeat(CHALLENGE_LEN))),
            Directive::Ship {
                challenge: "A".repeat(CHALLENGE_LEN)
            }
        );
    }

    #[test]
    fn a_challenge_is_counted_in_characters_rather_than_bytes() {
        // Eight bytes, four characters: a byte-length check alone would let this through.
        rejects("SHIP \u{c1}\u{c2}\u{c3}\u{c4}");
        rejects("CONFIRM PLAN \u{c1}\u{c2}\u{c3}\u{c4}");
        // Eight bytes, seven characters, six of them legal hex.
        rejects("SHIP ABCDEF\u{e9}");
    }

    #[test]
    fn a_challenge_holds_nothing_but_uppercase_hex() {
        for line in [
            "SHIP 1A2B3C4G",
            "SHIP 1A2B-3C4",
            "SHIP 1A2B 3C4D",
            "SHIP 1A2B\t3C4D",
            "SHIP 1A2B=3C4D",
            "SHIP 1A2B3C4D!",
            "SHIP ＡＢＣＤＥＦ12",
            "CONFIRM PLAN 1A2B3C4G",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn the_keyword_needs_exactly_one_space_before_the_challenge() {
        for line in [
            "CONFIRM  PLAN 7F3A91C2",
            "CONFIRMPLAN 7F3A91C2",
            "CONFIRM PLAN  7F3A91C2",
            "CONFIRM PLAN\t7F3A91C2",
            "SHIP  7F3A91C2",
            "SHIP\t7F3A91C2",
            "SHIPPING 7F3A91C2",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn a_non_ascii_space_after_the_keyword_is_not_the_separator() {
        rejects("CONFIRM PLAN\u{a0}7F3A91C2");
        rejects("SHIP\u{a0}7F3A91C2");
        rejects("SHIP\u{3000}7F3A91C2");
    }

    #[test]
    fn a_line_that_opens_with_a_keyword_never_falls_through_to_the_equals_rule() {
        // Otherwise `SHIP C1=REC` would answer C1, and a malformed ship would become a decision.
        for line in [
            "SHIP C1=REC",
            "CONFIRM PLAN C1=REC",
            "SHIP 1A2B=3C4D",
            "CONFIRM PLAN 1A2B=3C4D",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn trailing_content_after_a_complete_directive_is_rejected() {
        for line in [
            "C1=REC please",
            "C1=REC.",
            "C1=ALT1 or ALT2",
            "C1=UNKNOWN?",
            "S1=NA now",
            "SHIP 1A2B3C4D now",
            "CONFIRM PLAN 1A2B3C4D !",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn indices_reject_zero_and_leading_zeros() {
        for line in [
            "C0=REC", "C01=REC", "C007=REC", "S0=NA", "S01=NA", "C1=ALT0", "C1=ALT01",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn an_index_that_would_overflow_a_u32_is_rejected_rather_than_wrapped() {
        rejects("C4294967296=REC");
        rejects("C1=ALT4294967296");
        rejects(&format!("C{}=REC", "9".repeat(100)));
        // The largest index that cannot wrap is still an ordinary answer.
        assert_eq!(
            only("C4294967295=REC"),
            decision("C4294967295", Selection::Recommendation)
        );
        assert_eq!(
            only("C1=ALT4294967295"),
            decision("C1", alternative("ALT4294967295"))
        );
    }

    #[test]
    fn an_index_must_be_ascii_decimal_digits_only() {
        for line in [
            "C=REC",
            "C+1=REC",
            "C-1=REC",
            "C1.0=REC",
            "C1_0=REC",
            "C 1=REC",
            "C1a=REC",
            "C\u{0661}=REC",
            "C\u{ff12}=REC",
            "C1=ALT",
            "C1=ALT 1",
            "C1=ALT1x",
            "C1=ALTONE",
            "C1=ALT+1",
            "C1=ALT\u{0661}",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn surface_indices_stop_at_twelve() {
        rejects("S13=NA");
        rejects("S99=NA");
        rejects("S120=NA");
        rejects("S4294967296=NA");
        assert_eq!(only("S12=NA"), surface("S12"));
    }

    #[test]
    fn only_na_closes_a_surface_and_na_answers_nothing_else() {
        for line in [
            "S1=REC",
            "S1=ALT1",
            "S1=OTHER: x",
            "S1=UNKNOWN",
            "S1=",
            "S 1=NA",
            "C1=NA",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn an_other_value_is_the_rest_of_the_line_including_delimiters() {
        assert_eq!(
            only("C1=OTHER: use a=b: c"),
            decision("C1", other("use a=b: c"))
        );
        assert_eq!(
            only("C1=OTHER: C2=REC"),
            decision("C1", other("C2=REC")),
            "a directive inside a value is data, not a second directive"
        );
        assert_eq!(
            only("C1=OTHER: OTHER: x"),
            decision("C1", other("OTHER: x"))
        );
    }

    #[test]
    fn a_keyword_inside_an_other_value_is_data_rather_than_a_second_directive() {
        let parsed = parse_message("C1=OTHER: CONFIRM PLAN 1A2B3C4D");
        assert_eq!(
            parsed.directives,
            vec![decision("C1", other("CONFIRM PLAN 1A2B3C4D"))]
        );
        assert!(parsed.is_clean());
    }

    #[test]
    fn an_other_value_may_be_glued_to_the_colon() {
        assert_eq!(only("C1=OTHER:x"), decision("C1", other("x")));
        assert_eq!(
            only("C1=OTHER:  spaced  out  "),
            decision("C1", other("spaced  out"))
        );
    }

    #[test]
    fn an_empty_other_value_is_not_an_answer() {
        for line in [
            "C1=OTHER:",
            "C1=OTHER:   ",
            "C1=OTHER:\t",
            "C1 = OTHER:",
            "C1=OTHER",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn an_other_value_of_nothing_but_whitespace_is_not_an_answer() {
        // U+00A0 and U+3000 are invisible, so a value made only of them is an empty answer wearing a
        // disguise.
        for line in [
            "C1=OTHER: \u{a0}",
            "C1=OTHER:\u{3000}",
            "C1=OTHER: \u{a0}\u{3000}\u{2009}",
            "C1=OTHER:\u{2028}",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn an_other_value_is_trimmed_of_every_kind_of_whitespace_and_keeps_its_middle() {
        assert_eq!(
            only("C1=OTHER: \u{a0}fail\u{a0}fast\u{3000}"),
            decision("C1", other("fail\u{a0}fast"))
        );
        assert_eq!(only("C1=OTHER: a  b"), decision("C1", other("a  b")));
    }

    #[test]
    fn other_must_be_spelled_with_its_colon() {
        for line in [
            "C1=OTHER x",
            "C1=OTHERS: x",
            "C1=OTHER=x",
            "C1=OTHER;x",
            "C1=OTHER : x",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn unicode_in_an_other_value_survives_character_for_character() {
        for value in [
            "한글 답변",
            "e\u{0301}quipe",
            "👩‍👩‍👧‍👦",
            "naïve — “quoted” 🚀",
            "\u{a0}between\u{a0}words\u{a0}",
        ] {
            let expected = value.trim();
            assert_eq!(
                only(&format!("C1=OTHER: {value}")),
                decision("C1", other(expected)),
                "{value:?}"
            );
        }
    }

    #[test]
    fn a_zero_width_joiner_is_content_rather_than_a_control_character() {
        // The guard rejects the Cc category only; a family emoji is held together by U+200D, which
        // must survive.
        assert_eq!(only("C1=OTHER: 👩‍👩‍👧‍👦"), decision("C1", other("👩‍👩‍👧‍👦")));
    }

    #[test]
    fn a_control_character_in_an_other_value_makes_the_line_unrecognized() {
        for line in [
            "C1=OTHER: a\u{0}b",
            "C1=OTHER: a\tb",
            "C1=OTHER: \u{1b}[2Jcleared",
            "C1=OTHER: a\u{b}b",
            "C1=OTHER: a\u{7f}b",
            "C1=OTHER: a\u{85}b",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn a_very_long_other_value_is_kept_whole() {
        let value = "x".repeat(10_000);
        assert_eq!(
            only(&format!("C1=OTHER: {value}")),
            decision("C1", other(&value))
        );
    }

    #[test]
    fn blank_and_whitespace_only_lines_are_ignored_entirely() {
        let parsed = parse_message("\n\n   \n\t\n\r\n\u{c}\n");
        assert_eq!(parsed.directives, vec![]);
        assert_eq!(parsed.unrecognized, Vec::<String>::new());
        assert_eq!(parsed.conflicts, Vec::<String>::new());
        assert!(!parsed.is_clean(), "an empty message answers nothing");
        assert_eq!(parsed.rejection_message(), None, "silence is not an error");
    }

    #[test]
    fn blank_lines_between_directives_do_not_break_their_order() {
        let parsed = parse_message("C1=REC\n\n \t \nS2=NA\n\n");
        assert_eq!(
            parsed.directives,
            vec![decision("C1", Selection::Recommendation), surface("S2")]
        );
        assert!(parsed.is_clean());
    }

    #[test]
    fn an_empty_message_yields_nothing_at_all() {
        let parsed = parse_message("");
        assert_eq!(parsed.directives, vec![]);
        assert!(parsed.unrecognized.is_empty());
        assert!(parsed.conflicts.is_empty());
        assert!(!parsed.is_clean());
        assert_eq!(parsed.rejection_message(), None);
    }

    #[test]
    fn non_ascii_whitespace_is_content_rather_than_blank() {
        rejects("\u{a0}");
        rejects("\u{3000}");
        rejects("\u{a0}C1=REC");
        rejects("C1=REC\u{a0}");
    }

    #[test]
    fn a_byte_order_mark_in_front_of_a_directive_is_not_a_directive() {
        // A pasted BOM is invisible, and an invisible character is never the difference between an
        // answer and no answer.
        rejects("\u{feff}C1=REC");
        rejects("\u{feff}CONFIRM PLAN 1A2B3C4D");
    }

    #[test]
    fn crlf_and_a_trailing_newline_parse_the_same_as_lf() {
        let expected = vec![decision("C1", Selection::Recommendation), surface("S2")];
        for text in [
            "C1=REC\nS2=NA",
            "C1=REC\r\nS2=NA",
            "C1=REC\r\nS2=NA\r\n",
            "C1=REC\nS2=NA\n",
            "\r\nC1=REC\r\n\r\nS2=NA\r\n\r\n",
        ] {
            let parsed = parse_message(text);
            assert_eq!(parsed.directives, expected, "{text:?}");
            assert!(parsed.is_clean(), "{text:?}");
        }
    }

    #[test]
    fn a_lone_carriage_return_does_not_separate_lines() {
        let parsed = parse_message("C1=REC\rS2=NA");
        assert_eq!(parsed.directives, vec![]);
        assert_eq!(parsed.unrecognized, vec!["C1=REC\rS2=NA".to_string()]);
    }

    #[test]
    fn directives_keep_the_order_they_appeared_in() {
        let parsed = parse_message("S2=NA\nC3=REC\nC1=UNKNOWN\nSHIP 1A2B3C4D\nC2=OTHER: last");
        assert_eq!(
            parsed.directives,
            vec![
                surface("S2"),
                decision("C3", Selection::Recommendation),
                decision("C1", Selection::Unknown),
                Directive::Ship {
                    challenge: "1A2B3C4D".into()
                },
                decision("C2", other("last")),
            ]
        );
        assert!(parsed.is_clean());
    }

    #[test]
    fn a_hundred_line_message_parses_in_order() {
        let text = (1..=100)
            .map(|n| format!("C{n}=ALT{n}"))
            .collect::<Vec<_>>()
            .join("\n");
        let parsed = parse_message(&text);
        assert!(parsed.is_clean());
        assert_eq!(parsed.directives.len(), 100);
        for (i, directive) in parsed.directives.iter().enumerate() {
            let n = i + 1;
            assert_eq!(
                *directive,
                decision(&format!("C{n}"), alternative(&format!("ALT{n}")))
            );
        }
    }

    #[test]
    fn valid_and_invalid_lines_are_reported_side_by_side() {
        let parsed = parse_message("ok\nC1=REC\nplease just do it\nS4=NA\nC2=rec");
        assert_eq!(
            parsed.directives,
            vec![decision("C1", Selection::Recommendation), surface("S4")]
        );
        assert_eq!(
            parsed.unrecognized,
            vec![
                "ok".to_string(),
                "please just do it".to_string(),
                "C2=rec".to_string()
            ]
        );
        assert!(parsed.conflicts.is_empty());
        assert!(
            !parsed.is_clean(),
            "a partly understood message is not clean"
        );
        let message = parsed.rejection_message().expect("something was rejected");
        assert!(message.contains("\"ok\""), "{message}");
        assert!(message.contains("\"C2=rec\""), "{message}");
        assert!(!message.contains("\"C1=REC\""), "{message}");
    }

    #[test]
    fn every_non_blank_line_is_either_a_directive_or_reported_back() {
        let lines = [
            "C1=REC",
            "ok",
            "S2=NA",
            "",
            "   ",
            "C2=OTHER: 한글",
            "C3=rec",
            "CONFIRM PLAN 1A2B3C4D",
            "CONFIRM PLAN",
            "S13=NA",
            "\u{a0}",
            "SHIP 00FFAB12",
        ];
        let parsed = parse_message(&lines.join("\n"));
        let non_blank = lines.iter().filter(|l| !l.trim_ascii().is_empty()).count();
        assert_eq!(parsed.conflicts, Vec::<String>::new());
        assert_eq!(parsed.directives.len(), 5);
        assert_eq!(parsed.unrecognized.len(), 5);
        assert_eq!(
            parsed.directives.len() + parsed.unrecognized.len(),
            non_blank
        );
    }

    #[test]
    fn an_unrecognized_line_is_reported_once_per_occurrence() {
        let parsed = parse_message("ok\nC1=REC\nok");
        assert_eq!(
            parsed.unrecognized,
            vec!["ok".to_string(), "ok".to_string()]
        );
        assert_eq!(
            parsed.directives,
            vec![decision("C1", Selection::Recommendation)]
        );
        assert!(parsed.conflicts.is_empty());
    }

    #[test]
    fn a_directive_must_start_its_line() {
        for line in [
            "please C1=REC",
            "> C1=REC",
            "- C1=REC",
            "* C1=REC",
            "1. C1=REC",
            "# C1=REC",
            "`C1=REC`",
            "**C1=REC**",
            "\"C1=REC\"",
            "(C1=REC)",
            "So: C1=REC",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn a_line_without_an_equals_sign_or_a_keyword_is_unrecognized() {
        for line in ["C1", "S1", "REC", "ALT1", "NA", "UNKNOWN", "C1 REC"] {
            rejects(line);
        }
    }

    #[test]
    fn only_the_first_equals_sign_splits_the_line() {
        rejects("C1=REC=REC");
        rejects("C1==REC");
        rejects("=REC");
        rejects("=");
        assert_eq!(only("C1=OTHER: a=b"), decision("C1", other("a=b")));
    }

    #[test]
    fn control_characters_cannot_hide_inside_a_directive() {
        for line in [
            "C1=REC\u{0}",
            "C1\u{0}=REC",
            "C1=RE\u{b}C",
            "S1=N\u{0}A",
            "SHIP 1A2B3C4\u{0}",
            "C1=REC\u{b}",
            "C1=RE\tC",
            "C1=\u{1b}[0mREC",
            "CONFIRM PLAN 1A2B3C4\u{1b}",
            "C1=REC\u{7f}",
        ] {
            rejects(line);
        }
    }

    #[test]
    fn a_control_character_never_reaches_the_rejection_message_raw() {
        let message = parse_message("C1=REC\u{0}")
            .rejection_message()
            .expect("the line was rejected");
        assert!(
            !message.contains('\u{0}'),
            "the NUL was echoed back raw: {message:?}"
        );
        assert!(message.contains("C1=REC"), "{message}");
    }

    #[test]
    fn the_rejection_message_escapes_the_quotes_and_backslashes_of_a_user_line() {
        let message = parse_message("say \"yes\" \\ no")
            .rejection_message()
            .expect("the line was rejected");
        assert!(
            message.contains(r#"  "say \"yes\" \\ no""#),
            "a quote in the user's line was not escaped: {message}"
        );
    }

    #[test]
    fn answering_the_same_decision_twice_records_a_conflict_and_no_answer() {
        let parsed = parse_message("C3=REC\nC3=ALT1");
        assert_eq!(parsed.directives, vec![]);
        assert_eq!(parsed.conflicts, vec!["C3".to_string()]);
        assert!(parsed.unrecognized.is_empty());
        assert!(!parsed.is_clean());
    }

    #[test]
    fn two_identical_answers_are_still_a_conflict() {
        let parsed = parse_message("C3=REC\nC3=REC");
        assert_eq!(parsed.directives, vec![]);
        assert_eq!(parsed.conflicts, vec!["C3".to_string()]);
    }

    #[test]
    fn a_conflict_on_one_target_leaves_the_other_answers_standing() {
        let parsed = parse_message("C1=REC\nC3=REC\nC3=ALT2\nS1=NA");
        assert_eq!(
            parsed.directives,
            vec![decision("C1", Selection::Recommendation), surface("S1")]
        );
        assert_eq!(parsed.conflicts, vec!["C3".to_string()]);
    }

    #[test]
    fn a_rejected_spelling_of_a_target_does_not_conflict_with_its_answer() {
        let parsed = parse_message("C1=REC\nC1=rec");
        assert_eq!(
            parsed.directives,
            vec![decision("C1", Selection::Recommendation)]
        );
        assert_eq!(parsed.unrecognized, vec!["C1=rec".to_string()]);
        assert!(parsed.conflicts.is_empty());
        assert!(
            !parsed.is_clean(),
            "the rejected line still has to be reported"
        );
    }

    #[test]
    fn a_surface_answered_twice_conflicts_under_its_own_id() {
        let parsed = parse_message("S7=NA\nS7=NA");
        assert_eq!(parsed.directives, vec![]);
        assert_eq!(parsed.conflicts, vec!["S7".to_string()]);
    }

    #[test]
    fn a_decision_and_a_surface_of_the_same_number_are_different_targets() {
        let parsed = parse_message("C1=REC\nS1=NA");
        assert_eq!(
            parsed.directives,
            vec![decision("C1", Selection::Recommendation), surface("S1")]
        );
        assert!(parsed.conflicts.is_empty());
        assert!(parsed.is_clean());
    }

    #[test]
    fn two_confirmations_in_one_message_confirm_nothing() {
        for text in [
            "CONFIRM PLAN 1A2B3C4D\nCONFIRM PLAN 9E8F7A6B",
            "CONFIRM PLAN 1A2B3C4D\nCONFIRM PLAN 1A2B3C4D",
        ] {
            let parsed = parse_message(text);
            assert_eq!(parsed.directives, vec![], "{text:?}");
            assert_eq!(parsed.conflicts, vec![CONFIRM_PLAN.to_string()], "{text:?}");
        }
    }

    #[test]
    fn two_ship_lines_in_one_message_ship_nothing() {
        let parsed = parse_message("SHIP 1A2B3C4D\nSHIP 9E8F7A6B");
        assert_eq!(parsed.directives, vec![]);
        assert_eq!(parsed.conflicts, vec![SHIP.to_string()]);
    }

    #[test]
    fn confirming_and_shipping_are_two_different_targets() {
        let parsed = parse_message("CONFIRM PLAN 1A2B3C4D\nSHIP 9E8F7A6B");
        assert_eq!(
            parsed.directives,
            vec![
                Directive::ConfirmPlan {
                    challenge: "1A2B3C4D".into()
                },
                Directive::Ship {
                    challenge: "9E8F7A6B".into()
                },
            ]
        );
        assert!(parsed.conflicts.is_empty());
        assert!(parsed.is_clean());
    }

    #[test]
    fn conflicts_are_sorted_and_named_once_however_often_they_repeat() {
        // C3 is contested first, so insertion order would put it before C10.
        let parsed = parse_message(
            "C3=REC\nC3=REC\nC10=REC\nC10=ALT1\nC10=UNKNOWN\nSHIP 1A2B3C4D\nSHIP 1A2B3C4D\nS2=NA",
        );
        assert_eq!(
            parsed.conflicts,
            vec!["C10".to_string(), "C3".to_string(), SHIP.to_string()]
        );
        assert_eq!(parsed.directives, vec![surface("S2")]);
    }

    #[test]
    fn every_kind_of_conflict_uses_the_documented_label_and_they_sort_together() {
        let parsed = parse_message(
            "SHIP 1A2B3C4D\nSHIP 1A2B3C4D\nS2=NA\nS2=NA\nCONFIRM PLAN 1A2B3C4D\n\
             CONFIRM PLAN 9E8F7A6B\nC3=REC\nC3=UNKNOWN\nC10=REC\nC10=REC",
        );
        assert_eq!(
            parsed.conflicts,
            vec![
                "C10".to_string(),
                "C3".to_string(),
                "CONFIRM PLAN".to_string(),
                "S2".to_string(),
                "SHIP".to_string(),
            ]
        );
        assert_eq!(parsed.directives, vec![]);
    }

    #[test]
    fn reordering_the_lines_of_a_message_leaves_the_conflicts_identical() {
        let a = parse_message("C3=REC\nC3=ALT1\nC10=REC\nC10=REC\nS2=NA\nok");
        let b = parse_message("ok\nS2=NA\nC10=REC\nC3=ALT1\nC10=REC\nC3=REC");
        assert_eq!(a.conflicts, vec!["C10".to_string(), "C3".to_string()]);
        assert_eq!(
            a, b,
            "only line order differed, and no output depends on it"
        );
    }

    #[test]
    fn a_message_is_clean_only_when_it_answers_something_and_nothing_was_ambiguous() {
        assert!(parse_message("C1=REC").is_clean());
        assert!(parse_message("CONFIRM PLAN 1A2B3C4D").is_clean());
        assert!(!parse_message("").is_clean(), "nothing answered");
        assert!(!parse_message("ok").is_clean(), "only an unrecognized line");
        assert!(
            !parse_message("C1=REC\nok").is_clean(),
            "an unrecognized line remains"
        );
        assert!(
            !parse_message("C1=REC\nC1=ALT1").is_clean(),
            "only a conflict"
        );
        assert!(
            !parse_message("C1=REC\nC1=ALT1\nS1=NA").is_clean(),
            "a conflict remains"
        );
    }

    #[test]
    fn a_rejection_message_appears_exactly_when_something_was_not_understood() {
        assert_eq!(parse_message("C1=REC").rejection_message(), None);
        assert_eq!(parse_message("   \n\n").rejection_message(), None);
        assert!(parse_message("ok").rejection_message().is_some());
        assert!(parse_message("C1=REC\nC1=REC")
            .rejection_message()
            .is_some());
    }

    #[test]
    fn the_rejection_message_quotes_every_unrecognized_line_and_names_every_conflict() {
        let parsed = parse_message("ok\nC3=REC\nship it\nC3=ALT1");
        assert_eq!(
            parsed.rejection_message().expect("something was rejected"),
            format!(
                "Hwahap could not use every line of that message.\n\
                 \n\
                 Not an answer:\n  \"ok\"\n  \"ship it\"\n\
                 \n\
                 Answered more than once, so left unanswered:\n  C3\n\
                 \n\
                 {}",
                accepted_forms()
            )
        );
    }

    #[test]
    fn the_rejection_message_omits_the_section_that_has_nothing_in_it() {
        let only_unrecognized = parse_message("ok").rejection_message().expect("rejected");
        assert!(
            only_unrecognized.contains("Not an answer:"),
            "{only_unrecognized}"
        );
        assert!(
            !only_unrecognized.contains("Answered more than once"),
            "{only_unrecognized}"
        );

        let only_conflicting = parse_message("C1=REC\nC1=ALT1")
            .rejection_message()
            .expect("rejected");
        assert!(
            only_conflicting.contains("Answered more than once, so left unanswered:\n  C1\n"),
            "{only_conflicting}"
        );
        assert!(
            !only_conflicting.contains("Not an answer:"),
            "{only_conflicting}"
        );
    }

    #[test]
    fn the_rejection_message_states_every_accepted_form() {
        let message = parse_message("ok").rejection_message().expect("rejected");
        for form in [
            "C<n>=REC".to_string(),
            "C<n>=ALT<m>".to_string(),
            "C<n>=OTHER: <value>".to_string(),
            "C<n>=UNKNOWN".to_string(),
            "S<n>=NA (n is 1-12)".to_string(),
            format!("CONFIRM PLAN <{CHALLENGE_LEN} uppercase hex characters>"),
            format!("SHIP <{CHALLENGE_LEN} uppercase hex characters>"),
            "only a space may surround the =".to_string(),
            "control character".to_string(),
        ] {
            assert!(message.contains(&form), "{form} missing from {message}");
        }
    }

    #[test]
    fn rendering_a_directive_and_parsing_it_again_returns_the_same_directive() {
        let text = "C1=REC\n\
                    C2=ALT7\n\
                    C3=OTHER: keep a=b and 한글 👩‍👩‍👧‍👦\n\
                    C4=UNKNOWN\n\
                    S11=NA\n\
                    CONFIRM PLAN 7F3A91C2\n\
                    SHIP 00FFAB12";
        let parsed = parse_message(text);
        assert!(parsed.is_clean());
        assert_eq!(parsed.directives.len(), 7);
        let rendered = parsed
            .directives
            .iter()
            .map(render)
            .collect::<Vec<_>>()
            .join("\n");
        let reparsed = parse_message(&rendered);
        assert_eq!(reparsed.directives, parsed.directives);
        assert_eq!(
            rendered, text,
            "rendering must reproduce the canonical lines"
        );
    }

    #[test]
    fn parsing_the_same_message_twice_yields_the_same_result() {
        let text = "C2=REC\nok\nC2=ALT1\nS3=NA\n\u{a0}\nSHIP 1A2B3C4D";
        assert_eq!(parse_message(text), parse_message(text));
        assert_eq!(
            parse_message(text).rejection_message(),
            parse_message(text).rejection_message()
        );
    }
}
