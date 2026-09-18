//! TypeSafe / Jev judgments for card state and card titles.
//!
//! **Status: draft, NOT wired in.** This file holds only questions, thresholds
//! and the decision mapping — deliberately no HTTP client and no call site, so
//! adding it cannot change behaviour. Every threshold below records the
//! measurement it came from; all of them are n=5 preliminary, not validated.
//!
//! Why this exists: `tmux::extract_last_prompt()` guesses the user's prompt from
//! pane text with a hardcoded skip list, and `tmux::inspect_status()` derives
//! IDLE/WORKING/EXITED from the process tree. Measured on 5 live cards, the
//! title heuristic was right on 1 of 5 (the one card that reads its prompt from
//! the opencode DB) and produced a radio-station name, a cost-counter fragment
//! and a compiler-warning fragment as "prompts". The judgments below replace the
//! guess, not the ground truth: when the opencode DB has the exact prompt, that
//! wins and Jev is never consulted for that card.
//!
//! The skill's rules these constants encode:
//!   - code owns the workflow; each question is one narrow judgment
//!   - every independent question goes in ONE request (measured: 3.0x fewer
//!     tokens and 3.7x less wall time than three separate calls)
//!   - coverage: every candidate line is offered, or the model cannot choose it
//!   - thresholds are policy, so they live here, not scattered at call sites

#![allow(dead_code)]

/// Model alias. Pin a versioned id instead once thresholds are tuned against a
/// release, so an alias move cannot silently retune this app.
pub const MODEL: &str = "jev-latest";

pub const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";

// ─── Request budget ───────────────────────────────────────────────────────────

/// Bounded state: the daemon must never ship an unbounded scrollback.
pub const MAX_STATE_LINES: usize = 64;
pub const MAX_STATE_BYTES: usize = 12_000;

/// Hard floor between two calls for the same card. A 1s refresh loop is
/// unaffordable: measured ~5.2k input tokens per card per call is ~$0.00022, so
/// 5 cards every second would be roughly $94/day. At 60s it is ~$1.60/day.
pub const MIN_SECONDS_BETWEEN_CALLS: u64 = 60;

/// Ceiling on requests per minute across all cards, so a bug cannot burn quota.
pub const MAX_CALLS_PER_MINUTE: u32 = 30;

pub const REQUEST_TIMEOUT_SECONDS: u64 = 20;

/// Retry only on the two statuses the API documents as transient, and only with
/// backoff. Everything else is a bug or a bad key and must not be retried.
pub const RETRY_STATUSES: [u16; 2] = [429, 529];
pub const RETRY_ATTEMPTS: u8 = 3;

/// Answer with nothing rather than block the GTK main loop or the daemon's IPC
/// loop. Callers must treat `None` as "keep the previous value", never as a
/// state transition.
pub const FALLBACK_TO_HEURISTICS: bool = true;

// ─── Question: harness state ──────────────────────────────────────────────────

/// Choice options. Keys are the wire values; the doc comment on each is the
/// rubric sent as `criteria`.
pub mod state_option {
    pub const AGENT_WORKING: &str = "agent_working";
    pub const WAITING_FOR_INPUT: &str = "waiting_for_input";
    pub const BLOCKED_ON_USER: &str = "blocked_on_user";
    pub const NO_AGENT: &str = "no_agent";
    pub const ERRORED: &str = "errored";
}

/// Rubric text, verbatim what was measured. Wording here is the feature: the
/// `agent_at_rest` note was added after the model confused an idle opencode card
/// with `no_agent`, and `chrome`/`spinner` after it read status bars as content.
pub const STATE_INSTRUCTIONS: &str = "\
Decide what this terminal card is doing right now, for a status pill.\n\
Bottom status bars, token/cost counters, key-hint footers and box borders are chrome.\n\
A braille spinner character (U+2800..U+28FF) means the agent is still producing output.\n\
If this card runs an AI agent whose turn has finished and whose input box is empty, that is waiting_for_input.";

/// Minimum `confidence` before the state answer may be acted on.
///
/// Measured (n=5 cards, 2 trials, identical both trials): every correct answer
/// scored 0.85–0.99; the one wrong answer scored 0.51–0.59. 0.80 separated them
/// with margin. **Unvalidated** — 5 cards is not a validation set; the
/// documented jaggedness of the model is the reason this is a constant and not
/// a literal at a call site.
pub const STATE_MIN_CONFIDENCE: f64 = 0.80;

// ─── Question: which line is the user's own input ─────────────────────────────
//
// One Choice over every visible line id plus `none`, not a Noul per line:
// measured the same answers for 1/3 the tokens and 1/4 the wall time.

pub const LINE_INSTRUCTIONS: &str = "\
Choose the line id holding text the USER typed as input.\n\
Counts: a prompt typed into an AI agent's composer, or a shell command the user typed at a shell prompt.\n\
Does not count: agent or program output; a shell command echoed inside the agent's own transcript or tool-call output; todo lists; spinner lines; status bars; key-hint footers; box borders; markdown tables the agent wrote.\n\
If several qualify, choose the most recent one.\n\
Choose none when no visible line is text the user typed.";

pub const NO_MATCH_OPTION: &str = "none";

/// Minimum probability of the *chosen* option (not confidence).
///
/// Measured: correct picks scored 0.85 / 0.89, correct `none` answers 0.93 and
/// 1.00, and the one wrong pick (`$ cargo build …` echoed inside an agent's own
/// transcript) scored 0.77. 0.80 rejects the error; 0.75 would not have.
///
/// Caveat worth keeping visible: on one card where `none` was correct the
/// reported *confidence* was only 0.60, so a confidence gate here would have
/// thrown away a correct answer. Gate on probability, route on confidence.
pub const LINE_MIN_PROBABILITY: f64 = 0.80;

// ─── Question: is there any user input at all ─────────────────────────────────

pub const ANY_INPUT_INSTRUCTIONS: &str =
    "Does any visible line contain text the user typed as input (an agent prompt or a shell command)?";

/// The cleanest separation of anything measured: correct `yes` scored
/// 0.91 / 0.94 / 0.95, correct `no` scored 0.07 / 0.18 / 0.21 / 0.47 / 0.49,
/// and the one wrong answer scored 0.66. 0.80 rejects the error with a wide
/// margin on both sides. Use this as the gate that decides whether the title
/// may change at all; use `LINE_MIN_PROBABILITY` to decide *what* to show.
pub const ANY_INPUT_MIN_NOUL: f64 = 0.80;

// ─── Decision mapping ─────────────────────────────────────────────────────────

/// What the overlay and the phone may do with an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Act on the answer.
    Apply,
    /// Store the answer for display or logging, but do not change visible state.
    Observe,
    /// Model and heuristics disagree, or the model is unsure: keep what is shown.
    Keep,
}

/// Confidence-gated routing. `None` means the request failed, timed out, or the
/// feature is switched off — never a transition.
pub fn verdict(choice_confidence: f64) -> Verdict {
    if choice_confidence >= STATE_MIN_CONFIDENCE {
        Verdict::Apply
    } else if choice_confidence >= STATE_MIN_CONFIDENCE - 0.15 {
        Verdict::Observe
    } else {
        Verdict::Keep
    }
}

/// Map a state answer onto the existing `SessionStatus` vocabulary so nothing
/// downstream has to learn a new type. `None` = keep the heuristic's answer.
pub fn status_for(choice: &str) -> Option<(&'static str, &'static str)> {
    match choice {
        state_option::AGENT_WORKING => Some(("WORKING", "● WORKING")),
        state_option::BLOCKED_ON_USER => Some(("BLOCKED", "▲ NEEDS YOU")),
        state_option::WAITING_FOR_INPUT => Some(("IDLE", "○ READY")),
        state_option::NO_AGENT => Some(("IDLE", "○ IDLE")),
        state_option::ERRORED => Some(("ERROR", "✖ ERROR")),
        _ => None,
    }
}

// ─── Not yet designed ─────────────────────────────────────────────────────────
//
// Deliberately absent until there is a reason to add them:
//   - free-text command routing from the phone (intent routing/function calling)
//   - semantic search across cards (re-ranking)
// Both are plausible; neither is justified by a measurement yet.
