//! Tab abstraction. Each tab is one logical content category (generated
//! checklists, raw image folder, DCS native kneeboards, …) backed by a
//! source-type-specific loader. Tabs lazy-load: pages are read from disk the
//! first time a tab is activated, then cached for the rest of the session.

use anyhow::{Context, Result};
use serde::Deserialize;
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
    pub image: slint::Image,
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
        let image = slint::Image::load_from_path(&png_path)
            .map_err(|_| anyhow::anyhow!("loading {}", png_path.display()))?;
        pages.push(LoadedPage { manifest, image });
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
    for (idx, img_path) in image_paths.iter().enumerate() {
        let image = slint::Image::load_from_path(img_path)
            .map_err(|_| anyhow::anyhow!("loading {}", img_path.display()))?;
        let (w, h) = (image.size().width, image.size().height);
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
        pages.push(LoadedPage { manifest, image });
        let _ = idx;
    }
    Ok(pages)
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
