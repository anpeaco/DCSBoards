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
