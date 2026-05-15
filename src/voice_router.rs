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
use std::collections::HashMap;

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
/// Issue #15 step 2: apply post-STT corrections to a raw transcript
/// before it reaches `route()`. Matching is word-boundary, lower-cased,
/// longest-key-first so "jay dam" beats "jay" when both are mapped.
///
/// Returns `Some(corrected)` when at least one rule fires so the
/// caller can both log the rewrite and feed the corrected form into
/// routing. `None` means no rule matched and the original transcript
/// should be used unchanged.
///
/// Lives here (rather than in `query_aliases.rs`) because corrections
/// operate on the unprocessed STT output, while `QueryAliases::rewrite`
/// operates on already-classified query targets. Same algorithm, two
/// different layers; folding them together would conflate scopes.
pub fn apply_corrections(transcript: &str, corrections: &HashMap<String, String>) -> Option<String> {
    if corrections.is_empty() {
        return None;
    }
    let lower = transcript.to_lowercase();
    let mut keys: Vec<&String> = corrections.keys().collect();
    keys.sort_by_key(|k| std::cmp::Reverse(k.len()));
    let mut out = lower;
    let mut fired = false;
    for key in keys {
        let needle = key.to_lowercase();
        if needle.is_empty() {
            continue;
        }
        let Some(value) = corrections.get(key) else { continue };
        let next = replace_word(&out, &needle, value);
        if next != out {
            fired = true;
            out = next;
        }
    }
    if fired {
        Some(out)
    } else {
        None
    }
}

/// Replace every word-boundary occurrence of `needle` (already lower-
/// cased) with `replacement` in `haystack` (already lower-cased). Word
/// boundary = start-of-string or preceded by ASCII whitespace AND end-
/// of-string or followed by ASCII whitespace. Duplicated from
/// `query_aliases::replace_word` rather than imported because the two
/// callers shouldn't depend on each other; if a third site needs the
/// same logic, extract to a shared `text_utils` module then.
fn replace_word(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() || haystack.is_empty() {
        return haystack.to_string();
    }
    let bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    if n_bytes.len() > bytes.len() {
        return haystack.to_string();
    }
    let mut out = String::with_capacity(haystack.len());
    let mut last = 0usize;
    let mut from = 0usize;
    while from <= bytes.len().saturating_sub(n_bytes.len()) {
        let Some(pos) = haystack[from..].find(needle) else {
            break;
        };
        let abs = from + pos;
        let before_ok = abs == 0 || bytes[abs - 1] == b' ';
        let after_idx = abs + n_bytes.len();
        let after_ok = after_idx == bytes.len() || bytes[after_idx] == b' ';
        if before_ok && after_ok {
            out.push_str(&haystack[last..abs]);
            out.push_str(replacement);
            last = after_idx;
            from = after_idx;
        } else {
            from = abs + 1;
        }
    }
    out.push_str(&haystack[last..]);
    out
}

/// Issue #15 step 3: fuzzy fallback against the action phrase table.
/// Called only after the substring `RULES` pass and the query
/// classifier both miss — by then we know the transcript didn't
/// trigger anything by exact-ish match, so a Jaro-Winkler score above
/// `threshold` is worth treating as a misrecognition rather than
/// silence.
///
/// Returns the top `n` candidates above threshold, sorted by score
/// descending. The route uses index 0; the rest go in the debug log
/// so the user can see why a misrecognition landed where it did.
fn fuzzy_match_actions(
    cleaned: &str,
    threshold: f32,
    n: usize,
) -> Vec<(Action, f32, &'static str)> {
    if cleaned.is_empty() || threshold <= 0.0 || n == 0 {
        return Vec::new();
    }
    let mut all: Vec<(Action, f32, &'static str)> = Vec::new();
    for (phrases, action) in RULES.iter() {
        for phrase in *phrases {
            let score = strsim::jaro_winkler(cleaned, phrase) as f32;
            if score >= threshold {
                all.push((*action, score, *phrase));
            }
        }
    }
    all.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    all.truncate(n);
    all
}

/// Resolve a transcript to a routed intent. The fuzzy-fallback pass
/// against the action phrase table only fires when `fuzzy_threshold >
/// 0.0` (issue #15 step 3); call sites pass `0.0` for deterministic
/// no-fuzzy routing (tests, the initial release of the substring
/// pipeline) or the user-configured threshold from `config.toml
/// [stt] fuzzy_threshold` (default 0.85) for production.
pub fn route_with_fuzzy(transcript: &str, fuzzy_threshold: f32) -> RoutedIntent {
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
    // Last-chance fuzzy match: only fires above threshold, only on
    // transcripts that nothing else recognised. Safer than threading
    // it into the substring pass — the substring table has hand-tuned
    // anti-collision rules ("go back" before "go" before "go to ...")
    // that a fuzzy score would happily break.
    if fuzzy_threshold > 0.0 {
        let candidates = fuzzy_match_actions(&cleaned, fuzzy_threshold, 3);
        if let Some(&(action, score, phrase)) = candidates.first() {
            eprintln!(
                "[voice] fuzzy \"{cleaned}\" → {action:?} via \"{phrase}\" \
                 (jw {score:.2}, threshold {fuzzy_threshold:.2})"
            );
            for &(alt_action, alt_score, alt_phrase) in candidates.iter().skip(1) {
                eprintln!(
                    "[voice]   alt: {alt_action:?} via \"{alt_phrase}\" (jw {alt_score:.2})"
                );
            }
            return RoutedIntent::Action(action);
        }
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
    // VR overlay positioning (#30 phase 4). Place these at the END of
    // the table so they don't accidentally steal "place" / "move" /
    // "reset" / "bigger" / "smaller" tokens from other commands. Most
    // entries are multi-word phrases anyway, which lets the longest-
    // phrase-first match prefer them when the user is being explicit.
    (
        &[
            "place kneeboard here",
            "place board here",
            "kneeboard here",
            "place here",
            "snap here",
            "reposition here",
        ],
        Action::VrPlaceHere,
    ),
    (
        &[
            "move kneeboard closer",
            "move board closer",
            "kneeboard closer",
            "move closer",
            "bring closer",
            "closer to me",
        ],
        Action::VrMoveCloser,
    ),
    (
        &[
            "move kneeboard further",
            "move kneeboard farther",
            "kneeboard further",
            "move further",
            "move farther",
            "push back",
            "further away",
        ],
        Action::VrMoveFurther,
    ),
    (
        &[
            "move kneeboard left",
            "kneeboard left",
            "move left",
            "shift left",
        ],
        Action::VrMoveLeft,
    ),
    (
        &[
            "move kneeboard right",
            "kneeboard right",
            "move right",
            "shift right",
        ],
        Action::VrMoveRight,
    ),
    (
        &[
            "move kneeboard up",
            "kneeboard up",
            "move up",
            "raise kneeboard",
        ],
        Action::VrMoveUp,
    ),
    (
        &[
            "move kneeboard down",
            "kneeboard down",
            "move down",
            "lower kneeboard",
        ],
        Action::VrMoveDown,
    ),
    (
        &[
            "kneeboard bigger",
            "make bigger",
            "make it bigger",
            "make larger",
            "bigger kneeboard",
            "scale up",
        ],
        Action::VrSizeUp,
    ),
    (
        &[
            "kneeboard smaller",
            "make smaller",
            "make it smaller",
            "smaller kneeboard",
            "scale down",
        ],
        Action::VrSizeDown,
    ),
    (
        &[
            "reset kneeboard",
            "reset position",
            "default position",
            "kneeboard default",
        ],
        Action::VrResetPose,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    /// Test convenience: route with fuzzy disabled so the substring +
    /// classify behaviour is exercised deterministically. Issue #15's
    /// fuzzy path has its own targeted tests below.
    fn route(transcript: &str) -> RoutedIntent {
        route_with_fuzzy(transcript, 0.0)
    }

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

    // --- Issue #15: post-STT corrections + fuzzy fallback ---------------

    fn corrections(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn corrections_no_rules_is_passthrough() {
        let empty: HashMap<String, String> = HashMap::new();
        assert_eq!(apply_corrections("home", &empty), None);
    }

    #[test]
    fn corrections_no_match_returns_none() {
        let map = corrections(&[("home", "HARM")]);
        // "homestead" contains "home" but not as a whole word — must not fire.
        assert_eq!(apply_corrections("homestead", &map), None);
    }

    #[test]
    fn corrections_word_boundary_match() {
        let map = corrections(&[("home", "HARM")]);
        // Input is case-insensitive (lower-cased before matching), so
        // "Home" matches the "home" key. Replacement string is kept
        // verbatim — the corrected text feeds back into route() which
        // normalises again, so the casing of the output doesn't matter
        // for behaviour but lets config authors keep tactical-style
        // capitalisation in the table.
        assert_eq!(apply_corrections("select Home", &map), Some("select HARM".to_string()));
    }

    #[test]
    fn corrections_longest_key_first() {
        // "jay dam" beats "jay" because longest match wins, otherwise
        // "jay" would partially fire on "jay dam" leaving "harm dam".
        let map = corrections(&[("jay", "harm"), ("jay dam", "jdam")]);
        assert_eq!(apply_corrections("go to jay dam page", &map), Some("go to jdam page".to_string()));
    }

    #[test]
    fn corrections_route_end_to_end() {
        // Issue #15 acceptance criterion: "home" → routed-via-correction →
        // produces a recognised action (or query, depending on context).
        // Use a recognisable command: "next" mis-heard as "nest".
        let map = corrections(&[("nest", "next")]);
        let corrected = apply_corrections("nest please", &map);
        assert_eq!(corrected.as_deref(), Some("next please"));
        // The corrected text routes via the substring table.
        assert_eq!(
            route_with_fuzzy(corrected.as_deref().unwrap(), 0.0),
            RoutedIntent::Action(Action::Next)
        );
    }

    #[test]
    fn fuzzy_disabled_when_threshold_zero() {
        // "nect" doesn't substring-match anything; with the fuzzy fallback
        // off (threshold 0.0) it stays Unmatched.
        assert_eq!(route_with_fuzzy("nect", 0.0), RoutedIntent::Unmatched);
    }

    #[test]
    fn fuzzy_recovers_misheard_action() {
        // "nect" / "nest" are jw-close to "next" (>0.85) and have no
        // substring match — exactly the misrecognition case fuzzy is
        // for. Threshold 0.85 = default.
        assert_eq!(
            route_with_fuzzy("nect", 0.85),
            RoutedIntent::Action(Action::Next)
        );
        assert_eq!(
            route_with_fuzzy("nest", 0.85),
            RoutedIntent::Action(Action::Next)
        );
    }

    #[test]
    fn fuzzy_below_threshold_stays_unmatched() {
        // "knight" is too phonetically far from any action phrase to
        // count as a misrecognition. JW("knight", "next") < 0.85.
        assert_eq!(route_with_fuzzy("knight", 0.85), RoutedIntent::Unmatched);
    }

    #[test]
    fn fuzzy_does_not_clobber_exact_match() {
        // "next page" has a substring hit (PageNext) AND a high-JW score
        // against multiple phrases. Exact match must win.
        assert_eq!(
            route_with_fuzzy("next page", 0.85),
            RoutedIntent::Action(Action::PageNext)
        );
    }
}
