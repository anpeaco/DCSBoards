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
    /// panel is open; otherwise the dispatcher drops it. The results-
    /// panel flow (voice queries phase 5b) hasn't shipped yet, so no
    /// code path constructs this variant — kept on the enum because the
    /// classifier is already wired to match it.
    #[allow(dead_code)]
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
    // Issue #17: bare "start" / "go" mean "advance" — they're the words
    // users naturally say to exit the armed-after-section-jump wait.
    // They have to short-circuit before the substring table because that
    // would clobber phrasal commands like "go back", "go to page 3",
    // "go on", "start over", "start reading". Treat them as exact-only.
    if cleaned == "start" || cleaned == "go" {
        return RoutedIntent::Action(Action::Next);
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
    // List-sections runs first — "what sections are in this tab" contains
    // the literal word "this" which is harmless but the recogniser is
    // self-contained and the cheapest check.
    if matches_list_sections(cleaned) {
        return Some(QueryIntent::ListSections);
    }
    // Page query before section query — "go to page 3" stays a page query.
    if let Some(n) = match_page_query(cleaned) {
        return Some(QueryIntent::NavigateToPage(n));
    }
    // The section-query prefix recogniser produces a raw target string. We
    // then decide whether the target names a tab ("takeoff tab", "JDAM
    // checklist") or a section (default). The suffix marker — "tab" or
    // "checklist" — is the disambiguator and gets stripped from the target.
    if let Some(target) = match_section_query(cleaned) {
        if let Some(tab_target) = strip_tab_suffix(&target) {
            return Some(QueryIntent::NavigateToTab(tab_target));
        }
        return Some(QueryIntent::NavigateToSection(target));
    }
    None
}

/// Recognise "what sections are in this tab" and the obvious variants.
/// Matched as substrings so trailing politeness doesn't ruin the match.
fn matches_list_sections(cleaned: &str) -> bool {
    // Apostrophes are stripped by normalise, so "what's" → "whats".
    const PHRASES: &[&str] = &[
        "what sections",
        "list sections",
        "list the sections",
        "sections in this tab",
        "whats in this tab",
        "what is in this tab",
    ];
    PHRASES.iter().any(|p| contains_phrase(cleaned, p))
}

/// Recognise "<X> tab" / "<X> checklist" as an explicit tab-navigation
/// intent. Returns the target with the suffix stripped, or None if the
/// query is just a plain section target.
fn strip_tab_suffix(s: &str) -> Option<String> {
    const SUFFIXES: &[&str] = &[" tab", " checklist"];
    for suf in SUFFIXES {
        if let Some(prefix) = s.strip_suffix(suf) {
            let prefix = prefix.trim();
            if !prefix.is_empty() {
                return Some(prefix.to_string());
            }
        }
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

/// Recognise free-text section navigation prefixes and return the target
/// string. Trailing chatter ("the agm 65 section", "please") is left in
/// place — the resolver does fuzzy matching, so a little noise is fine.
fn match_section_query(cleaned: &str) -> Option<String> {
    // Direct prefixes — listed longest-first to avoid "go to" stealing
    // matches that "go to the" would also accept (we strip "the " later).
    const PREFIXES: &[&str] = &[
        "take me to ",
        "navigate to ",
        "search for ",
        "show me ",
        "jump to ",
        "go to ",
        "get to ",
        "find ",
    ];
    for p in PREFIXES {
        if let Some(rest) = cleaned.strip_prefix(p) {
            let rest = rest.trim();
            if !rest.is_empty() {
                return Some(strip_leading_article(rest));
            }
        }
    }
    // "how do I [verb] X" / "how to [verb] X" — verbs are aviation-flavoured
    // and listed longest-first so "set up" beats the prefix-shorter "set".
    if let Some(rest) = strip_how_prefix(cleaned) {
        let rest = rest.trim();
        if !rest.is_empty() {
            return Some(strip_leading_article(rest));
        }
    }
    None
}

fn strip_how_prefix(s: &str) -> Option<&str> {
    const HOW: &[&str] = &["how do i ", "how to "];
    const VERBS: &[&str] = &[
        // longest-first: "set up" before "set", "prepare" before "prep"
        "set up ",
        "prepare ",
        "configure ",
        "employ ",
        "prep ",
        "fire ",
        "set ",
        "use ",
        "do ",
    ];
    for hp in HOW {
        if let Some(after_how) = s.strip_prefix(hp) {
            for verb in VERBS {
                if let Some(target) = after_how.strip_prefix(verb) {
                    return Some(target);
                }
            }
        }
    }
    None
}

/// Drop a leading "the " so "find the JDAM section" matches "JDAM section"
/// instead of fuzzy-scoring the literal "the" prefix down.
fn strip_leading_article(s: &str) -> String {
    s.strip_prefix("the ").unwrap_or(s).trim().to_string()
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
        } else if (ch.is_whitespace() || ch == '-' || ch == '_') && !last_space {
            out.push(' ');
            last_space = true;
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

/// Example phrasings for the query-intent recognisers. Kept here rather
/// than alongside `classify_query` so the help panel can render both the
/// static Action rules and the free-form query layer in one list. Edit when
/// you add a recogniser — these are the source of truth users see.
pub fn query_examples() -> &'static [(&'static str, &'static [&'static str])] {
    &[
        (
            "Go to page",
            &[
                "go to page 3",
                "page 5",
                "page number 2",
                "page three",
            ],
        ),
        (
            "Go to section",
            &[
                "go to AGM-65",
                "take me to the maverick section",
                "find the JDAM section",
                "how do I employ the AGM-65",
                "search for AGM-65 employment",
            ],
        ),
        (
            "Go to tab",
            &[
                "go to the takeoff tab",
                "switch to the JDAM checklist",
            ],
        ),
        (
            "List sections",
            &[
                "what sections are in this tab",
                "list sections",
                "what's in this tab",
            ],
        ),
    ]
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

    /// Issue #17: "start" and "go" exit the armed-after-section-jump state
    /// and read the first step. They route to Next so they also work as
    /// plain advance when the controller isn't armed.
    #[test]
    fn start_and_go_route_to_next() {
        assert_eq!(action("start"), Some(Action::Next));
        assert_eq!(action("Start."), Some(Action::Next));
        assert_eq!(action("go"), Some(Action::Next));
        assert_eq!(action("Go!"), Some(Action::Next));
    }

    /// The "start" / "go" short-circuit must NOT swallow phrasal commands
    /// that contain those words. These cases all existed pre-#17 and
    /// would silently regress to Next if the routing order is wrong.
    #[test]
    fn start_and_go_do_not_eat_phrasal_commands() {
        assert_eq!(action("go back"), Some(Action::Previous));
        assert_eq!(action("go on"), Some(Action::TogglePlay));
        assert_eq!(action("start over"), Some(Action::RestartSection));
        assert_eq!(action("start reading"), Some(Action::TogglePlay));
        // "go to page 3" is a query, not an action.
        assert_eq!(page_query("go to page 3"), Some(3));
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
        // A non-number token after "page" doesn't trigger a page query,
        // but the section recogniser does pick it up.
        assert_eq!(page_query("page that section"), None);
    }

    // --- Phase 3: section query -----------------------------------------

    fn section_query(s: &str) -> Option<String> {
        match route(s) {
            RoutedIntent::Query(QueryIntent::NavigateToSection(t)) => Some(t),
            _ => None,
        }
    }

    #[test]
    fn section_query_simple_prefixes() {
        assert_eq!(section_query("go to AGM-65"), Some("agm 65".to_string()));
        assert_eq!(
            section_query("take me to the maverick section"),
            Some("maverick section".to_string()) // "the " stripped
        );
        assert_eq!(
            section_query("search for AGM-65 employment"),
            Some("agm 65 employment".to_string())
        );
        assert_eq!(
            section_query("find the JDAM section"),
            Some("jdam section".to_string())
        );
        assert_eq!(
            section_query("navigate to landing"),
            Some("landing".to_string())
        );
    }

    #[test]
    fn section_query_how_do_i_form() {
        assert_eq!(
            section_query("how do I employ the AGM-65"),
            Some("agm 65".to_string())
        );
        assert_eq!(
            section_query("how to fire the maverick"),
            Some("maverick".to_string())
        );
        // "set up" must beat "set" so we don't get "up the ..."
        assert_eq!(
            section_query("how do I set up the JDAM"),
            Some("jdam".to_string())
        );
    }

    #[test]
    fn section_query_not_a_match() {
        // Prefix alone with no target is not a query.
        assert_eq!(route("go to"), RoutedIntent::Unmatched);
        assert_eq!(route("find"), RoutedIntent::Unmatched);
        // "go back" is the Previous action — not a section query.
        assert_eq!(route("go back"), RoutedIntent::Action(Action::Previous));
        // "find the switch" is going to be Action::WhereIs (issue #3, other
        // session); today it just routes as a section query for "switch".
        // Regression check that the Action::WhereIs phrase list overrides
        // this when issue #3 lands.
    }

    // --- Phase 4: tab query ---------------------------------------------

    fn tab_query(s: &str) -> Option<String> {
        match route(s) {
            RoutedIntent::Query(QueryIntent::NavigateToTab(t)) => Some(t),
            _ => None,
        }
    }

    #[test]
    fn tab_query_suffix_disambiguates() {
        // "X tab" / "X checklist" suffix → NavigateToTab; otherwise it
        // stays NavigateToSection.
        assert_eq!(tab_query("go to the takeoff tab"), Some("takeoff".to_string()));
        assert_eq!(
            tab_query("take me to the JDAM checklist"),
            Some("jdam".to_string())
        );
        assert_eq!(
            tab_query("switch to scratchpad tab"),
            None // "switch to" isn't a recognised prefix; falls through.
        );
        assert_eq!(
            tab_query("navigate to the tactical tab"),
            Some("tactical".to_string())
        );
        // Plain target with no suffix → section query, not tab query.
        assert_eq!(tab_query("go to landing"), None);
        assert_eq!(
            section_query("go to landing"),
            Some("landing".to_string())
        );
    }

    // --- Phase 5a: list-sections query ----------------------------------

    #[test]
    fn list_sections_phrases() {
        assert_eq!(
            route("what sections are in this tab"),
            RoutedIntent::Query(QueryIntent::ListSections)
        );
        assert_eq!(
            route("list sections"),
            RoutedIntent::Query(QueryIntent::ListSections)
        );
        assert_eq!(
            route("what's in this tab?"),
            RoutedIntent::Query(QueryIntent::ListSections)
        );
        assert_eq!(
            route("sections in this tab"),
            RoutedIntent::Query(QueryIntent::ListSections)
        );
    }

    /// Critical regression: existing Action phrases must keep beating any
    /// new section-query prefix. "what was that" → ReadCurrent. "where am
    /// I" → ReadSection. "next page" → PageNext.
    #[test]
    fn existing_actions_still_win() {
        assert_eq!(
            route("what was that"),
            RoutedIntent::Action(Action::ReadCurrent)
        );
        assert_eq!(route("where am I"), RoutedIntent::Action(Action::ReadSection));
        assert_eq!(route("next page"), RoutedIntent::Action(Action::PageNext));
        assert_eq!(
            route("previous heading"),
            RoutedIntent::Action(Action::PrevHeading)
        );
    }
}
