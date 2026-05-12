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

/// Try the query recognisers in order of specificity. Add more recognisers
/// here as later phases land; each should return early on a confident match.
fn classify_query(cleaned: &str) -> Option<QueryIntent> {
    if let Some(n) = match_page_query(cleaned) {
        return Some(QueryIntent::NavigateToPage(n));
    }
    None
}

/// Recognise `"page N"`, `"go to page N"`, `"page number N"`, with N as a
/// digit string or a number word ("one"…"twelve"). Returns the 1-based page
/// number; resolver clamps. Returns None if "page" isn't followed by a valid
/// number, so utterances like `"page next"` (handled by the Action fast
/// path) and `"page that section"` fall through to Unmatched cleanly.
fn match_page_query(cleaned: &str) -> Option<u32> {
    let tokens: Vec<&str> = cleaned.split_whitespace().collect();
    for i in 0..tokens.len() {
        if tokens[i] != "page" {
            continue;
        }
        let mut j = i + 1;
        if tokens.get(j) == Some(&"number") {
            j += 1;
        }
        if let Some(tok) = tokens.get(j) {
            if let Some(n) = parse_small_number(tok) {
                return Some(n);
            }
        }
    }
    None
}

/// Parse a small positive integer from either a digit string ("3") or a
/// number word ("three"). Stops at twelve — anything larger is expected in
/// digit form. Zero is rejected because page numbers are 1-based in voice.
fn parse_small_number(s: &str) -> Option<u32> {
    if let Ok(n) = s.parse::<u32>() {
        return if n > 0 { Some(n) } else { None };
    }
    match s {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        "six" => Some(6),
        "seven" => Some(7),
        "eight" => Some(8),
        "nine" => Some(9),
        "ten" => Some(10),
        "eleven" => Some(11),
        "twelve" => Some(12),
        _ => None,
    }
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

    // --- Phase 2: page query --------------------------------------------

    fn page_query(s: &str) -> Option<u32> {
        match route(s) {
            RoutedIntent::Query(QueryIntent::NavigateToPage(n)) => Some(n),
            _ => None,
        }
    }

    #[test]
    fn page_query_digit() {
        assert_eq!(page_query("page 3"), Some(3));
        assert_eq!(page_query("go to page 5"), Some(5));
        assert_eq!(page_query("page number 2"), Some(2));
        assert_eq!(page_query("Take me to page 7, please."), Some(7));
    }

    #[test]
    fn page_query_word_form() {
        assert_eq!(page_query("page three"), Some(3));
        assert_eq!(page_query("go to page nine"), Some(9));
        assert_eq!(page_query("page number twelve"), Some(12));
    }

    #[test]
    fn page_query_not_a_match() {
        // "page next" / "page back" are Action fast-path matches, not queries.
        // Confirm they still route to actions, not NavigateToPage.
        assert_eq!(route("page next"), RoutedIntent::Action(Action::PageNext));
        assert_eq!(route("page back"), RoutedIntent::Action(Action::PagePrev));
        // Bare "page" with no number is Unmatched, not NavigateToPage(0).
        assert_eq!(route("page"), RoutedIntent::Unmatched);
        // Zero is rejected — voice page numbers are 1-based.
        assert_eq!(page_query("page 0"), None);
        // A non-number token after "page" doesn't trigger a query.
        assert_eq!(page_query("page that section"), None);
    }
}
