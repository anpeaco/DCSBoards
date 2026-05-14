//! Phonetic query aliases — rewrite STT transcripts into the canonical
//! terminology used in checklist content, *before* fuzzy matching.
//!
//! This is the inverse of `pronunciation.toml` (which goes canonical →
//! spoken). The user says "mark 82"; the sidecar JSON section header is
//! "MK-82". Jaro-Winkler can't bridge that gap reliably because the
//! lengths differ and the common prefix is one character. A pre-pass
//! rewrite of `mark → mk` makes the rest of the resolver Just Work.
//!
//! Issue: #8.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct QueryAliases {
    #[serde(default)]
    pub rewrites: HashMap<String, String>,
}

impl QueryAliases {
    pub fn load_or_default(path: &Path) -> Self {
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(cfg) => {
                    eprintln!(
                        "[aliases] loaded {} query rewrites from {}",
                        cfg.rewrites.len(),
                        path.display()
                    );
                    cfg
                }
                Err(e) => {
                    eprintln!("[aliases] {} parse failed: {e}", path.display());
                    Self::default()
                }
            },
            Err(_) => {
                eprintln!(
                    "[aliases] no {} found; no query rewrites",
                    path.display()
                );
                Self::default()
            }
        }
    }

    /// Rewrite a voice-query string using the alias table.
    ///
    /// Input is expected to already be normalised by `voice_router::normalise`
    /// (lowercase ASCII alphanumerics separated by single spaces, no
    /// punctuation). Rewrite keys are lowercased on load; word-boundary
    /// substring replacement avoids "mark" → "mk" eating "remark".
    /// Longest-first key ordering means `"mark 82" → "mk 82"` beats a more
    /// generic `"mark" → "mk"` when both are defined.
    pub fn rewrite(&self, query: &str) -> String {
        if self.rewrites.is_empty() {
            return query.to_string();
        }
        let mut keys: Vec<(&String, &String)> = self.rewrites.iter().collect();
        // Longest key first so "mark 82" → "mk 82" beats "mark" → "mk".
        keys.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        let mut out = query.to_string();
        for (key, value) in keys {
            if key.is_empty() {
                continue;
            }
            out = replace_word(&out, &key.to_lowercase(), value);
        }
        out
    }
}

/// Replace every word-boundary occurrence of `needle` (already lowercase)
/// with `replacement` in `haystack` (already lowercase). Word boundary
/// means: start-of-string or preceded by ASCII whitespace, AND end-of-
/// string or followed by ASCII whitespace.
fn replace_word(haystack: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() || haystack.is_empty() {
        return haystack.to_string();
    }
    let bytes = haystack.as_bytes();
    let n_bytes = needle.as_bytes();
    let mut out = String::with_capacity(haystack.len());
    let mut last = 0usize;
    let mut from = 0usize;
    while from <= bytes.len().saturating_sub(n_bytes.len()) {
        let Some(pos) = find_substr(&bytes[from..], n_bytes) else {
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

fn find_substr(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aliases(pairs: &[(&str, &str)]) -> QueryAliases {
        QueryAliases {
            rewrites: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn rewrites_basic_alias() {
        let a = aliases(&[("mark", "mk")]);
        assert_eq!(a.rewrite("mark 82"), "mk 82");
        assert_eq!(a.rewrite("mark 82 employment"), "mk 82 employment");
    }

    #[test]
    fn respects_word_boundaries() {
        let a = aliases(&[("mark", "mk")]);
        // "remark" must not become "remk".
        assert_eq!(a.rewrite("remark mode"), "remark mode");
        // "markup" must not become "mkup".
        assert_eq!(a.rewrite("markup language"), "markup language");
        // Standalone "mark" still rewrites.
        assert_eq!(a.rewrite("the mark of zorro"), "the mk of zorro");
    }

    #[test]
    fn longest_key_wins() {
        // If both "mark" → "mk" and "mark 82" → "mk 82" are defined, the
        // longer key matches first so the shorter doesn't pre-empt it.
        // (Result is the same string in this case, but the count of
        // operations differs; this is here as a regression guard.)
        let a = aliases(&[("mark", "mk"), ("mark 82", "mk 82")]);
        assert_eq!(a.rewrite("mark 82"), "mk 82");
    }

    #[test]
    fn multiple_aliases() {
        let a = aliases(&[
            ("mark", "mk"),
            ("agem", "agm"),
            ("jay damn", "jdam"),
        ]);
        assert_eq!(
            a.rewrite("how do i employ the agem 65 with mark 82"),
            "how do i employ the agm 65 with mk 82"
        );
        assert_eq!(a.rewrite("jay damn release"), "jdam release");
    }

    #[test]
    fn empty_aliases_passthrough() {
        let a = QueryAliases::default();
        assert_eq!(a.rewrite("anything goes here"), "anything goes here");
    }

    #[test]
    fn empty_key_ignored() {
        let a = aliases(&[("", "should not apply"), ("mark", "mk")]);
        assert_eq!(a.rewrite("mark 82"), "mk 82");
    }
}
