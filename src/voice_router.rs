//! Transcript → RoutedIntent mapping for voice commands.
//!
//! Two layers:
//! 1. **Action fast path** — a hand-written phrase table (`RULES`), longer
//!    phrases first, anchored to word boundaries. STT outputs are noisy so we
//!    lowercase, strip punctuation, and substring-match.
//! 2. **Query classifier** — if no Action rule matches, try to classify the
//!    transcript as a structured query (navigate by page/section/tab, list
//!    sections, pick a result). The classifier is deterministic prefix /
//!    keyword matching — no ML. Stays consistent with the on-device constraint.

use crate::actions::Action;

/// What the voice router decided about a transcript.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedIntent {
    /// One of the predefined actions (Next, PageNext, Cancel, …). Fast path.
    Action(Action),
    /// A structured query the caller needs to resolve against the loaded
    /// content (navigate to a section by name, jump to a page index, …).
    Query(QueryIntent),
    /// Nothing matched. Caller surfaces the raw transcript.
    Unmatched,
}

/// Structured-query forms the router can recognise. Resolution against the
/// loaded pages / tabs is the caller's job (see src/checklist/resolver.rs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryIntent {
    /// "search for AGM-65", "take me to the takeoff checklist", "how do I
    /// employ the maverick". Free-text target; resolver decides whether it
    /// names a section, an item, or (later) an alias.
    NavigateToSection(String),
    /// "get to the takeoff tab", explicit tab-only intent. Matched against
    /// Tab.label + Tab.id.
    NavigateToTab(String),
    /// "go to page 3". 1-based in voice; resolver clamps.
    NavigateToPage(u32),
    /// "what sections are in this tab".
    ListSections,
    /// "the second one" / "number three". Only honoured while a results
    /// panel is open; otherwise the dispatcher drops it.
    PickResult(u32),
}

/// Resolve a transcript to a routed intent. Action rules run first; on a
/// miss the query classifier gets a turn; on a miss there too the caller
/// sees `Unmatched`.
pub fn route(transcript: &str) -> RoutedIntent {
    let cleaned = normalise(transcript);
    if cleaned.is_empty() {
        return RoutedIntent::Unmatched;
    }
    for (phrases, action) in RULES.iter() {
        for phrase in *phrases {
            if contains_phrase(&cleaned, phrase) {
                return RoutedIntent::Action(*action);
            }
        }
    }
    if let Some(query) = classify_query(&cleaned) {
        return RoutedIntent::Query(query);
    }
    RoutedIntent::Unmatched
}

/// Phase-1 placeholder. Subsequent phases fill in page / section / tab /
/// list / pick recognisers. Leaving the function in place now keeps the
/// dispatch wiring honest from day one.
fn classify_query(_cleaned: &str) -> Option<QueryIntent> {
    None
}

/// Pre-process the transcript for matching: lowercase, strip ASCII punctuation,
/// collapse runs of whitespace.
fn normalise(s: &str) -> String {
    let lowered = s.to_ascii_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut last_space = true;
    for ch in lowered.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_space = false;
        } else if ch.is_whitespace() || ch == '-' || ch == '_' {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        }
        // Other punctuation (commas, periods, apostrophes) just drops.
    }
    out.trim().to_string()
}

/// Substring match anchored to word boundaries: "back" must not match
/// "backslash". Both `haystack` and `needle` have already been normalised.
fn contains_phrase(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let mut start = 0;
    while let Some(pos) = haystack[start..].find(needle) {
        let abs = start + pos;
        let before_ok = abs == 0 || haystack.as_bytes()[abs - 1] == b' ';
        let after_idx = abs + needle.len();
        let after_ok =
            after_idx == haystack.len() || haystack.as_bytes()[after_idx] == b' ';
        if before_ok && after_ok {
            return true;
        }
        start = abs + 1;
        if start >= haystack.len() {
            break;
        }
    }
    false
}

/// Read-only view of the rules table for the help panel.
pub fn all_rules() -> &'static [(&'static [&'static str], Action)] {
    RULES
}

/// Phrase → action table. Order matters: longer/more-specific phrases first
/// so "next page" beats the bare "next" rule. Each entry lists every variant
/// we've seen in the wild; add more freely.
const RULES: &[(&[&str], Action)] = &[
    // Page navigation
    (
        &["next page", "page next", "page down", "page forward"],
        Action::PageNext,
    ),
    (
        &["previous page", "prev page", "page previous", "page up", "page back"],
        Action::PagePrev,
    ),
    // Heading / section navigation
    (
        &[
            "next heading",
            "next section",
            "next group",
            "skip section",
            "skip heading",
        ],
        Action::NextHeading,
    ),
    (
        &[
            "previous heading",
            "previous section",
            "prev heading",
            "prev section",
            "back section",
        ],
        Action::PrevHeading,
    ),
    // Tab cycling
    (&["next tab", "tab next", "tab forward"], Action::CycleTabNext),
    (
        &["previous tab", "prev tab", "tab back", "tab previous"],
        Action::CycleTabPrev,
    ),
    // Per-item navigation. Previous goes first so "go back" wins over the
    // Next rules' "go" — ambiguous single words like "go" are intentionally
    // omitted from the Next list to avoid masking phrasal commands.
    (
        &[
            "previous item",
            "previous step",
            "previous line",
            "go back",
            "previous",
            "prev",
            "back",
            "undo",
        ],
        Action::Previous,
    ),
    (
        &[
            "next item",
            "next step",
            "next line",
            "next",
            "advance",
            "continue",
            "proceed",
            "okay",
            "ok",
            "done",
            "got it",
            "check",
            "yup",
            "yep",
            "yeah",
            "yes"
        ],
        Action::Next,
    ),
    // Restart current section — placed before ReadSection so "start section
    // over" beats the section-query patterns.
    (
        &[
            "restart section",
            "restart this section",
            "start section over",
            "restart heading",
            "from the top",
            "back to top",
            "start over",
            "again from start",
        ],
        Action::RestartSection,
    ),
    // Section query — must come before ReadCurrent so "what section" beats
    // ReadCurrent's "what was that" patterns when STT mishears.
    (
        &[
            "what section",
            "which section",
            "current section",
            "what heading",
            "which heading",
            "where am i",
        ],
        Action::ReadSection,
    ),
    // Read / repeat / play-pause
    (
        &[
            "read it again",
            "read again",
            "say again",
            "repeat",
            "again",
            "what was that",
        ],
        Action::ReadCurrent,
    ),
    (
        &[
            "play", "read", "start reading", "speak", "go on", "pause", "stop reading",
        ],
        Action::TogglePlay,
    ),
    // Toggles
    (
        &[
            "more info on",
            "more info off",
            "more info",
            "toggle notes",
            "toggle info",
            "extra info",
            "extra detail",
            "details on",
            "details off",
        ],
        Action::ToggleReadNotes,
    ),
    (
        &[
            "click through",
            "toggle click through",
            "pass through",
            "ghost mode",
        ],
        Action::ToggleClickThrough,
    ),
    (
        &[
            "hot mic",
            "open mic",
            "mic on",
            "mic off",
            "toggle mic",
            "listen mode",
        ],
        Action::HotMicToggle,
    ),
    // Panel + cancel
    (
        &[
            "voice commands",
            "list commands",
            "help commands",
            "what can i say",
            "show commands",
            "command help",
        ],
        Action::OpenVoiceCommands,
    ),
    (&["settings", "open settings", "preferences"], Action::OpenSettings),
    (
        &["cancel", "stop", "never mind", "nevermind", "shut up", "quiet"],
        Action::Cancel,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn action(s: &str) -> Option<Action> {
        match route(s) {
            RoutedIntent::Action(a) => Some(a),
            _ => None,
        }
    }

    #[test]
    fn next_item() {
        assert_eq!(action("Next"), Some(Action::Next));
        assert_eq!(action("next."), Some(Action::Next));
        assert_eq!(action("Okay"), Some(Action::Next));
        assert_eq!(action("got it"), Some(Action::Next));
    }

    #[test]
    fn previous_item() {
        assert_eq!(action("Back"), Some(Action::Previous));
        assert_eq!(action("go back"), Some(Action::Previous));
        assert_eq!(action("previous step"), Some(Action::Previous));
    }

    #[test]
    fn longer_phrase_wins() {
        assert_eq!(action("next page"), Some(Action::PageNext));
        assert_eq!(action("previous heading"), Some(Action::PrevHeading));
        assert_eq!(action("Next tab please"), Some(Action::CycleTabNext));
    }

    #[test]
    fn word_boundary() {
        // "back" must not match "background"
        assert_eq!(route("background noise"), RoutedIntent::Unmatched);
        // "next" must not match "context"
        assert_eq!(route("the context is"), RoutedIntent::Unmatched);
    }

    #[test]
    fn punctuation_tolerant() {
        assert_eq!(action("Repeat, please."), Some(Action::ReadCurrent));
        assert_eq!(action("CANCEL!!!"), Some(Action::Cancel));
    }

    #[test]
    fn empty_or_garbage() {
        assert_eq!(route(""), RoutedIntent::Unmatched);
        assert_eq!(route("   "), RoutedIntent::Unmatched);
        assert_eq!(route("hmmmm"), RoutedIntent::Unmatched);
    }
}
