//! Tab abstraction. Each tab is one logical content category (generated
//! checklists, raw image folder, DCS native kneeboards, …) backed by a
//! source-type-specific loader. Tabs lazy-load: pages are read from disk the
//! first time a tab is activated, then cached for the rest of the session.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use crate::config::{
    resolve_aircraft, resolve_dcs_kneeboard_dir, AppConfig, SourceConfig, TabConfig,
};

// Mirror of the Item/PageManifest types in main.rs. Kept in sync deliberately
// — main.rs owns the version used by the UI, but the loader needs to
// deserialize the same JSON shape so it lives here too.
#[derive(Debug, Deserialize, Clone)]
pub struct PageManifest {
    #[serde(default)]
    pub schema_version: String,
    pub title: String,
    pub image: String,
    pub image_size: [u32; 2],
    pub items: Vec<Item>,
}

#[derive(Debug, Deserialize, Clone)]
#[allow(dead_code)]
pub struct Item {
    pub idx: u32,
    #[serde(default)]
    pub group: String,
    pub kind: String,
    pub text: String,
    #[serde(default)]
    pub spoken: Option<String>,
    pub navigable: bool,
    pub bbox: [f32; 4],
}

pub struct LoadedPage {
    pub manifest: PageManifest,
    /// Disk path for the page PNG. Decoded on demand via `image()` so we
    /// don't eat 11 MB per page × 30 pages of RGBA at startup.
    pub image_path: PathBuf,
    image_cache: RefCell<Option<slint::Image>>,
}

impl LoadedPage {
    pub fn new(manifest: PageManifest, image_path: PathBuf) -> Self {
        Self {
            manifest,
            image_path,
            image_cache: RefCell::new(None),
        }
    }

    /// Decode the PNG the first time it's asked for; hand out clones of the
    /// reference-counted slint::Image afterwards. `Image::clone` is cheap.
    pub fn image(&self) -> slint::Image {
        if let Some(img) = self.image_cache.borrow().as_ref() {
            return img.clone();
        }
        let img = slint::Image::load_from_path(&self.image_path).unwrap_or_default();
        *self.image_cache.borrow_mut() = Some(img.clone());
        img
    }

    /// Drop the decoded image so its RGBA buffer is freed. Called by the
    /// page-eviction logic when the cursor leaves the page.
    pub fn evict_image(&self) {
        *self.image_cache.borrow_mut() = None;
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub page: usize,
    pub item: usize,
}

pub enum TabKind {
    /// Generator output — has per-item bboxes, full step nav + TTS.
    Generator,
    /// Raw images — each image is one "page" with a single page-level item.
    /// Item nav degrades to page nav; heading/Read are no-ops.
    ImageFolder,
}

pub struct Tab {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub kind: TabKind,
    /// Resolved disk path for the source. None until first load.
    pub source_path: Option<PathBuf>,
    pub source: SourceConfig,
    pub pages: Vec<LoadedPage>,
    pub cursor: Cursor,
    pub loaded: bool,
    pub load_error: Option<String>,
}

impl Tab {
    pub fn from_config(cfg: TabConfig) -> Self {
        let kind = match &cfg.source {
            SourceConfig::Generator { .. } => TabKind::Generator,
            SourceConfig::ImageFolder { .. } => TabKind::ImageFolder,
            SourceConfig::DcsKneeboards { .. } => TabKind::ImageFolder,
        };
        Self {
            id: cfg.id,
            label: cfg.label,
            icon: cfg.icon,
            kind,
            source_path: None,
            source: cfg.source,
            pages: Vec::new(),
            cursor: Cursor { page: 0, item: 0 },
            loaded: false,
            load_error: None,
        }
    }

    /// Load this tab's pages from disk. No-op if already loaded.
    /// `aircraft` is used to resolve `{aircraft}` placeholders and the DCS
    /// kneeboard path.
    pub fn ensure_loaded(&mut self, aircraft: &str) {
        if self.loaded {
            return;
        }
        self.loaded = true; // mark even on failure to avoid retry storms
        let path = match &self.source {
            SourceConfig::Generator { path } => Some(resolve_aircraft(path, aircraft)),
            SourceConfig::ImageFolder { path, .. } => Some(resolve_aircraft(path, aircraft)),
            SourceConfig::DcsKneeboards { base } => {
                resolve_dcs_kneeboard_dir(aircraft, base.as_deref())
            }
        };
        let Some(path) = path else {
            self.load_error = Some(format!(
                "DCS kneeboard folder not found for aircraft {aircraft}"
            ));
            eprintln!("[tab {}] {}", self.id, self.load_error.as_ref().unwrap());
            return;
        };
        self.source_path = Some(path.clone());

        let result = match &self.source {
            SourceConfig::Generator { .. } => load_generator_pages(&path),
            SourceConfig::ImageFolder { recursive, .. } => load_image_folder(&path, *recursive),
            SourceConfig::DcsKneeboards { .. } => load_image_folder(&path, false),
        };
        match result {
            Ok(pages) => {
                eprintln!(
                    "[tab {}] loaded {} pages from {}",
                    self.id,
                    pages.len(),
                    path.display()
                );
                self.pages = pages;
                self.cursor = Cursor {
                    page: 0,
                    item: first_navigable(&self.pages.first().map(|p| p.manifest.items.as_slice()).unwrap_or(&[])),
                };
            }
            Err(e) => {
                self.load_error = Some(format!("{e}"));
                eprintln!("[tab {}] load failed: {e:?}", self.id);
            }
        }
    }
}

pub fn first_navigable(items: &[Item]) -> usize {
    items.iter().position(|i| i.navigable).unwrap_or(0)
}

// --- Voice query resolution (issue #2) --------------------------------------

/// Destination produced by a voice query — addresses a specific item in a
/// specific page in a specific tab. The dispatcher uses it to switch tab +
/// page + cursor in one shot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavTarget {
    pub tab_idx: usize,
    pub page_idx: usize,
    pub item_idx: usize,
}

/// One result from a voice query lookup. `label` is the matched group or
/// item text — shown in the results panel and (later) read by TTS. `score`
/// is Jaro-Winkler similarity ∈ [0.0, 1.0] post-weighting. `alternates` is
/// populated only on the top result, and lists the next-best hits so the
/// caller can disambiguate.
#[derive(Debug, Clone)]
pub struct NavMatch {
    pub target: NavTarget,
    pub label: String,
    pub score: f32,
    pub alternates: Vec<NavMatch>,
}

/// Minimum similarity for an item to be considered a candidate. Started at
/// 0.6 per the issue plan; bumped to 0.7 after `kangaroo` vs `landing`
/// Jaro-Winkler'd to 0.60 in unit tests — too loose for short groups. STT
/// noise on real multi-token queries still scores 0.8+, so 0.7 keeps real
/// usage intact while filtering coincidental letter overlap.
const MATCH_THRESHOLD: f32 = 0.7;

/// Token-coverage bonus cap: when every word in the query also appears as a
/// token in the candidate, the score gets +TOKEN_BONUS_CAP. Designed to
/// break ties between sub-sections that share a long prefix — "AGM-65 IR"
/// vs "AGM-65 EMPLOYMENT" both score ~0.9 on Jaro-Winkler against an
/// AGM-65 header, but only one has the "ir" token, so the bonus separates
/// them. Tuning rationale: 0.20 is enough to overcome a ~0.05 JW gap from
/// a longer-but-irrelevant header, small enough not to swamp JW entirely.
const TOKEN_BONUS_CAP: f32 = 0.20;

/// Direct-navigate threshold: a single hit above this, with no close
/// alternates, dispatches without the results panel.
pub const CONFIDENT_THRESHOLD: f32 = 0.8;

/// Pure function over a tab's pages. Each `pages[i]` is the item slice for
/// one page. Returns the best match (with up to 4 alternates) if any item
/// scores ≥ MATCH_THRESHOLD; None otherwise.
///
/// Scoring strategy:
/// - Navigable items only — section headers (`navigable=false`) score, but
///   we land on the first navigable item in their group instead.
/// - First-occurrence-per-group-per-page is what gets scored; subsequent
///   items in the same group on the same page are dropped, so a section
///   with 8 steps doesn't crowd the alternates list.
/// - Score = jaro_winkler(query, item.group) when group is non-empty;
///   otherwise jaro_winkler(query, item.text) with a 0.85x weight to
///   demote unsourced fallbacks.
pub fn resolve_section_in_pages(
    query: &str,
    pages: &[&[Item]],
    tab_idx: usize,
) -> Option<NavMatch> {
    let q = normalize_match(query);
    if q.is_empty() {
        return None;
    }

    let mut candidates: Vec<(f32, NavTarget, String)> = Vec::new();
    for (page_idx, items) in pages.iter().enumerate() {
        let mut seen_groups: std::collections::HashSet<String> = std::collections::HashSet::new();
        for (item_idx, item) in items.iter().enumerate() {
            if !item.navigable {
                continue;
            }
            let (text_for_match, weight, label) = if !item.group.is_empty() {
                if !seen_groups.insert(item.group.to_lowercase()) {
                    continue;
                }
                (item.group.as_str(), 1.0_f32, item.group.clone())
            } else {
                (item.text.as_str(), 0.85_f32, item.text.clone())
            };
            // Both sides go through the same normaliser so letter↔digit
            // boundaries in headers like "AGM-65D/G IR PRE" tokenise the
            // same way the voice query already did.
            let cand_norm = normalize_match(text_for_match);
            let jw = strsim::jaro_winkler(&q, &cand_norm) as f32;
            let bonus = token_overlap_bonus(&q, &cand_norm);
            let score = (jw + bonus) * weight;
            if score >= MATCH_THRESHOLD {
                candidates.push((
                    score,
                    NavTarget {
                        tab_idx,
                        page_idx,
                        item_idx,
                    },
                    label,
                ));
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }
    // Highest score first.
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut iter = candidates.into_iter();
    let (top_score, top_target, top_label) = iter.next()?;
    let alternates: Vec<NavMatch> = iter
        .take(4)
        .map(|(s, t, l)| NavMatch {
            target: t,
            label: l,
            score: s,
            alternates: Vec::new(),
        })
        .collect();
    Some(NavMatch {
        target: top_target,
        label: top_label,
        score: top_score,
        alternates,
    })
}

/// Normalise text for matching: lowercase, replace non-alphanumeric runs
/// with a single space, and split at letter↔digit boundaries so
/// "AGM-65D/G" tokenises as ["agm", "65", "d", "g"] instead of one opaque
/// "agm-65d/g" blob. Idempotent on already-normalised inputs.
fn normalize_match(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alnum: Option<char> = None;
    for ch in s.chars() {
        if ch.is_ascii_alphanumeric() {
            let lc = ch.to_ascii_lowercase();
            if let Some(p) = prev_alnum {
                let boundary = (p.is_ascii_alphabetic() && lc.is_ascii_digit())
                    || (p.is_ascii_digit() && lc.is_ascii_alphabetic());
                if boundary {
                    out.push(' ');
                }
            }
            out.push(lc);
            prev_alnum = Some(lc);
        } else {
            if !out.is_empty() && !out.ends_with(' ') {
                out.push(' ');
            }
            prev_alnum = None;
        }
    }
    out.trim().to_string()
}

/// How many of the query's whitespace-separated tokens appear verbatim in
/// the candidate's tokens, scaled to TOKEN_BONUS_CAP. Order-independent —
/// "ir pre agm 65" against "agm 65 ir pre" gets the full bonus because
/// every query token is present, even though Jaro-Winkler would penalise
/// the position mismatch.
fn token_overlap_bonus(query: &str, candidate: &str) -> f32 {
    let q: std::collections::HashSet<&str> = query.split_whitespace().collect();
    if q.is_empty() {
        return 0.0;
    }
    let c: std::collections::HashSet<&str> = candidate.split_whitespace().collect();
    let common = q.iter().filter(|t| c.contains(*t)).count();
    let coverage = common as f32 / q.len() as f32;
    coverage * TOKEN_BONUS_CAP
}

#[cfg(test)]
mod resolver_tests {
    use super::*;

    fn step(idx: u32, group: &str, text: &str) -> Item {
        Item {
            idx,
            group: group.to_string(),
            kind: "step".to_string(),
            text: text.to_string(),
            spoken: None,
            navigable: true,
            bbox: [0.0, 0.0, 1.0, 1.0],
        }
    }

    fn header(idx: u32, text: &str) -> Item {
        Item {
            idx,
            group: String::new(),
            kind: "section-header".to_string(),
            text: text.to_string(),
            spoken: None,
            navigable: false,
            bbox: [0.0, 0.0, 1.0, 1.0],
        }
    }

    fn agm_fixture() -> Vec<Item> {
        vec![
            header(0, "AGM-65D/G IR PRE"),
            step(1, "AGM-65D/G IR PRE", "FCR switch | provides ranging ... ON"),
            step(2, "AGM-65D/G IR PRE", "MFD page ... WPN"),
            header(3, "AGM-65 EMPLOYMENT"),
            step(4, "AGM-65 EMPLOYMENT", "Master arm switch ... ARM"),
            step(5, "AGM-65 EMPLOYMENT", "Pickle ... AS REQUIRED"),
            header(6, "AGM-65 BORESIGHT"),
            step(7, "AGM-65 BORESIGHT", "TGP page ... BORE"),
        ]
    }

    /// Specific query → the right section wins, exact-match score, lands on
    /// the first navigable item in that section's group.
    #[test]
    fn picks_best_section_among_alternatives() {
        let items = agm_fixture();
        let pages: [&[Item]; 1] = [&items];

        let m = resolve_section_in_pages("agm 65 employment", &pages, 0)
            .expect("should match a section");
        assert_eq!(m.label, "AGM-65 EMPLOYMENT");
        // First navigable item in the matched group — idx 4 (Master arm).
        assert_eq!(m.target.item_idx, 4);
        // Case-insensitive exact match ⇒ score effectively 1.0.
        assert!(
            m.score > 0.95,
            "exact-match score should be near 1.0, got {}",
            m.score
        );
    }

    /// Issue #7: qualifying the AGM-65 query with a mode keyword should
    /// land on the matching sub-section, not the prefix-shared sibling.
    /// Before the token-overlap bonus, all three AGM-65 headers scored
    /// near-identically on "agm 65 ir" because Jaro-Winkler is
    /// character-level — the new tokeniser splits "65d/g" so "ir" can
    /// register as a present-or-absent token, and the bonus separates
    /// the candidates.
    #[test]
    fn weapon_mode_qualifier_picks_right_subsection() {
        let items = agm_fixture();
        let pages: [&[Item]; 1] = [&items];

        let m = resolve_section_in_pages("agm 65 ir", &pages, 0)
            .expect("AGM-65 IR query should match");
        assert_eq!(m.label, "AGM-65D/G IR PRE", "wanted IR PRE, got {}", m.label);

        let m = resolve_section_in_pages("agm 65 boresight", &pages, 0)
            .expect("AGM-65 boresight query should match");
        assert_eq!(
            m.label, "AGM-65 BORESIGHT",
            "wanted BORESIGHT, got {}",
            m.label
        );

        // Confirm the existing canonical case still resolves the same way.
        let m = resolve_section_in_pages("agm 65 employment", &pages, 0)
            .expect("AGM-65 employment query should match");
        assert_eq!(m.label, "AGM-65 EMPLOYMENT");
    }

    /// Token-coverage bonus is order-independent. STT sometimes inverts
    /// word order ("IR pre AGM-65") and the user shouldn't have to memorise
    /// the header's word order to navigate.
    #[test]
    fn token_order_independence() {
        let items = agm_fixture();
        let pages: [&[Item]; 1] = [&items];
        let m = resolve_section_in_pages("ir pre agm 65", &pages, 0)
            .expect("reversed-order query should still match");
        assert_eq!(m.label, "AGM-65D/G IR PRE");
    }

    /// The normaliser splits letter/digit runs so designator-style tokens
    /// don't get glued to model variants. Without this, "agm 65d g ir pre"
    /// wouldn't tokenise cleanly and the "65" token wouldn't match.
    #[test]
    fn normalize_match_splits_letter_digit_boundaries() {
        assert_eq!(normalize_match("AGM-65D/G IR PRE"), "agm 65 d g ir pre");
        assert_eq!(normalize_match("MK-82"), "mk 82");
        assert_eq!(normalize_match("AIM-120C"), "aim 120 c");
        // Idempotent on already-normalised input.
        assert_eq!(normalize_match("agm 65 ir pre"), "agm 65 ir pre");
        // Empty / whitespace input collapses to empty.
        assert_eq!(normalize_match(""), "");
        assert_eq!(normalize_match("   "), "");
        assert_eq!(normalize_match("---"), "");
    }

    /// Broad query that hits the common prefix of all three sections — the
    /// alternates list should surface the other two so the panel (phase 5)
    /// can disambiguate.
    #[test]
    fn broad_query_returns_multiple_alternates() {
        let items = agm_fixture();
        let pages: [&[Item]; 1] = [&items];

        let m = resolve_section_in_pages("agm 65", &pages, 0)
            .expect("should match");
        // At least one alternate — the broad query is intentionally ambiguous.
        // We don't pin the exact top-hit label because all three sections
        // share the "agm 65" prefix and any tie-break is fine.
        assert!(
            !m.alternates.is_empty(),
            "broad query should produce alternates, got top-only: {:?}",
            m.label
        );
    }

    /// Same group on multiple pages should still resolve cleanly — first
    /// occurrence wins (it's where the section "starts").
    #[test]
    fn dedupes_repeated_group_within_page() {
        // 5 steps in the same group; only the first should be scored, so
        // alternates aren't crowded with duplicates.
        let page = [
            header(0, "TAKEOFF"),
            step(1, "TAKEOFF", "step 1"),
            step(2, "TAKEOFF", "step 2"),
            step(3, "TAKEOFF", "step 3"),
            step(4, "TAKEOFF", "step 4"),
        ];
        let pages: [&[Item]; 1] = [&page];
        let m = resolve_section_in_pages("takeoff", &pages, 0).expect("should match");
        assert_eq!(m.target.item_idx, 1); // first navigable in group
        assert!(m.alternates.is_empty()); // no duplicates promoted
    }

    /// End-to-end: an alias rewrite turns a voice query the user actually
    /// says into the canonical form the section header uses, unlocking a
    /// match that wouldn't otherwise clear the 0.7 threshold.
    ///
    /// "maverick" vs "agm 65" scores ~0.44 in Jaro-Winkler (different
    /// first letter, almost no character overlap) — well below threshold.
    /// The alias "maverick" → "agm 65" rewrites the query, after which it
    /// exact-matches the section group at score 1.0.
    #[test]
    fn alias_rewrite_unlocks_canonical_section_match() {
        use crate::query_aliases::QueryAliases;
        let items = [
            header(0, "AGM-65 EMPLOYMENT"),
            step(1, "AGM-65 EMPLOYMENT", "Master arm switch ... ARM"),
            step(2, "AGM-65 EMPLOYMENT", "Pickle ... AS REQUIRED"),
        ];
        let pages: [&[Item]; 1] = [&items];

        // Without aliases: "maverick" can't reach the canonical section.
        assert!(
            resolve_section_in_pages("maverick", &pages, 0).is_none(),
            "raw \"maverick\" should not match \"AGM-65\" without alias rewrite"
        );

        // With aliases: same query rewrites to canonical, then matches.
        let aliases = QueryAliases {
            rewrites: [("maverick".to_string(), "agm 65".to_string())]
                .into_iter()
                .collect(),
        };
        let rewritten = aliases.rewrite("maverick");
        assert_eq!(rewritten, "agm 65");
        let m = resolve_section_in_pages(&rewritten, &pages, 0)
            .expect("rewritten query should match the canonical section");
        assert!(
            m.label.starts_with("AGM-65"),
            "expected an AGM-65 section, got {}",
            m.label
        );
    }

    /// Nothing similar enough → None, so the caller can fall through to
    /// "no match" UX instead of dispatching to a garbage target.
    #[test]
    fn rejects_unrelated_query() {
        let page = [
            header(0, "TAKEOFF"),
            step(1, "TAKEOFF", "throttle ... MIL"),
            header(2, "LANDING"),
            step(3, "LANDING", "gear ... DOWN"),
        ];
        let pages: [&[Item]; 1] = [&page];
        assert!(resolve_section_in_pages("kangaroo", &pages, 0).is_none());
        assert!(resolve_section_in_pages("", &pages, 0).is_none());
        assert!(resolve_section_in_pages("   ", &pages, 0).is_none());
    }
}

fn load_generator_pages(dir: &Path) -> Result<Vec<LoadedPage>> {
    let mut json_paths: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context(|| format!("reading dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map_or(false, |ext| ext == "json"))
        .collect();
    json_paths.sort();

    let mut pages = Vec::with_capacity(json_paths.len());
    for json_path in json_paths {
        let text = std::fs::read_to_string(&json_path)
            .with_context(|| format!("reading {}", json_path.display()))?;
        let manifest: PageManifest = serde_json::from_str(&text)
            .with_context(|| format!("parsing {}", json_path.display()))?;
        let png_path = dir.join(&manifest.image);
        if !png_path.exists() {
            anyhow::bail!("missing PNG {}", png_path.display());
        }
        pages.push(LoadedPage::new(manifest, png_path));
    }
    if pages.is_empty() {
        anyhow::bail!("no page JSONs in {}", dir.display());
    }
    Ok(pages)
}

/// Treat each image file in `dir` as one "page" with a single full-page
/// `image` item so the existing nav code Just Works. Item-level nav and TTS
/// degrade to page-level — there's no structure to walk.
fn load_image_folder(dir: &Path, recursive: bool) -> Result<Vec<LoadedPage>> {
    let mut image_paths = Vec::new();
    collect_images(dir, recursive, &mut image_paths)
        .with_context(|| format!("scanning {}", dir.display()))?;
    image_paths.sort();

    if image_paths.is_empty() {
        anyhow::bail!("no images in {}", dir.display());
    }

    let mut pages = Vec::with_capacity(image_paths.len());
    for img_path in image_paths.iter() {
        // Use the image crate's lightweight dimension reader so we don't
        // decode every PNG up front just to fill in bbox metadata.
        let (w, h) = image_dimensions(img_path).unwrap_or((1, 1));
        let title = img_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("page")
            .to_string();
        let manifest = PageManifest {
            schema_version: "image-folder".to_string(),
            title: title.clone(),
            image: img_path.file_name().and_then(|s| s.to_str()).unwrap_or("").to_string(),
            image_size: [w.max(1), h.max(1)],
            items: vec![Item {
                idx: 0,
                group: String::new(),
                kind: "image".to_string(),
                text: title,
                spoken: None,
                navigable: true,
                // Whole-page bbox — the highlight rect is suppressed in
                // apply_cursor for `kind == "image"` so this doesn't draw.
                bbox: [0.0, 0.0, w.max(1) as f32, h.max(1) as f32],
            }],
        };
        pages.push(LoadedPage::new(manifest, img_path.clone()));
    }
    Ok(pages)
}

/// Read just the dimensions of a PNG/JPEG without keeping the decoded
/// pixels around. We fall back to a momentary decode-and-drop for non-PNG
/// formats so a transient ~10 MB peak is the worst case per file.
fn image_dimensions(path: &Path) -> Option<(u32, u32)> {
    if let Some((w, h)) = read_png_dims(path) {
        return Some((w, h));
    }
    let img = slint::Image::load_from_path(path).ok()?;
    let sz = img.size();
    Some((sz.width.max(1), sz.height.max(1)))
}

fn read_png_dims(path: &Path) -> Option<(u32, u32)> {
    use std::io::Read;
    if !matches!(
        path.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("png")
    ) {
        return None;
    }
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = [0u8; 24];
    f.read_exact(&mut buf).ok()?;
    if &buf[0..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let w = u32::from_be_bytes(buf[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(buf[20..24].try_into().ok()?);
    Some((w, h))
}

fn collect_images(dir: &Path, recursive: bool, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir)?;
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() && recursive {
            collect_images(&p, true, out)?;
        } else if is_image_path(&p) {
            out.push(p);
        }
    }
    Ok(())
}

fn is_image_path(p: &Path) -> bool {
    matches!(
        p.extension().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase()).as_deref(),
        Some("png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif")
    )
}

/// Owning collection of tabs + the currently-active index. Switching tabs
/// triggers lazy-load of the destination.
pub struct TabRegistry {
    pub tabs: Vec<Tab>,
    pub active: usize,
    pub aircraft: String,
    pub aircraft_list: Vec<(String, String)>, // (id, label)
}

impl TabRegistry {
    pub fn new(cfg: &AppConfig, aircraft: String) -> Self {
        let tabs: Vec<Tab> = cfg.tabs.iter().cloned().map(Tab::from_config).collect();
        let aircraft_list = if cfg.aircraft.is_empty() {
            vec![(aircraft.clone(), aircraft.clone())]
        } else {
            cfg.aircraft
                .iter()
                .map(|a| (a.id.clone(), if a.label.is_empty() { a.id.clone() } else { a.label.clone() }))
                .collect()
        };
        Self {
            tabs,
            active: 0,
            aircraft,
            aircraft_list,
        }
    }

    pub fn active_tab(&self) -> Option<&Tab> {
        self.tabs.get(self.active)
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut Tab> {
        self.tabs.get_mut(self.active)
    }

    pub fn set_active_by_id(&mut self, id: &str) {
        if let Some(idx) = self.tabs.iter().position(|t| t.id == id) {
            self.active = idx;
            let aircraft = self.aircraft.clone();
            self.tabs[idx].ensure_loaded(&aircraft);
        }
    }

    pub fn set_active(&mut self, idx: usize) {
        if idx < self.tabs.len() {
            self.active = idx;
            let aircraft = self.aircraft.clone();
            self.tabs[idx].ensure_loaded(&aircraft);
        }
    }

    /// Force-reload the active tab from disk, preserving the cursor where
    /// possible. Called by the watcher when files in the source dir change.
    /// Returns true if the tab had a source to reload.
    pub fn reload_active(&mut self) -> bool {
        let aircraft = self.aircraft.clone();
        let Some(tab) = self.tabs.get_mut(self.active) else {
            return false;
        };
        // Snapshot the cursor identity before we drop the old pages so we
        // can try to find the same item in the freshly-loaded set.
        let prev_cursor = tab.cursor;
        let prev_page_title = tab
            .pages
            .get(prev_cursor.page)
            .map(|p| p.manifest.title.clone());
        let prev_item_idx_in_manifest = tab
            .pages
            .get(prev_cursor.page)
            .and_then(|p| p.manifest.items.get(prev_cursor.item).map(|i| i.idx));

        tab.loaded = false;
        tab.pages.clear();
        tab.source_path = None;
        tab.load_error = None;
        tab.ensure_loaded(&aircraft);

        // Try to restore cursor: same page title + same item.idx wins; else
        // fall back to first navigable of page 0.
        if let Some(title) = prev_page_title {
            if let Some((page_idx, page)) = tab
                .pages
                .iter()
                .enumerate()
                .find(|(_, p)| p.manifest.title == title)
            {
                let item_idx = prev_item_idx_in_manifest
                    .and_then(|target_idx| {
                        page.manifest
                            .items
                            .iter()
                            .position(|i| i.idx == target_idx)
                    })
                    .unwrap_or_else(|| first_navigable(&page.manifest.items));
                tab.cursor = Cursor {
                    page: page_idx,
                    item: item_idx,
                };
            }
        }
        true
    }

    /// Search the active tab for items whose group (or text, as a fallback)
    /// fuzzy-matches `query`. Returns the top hit with up to 4 alternates,
    /// or None if no item scored above the threshold.
    pub fn resolve_section_query(&self, query: &str) -> Option<NavMatch> {
        let tab = self.active_tab()?;
        let pages: Vec<&[Item]> = tab
            .pages
            .iter()
            .map(|p| p.manifest.items.as_slice())
            .collect();
        resolve_section_in_pages(query, &pages, self.active)
    }

    /// Fuzzy-match a query against every tab's label + id. No page data
    /// needed, so this works even for tabs that haven't lazy-loaded yet.
    /// Returns a NavTarget pointing at the matched tab's (page 0, item 0);
    /// the caller's switch_tab handles the load.
    pub fn resolve_tab_query(&self, query: &str) -> Option<NavMatch> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return None;
        }
        let mut candidates: Vec<(f32, NavTarget, String)> = Vec::new();
        for (tab_idx, tab) in self.tabs.iter().enumerate() {
            // Score against the label, then the id; keep whichever's higher.
            let s_label = strsim::jaro_winkler(&q, &tab.label.to_lowercase()) as f32;
            let s_id = strsim::jaro_winkler(&q, &tab.id.to_lowercase()) as f32;
            let score = s_label.max(s_id);
            if score >= MATCH_THRESHOLD {
                candidates.push((
                    score,
                    NavTarget {
                        tab_idx,
                        page_idx: 0,
                        item_idx: 0,
                    },
                    tab.label.clone(),
                ));
            }
        }
        candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut iter = candidates.into_iter();
        let (top_score, top_target, top_label) = iter.next()?;
        let alternates: Vec<NavMatch> = iter
            .take(4)
            .map(|(s, t, l)| NavMatch {
                target: t,
                label: l,
                score: s,
                alternates: Vec::new(),
            })
            .collect();
        Some(NavMatch {
            target: top_target,
            label: top_label,
            score: top_score,
            alternates,
        })
    }

    pub fn set_aircraft(&mut self, aircraft: String) {
        if self.aircraft == aircraft {
            return;
        }
        self.aircraft = aircraft.clone();
        // Reset all tabs so they reload on next activation (different aircraft
        // means different content in {aircraft}-relative paths).
        for tab in &mut self.tabs {
            tab.loaded = false;
            tab.pages.clear();
            tab.cursor = Cursor { page: 0, item: 0 };
            tab.source_path = None;
            tab.load_error = None;
        }
        // Eagerly load the currently-active tab so the UI updates immediately.
        if let Some(tab) = self.tabs.get_mut(self.active) {
            tab.ensure_loaded(&aircraft);
        }
    }
}
