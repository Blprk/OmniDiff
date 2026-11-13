//! OmniDiff Pro 2.0 — the Rune UI.
//!
//! State lives in `OmniApp`; the compare/sync engine runs on a worker thread
//! and reports back through `AppHandle::update`, which wakes the render loop.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender, channel};

use rune::{App, AppHandle, AppSpec, Color, Cursor, Cx, El, Icon, Image, ImageCache, KeyEvent, Tx, col, el, row, text};
use rune_kit::diff::{DiffMode, DiffState, FileDiff, DiffView};
use rune_kit::{
    Button, ButtonVariant, Cell, Checkbox, Dialog, IconButton, IconButtonVariant, Kit,
    Progress, SegmentedControl, Table, TableColumn, TableState, TextInput,
};

use crate::insights::{self, Insights};
use crate::scanner::{self, CompareResult, FileEntry, ScanStatus};
use crate::settings::{self, Settings, Theme};

/// The Rune rev this app is developed against. Keep in sync with Cargo.toml.
const RUNE_REV: &str = "3aea1e4ae699b2a1079359ea000c8d3d1adb488e";

/// "Reveal in Finder" glyph: a macOS-style folder outline.
static FINDER: Icon = Icon {
    name: "omnidiff-finder",
    view: 16.0,
    body: r#"<path d="M2 4.75C2 3.784 2.784 3 3.75 3h2.882c.3 0 .586.119.797.33L8.5 4.5h3.75c.966 0 1.75.784 1.75 1.75v4.5c0 .966-.784 1.75-1.75 1.75H3.75A1.75 1.75 0 0 1 2 10.75v-6Z" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round"/>"#,
};

/// Settings glyph: adjustment sliders.
static SETTINGS: Icon = Icon {
    name: "omnidiff-settings",
    view: 16.0,
    body: r#"<path d="M2.5 4.5h11M2.5 8h11M2.5 11.5h11" stroke="currentColor" stroke-width="1.2" stroke-linecap="round"/><circle cx="6" cy="4.5" r="1.5" fill="currentColor"/><circle cx="10.5" cy="8" r="1.5" fill="currentColor"/><circle cx="5" cy="11.5" r="1.5" fill="currentColor"/>"#,
};

// ---------------------------------------------------------------------------
// Worker plumbing: one persistent thread consumes jobs and reports via handle.
// ---------------------------------------------------------------------------

enum Job {
    Compare {
        source: PathBuf,
        dest: PathBuf,
        check_content: bool,
    },
    Sync {
        dest: PathBuf,
        results: CompareResult,
        delete_extra: bool,
        preserve_mtime: bool,
    },
    DeepCheck {
        source: PathBuf,
        dest: PathBuf,
    },
}

static HANDLE: OnceLock<AppHandle<OmniApp>> = OnceLock::new();
static JOBS: OnceLock<Sender<Job>> = OnceLock::new();
static JOBS_RX: OnceLock<std::sync::Mutex<Receiver<Job>>> = OnceLock::new();

fn send_job(job: Job) {
    let tx = JOBS.get_or_init(|| {
        let (tx, rx) = channel();
        let _ = JOBS_RX.set(std::sync::Mutex::new(rx));
        tx
    });
    let _ = tx.send(job);
}

fn spawn_worker() {
    std::thread::spawn(|| {
        let Some(rx) = JOBS_RX.get() else { return; };
        loop {
            let Ok(job) = rx.lock().unwrap().recv() else { break };
            let Some(handle) = HANDLE.get() else { continue; };
            let handle = handle.clone();
            let report = move |status: ScanStatus| {
                handle.update(move |s: &mut OmniApp, _| s.apply_status(status.clone()));
            };
            match job {
                Job::Compare { source, dest, check_content } => {
                    match scanner::run_comparison(source, dest, check_content, report) {
                        Ok(results) => {
                            let handle = HANDLE.get().unwrap().clone();
                            handle.update(move |s: &mut OmniApp, _| s.apply_results(results));
                        }
                        Err(e) => {
                            let handle = HANDLE.get().unwrap().clone();
                            handle.update(move |s: &mut OmniApp, _| {
                                s.apply_status(ScanStatus::Error(e));
                            });
                        }
                    }
                }
                Job::Sync { dest, results, delete_extra, preserve_mtime } => {
                    let _ = scanner::run_sync(PathBuf::new(), dest, &results, delete_extra, preserve_mtime, report);
                }
                Job::DeepCheck { source, dest } => {
                    let handle = HANDLE.get().unwrap().clone();
                    match insights::deep_check(source, dest, report) {
                        Ok(check) => handle.update(move |s: &mut OmniApp, _| s.apply_deep(check)),
                        Err(e) => handle.update(move |s: &mut OmniApp, _| {
                            s.phase = Phase::Idle;
                            s.status = format!("❌ {e}");
                            s.status_error = true;
                        }),
                    }
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    MissingInDest,
    MissingInSource,
    Different,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    Idle,
    Scanning,
    Hashing,
    Syncing,
}

/// What the worker is currently doing, for progress text.
#[derive(Clone, Copy, PartialEq)]
enum JobKind {
    Compare,
    Sync,
    DeepCheck,
}

/// A table filter picked by clicking the insights panel.
#[derive(Clone, PartialEq)]
enum Filter {
    Folder(String),
    Type(String),
}

impl Filter {
    fn label(&self) -> &str {
        match self {
            Filter::Folder(d) => d,
            Filter::Type(t) => t,
        }
    }
}

/// The two top-level screens: the compare dashboard and the full-window diff viewer.
#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Main,
    Diff,
}

/// One visible table row: either a folder group header or a file row.
#[derive(Clone, Debug)]
enum DisplayRow {
    Folder(String, usize), // folder name, involved file count
    File(usize),           // index into rows/pairs for the active tab
}

pub struct OmniApp {
    source: rune::TextEdit,
    dest: rune::TextEdit,
    delete_extra: bool,

    phase: Phase,
    progress: f32,
    status: String,
    status_error: bool,
    /// What the worker is running, since when, and the first hashing sample
    /// (count, time) used to estimate the rate for the ETA.
    job: Option<JobKind>,
    job_start: Option<std::time::Instant>,
    hashing_sample: Option<(usize, std::time::Instant)>,

    results: Option<CompareResult>,
    insights: Option<Insights>,
    show_insights: bool,
    /// Table filter picked by clicking the insights panel.
    filter: Option<Filter>,
    tab: Tab,
    table: TableState,
    /// Rows backing the visible table, rebuilt whenever results or the tab change.
    rows: Vec<FileEntry>,
    pairs: Vec<(FileEntry, FileEntry)>,
    /// Grouped display model: folder headers interleaved with file rows.
    grouped: bool,
    collapsed: std::collections::BTreeSet<String>,
    display: Vec<DisplayRow>,

    /// Backup-safety deep check (full-content hashing of both sides).
    deep: Option<insights::DeepCheck>,

    screen: Screen,
    diff: Option<DiffState>,
    diff_is_image: bool,
    diff_title: String,
    img_src: Option<Image>,
    img_dest: Option<Image>,
    image_error: Option<String>,
    images: Option<ImageCache>,

    confirm_sync: bool,
    show_settings: bool,
    settings: Settings,
}

impl OmniApp {
    fn new() -> Self {
        let settings = settings::load(&settings::settings_path());
        let last_source = if settings.remember_folders && !settings.last_source.is_empty() {
            settings.last_source.clone()
        } else {
            String::new()
        };
        let last_dest = if settings.remember_folders && !settings.last_dest.is_empty() {
            settings.last_dest.clone()
        } else {
            String::new()
        };
        OmniApp {
            source: rune::TextEdit::new(last_source),
            dest: rune::TextEdit::new(last_dest),
            delete_extra: false,
            phase: Phase::Idle,
            progress: 0.0,
            status: "Ready".to_owned(),
            status_error: false,
            job: None,
            job_start: None,
            hashing_sample: None,
            results: None,
            insights: None,
            show_insights: false,
            filter: None,
            tab: Tab::MissingInDest,
            table: TableState::new(),
            rows: Vec::new(),
            pairs: Vec::new(),
            grouped: true,
            collapsed: Default::default(),
            display: Vec::new(),
            deep: None,
            screen: Screen::Main,
            diff: None,
            diff_is_image: false,
            diff_title: String::new(),
            img_src: None,
            img_dest: None,
            image_error: None,
            images: None,
            confirm_sync: false,
            show_settings: false,
            settings,
        }
    }

    /// Save settings to disk. Skipped in headless test runs, so scripted
    /// scenes never touch the user's real settings file.
    fn persist_settings(&self) {
        if rune::tooling::headless_run().is_some() {
            return;
        }
        let _ = settings::save(&settings::settings_path(), &self.settings);
    }

    // -- worker -> UI -------------------------------------------------------

    fn apply_status(&mut self, status: ScanStatus) {
        match status {
            ScanStatus::ScanningSource | ScanStatus::ScanningDest | ScanStatus::ScanningBoth => {
                self.phase = Phase::Scanning;
                self.progress = 0.05;
                self.status = match self.job {
                    Some(JobKind::DeepCheck) => "🛡 Scanning both backup folders...".into(),
                    Some(JobKind::Sync) => "♻️ Preparing sync...".into(),
                    _ => "📂 Scanning folders...".into(),
                };
                self.status_error = false;
            }
            ScanStatus::Hashing(current, total) => {
                self.phase = Phase::Hashing;
                let frac = current as f64 / total.max(1) as f64;
                self.progress = frac as f32;

                // ETA from the observed hashing rate once we have a >= 1s
                // sample window; before that, fall back to elapsed/frac.
                let now = std::time::Instant::now();
                let sample = self.hashing_sample.get_or_insert((current, now));
                let window = now.duration_since(sample.1).as_secs_f64();
                let rate = if window >= 1.0 { (current - sample.0) as f64 / window } else { 0.0 };
                let elapsed = self
                    .job_start
                    .map(|t| now.duration_since(t).as_secs_f64())
                    .unwrap_or(0.0);
                let eta = if rate > 0.0 {
                    Some(((total - current) as f64 / rate) as u64)
                } else if frac > 0.02 {
                    Some((elapsed / frac * (1.0 - frac)) as u64)
                } else {
                    None
                };

                let prefix = match self.job {
                    Some(JobKind::DeepCheck) => "🛡 Hashing content",
                    Some(JobKind::Sync) => "♻️ Syncing",
                    _ => "⚡ Verifying content (Blake3)",
                };
                self.status = match eta {
                    Some(eta) => format!(
                        "{prefix} — {current}/{total} · {:.0}% · {} elapsed · ETA ~{}",
                        frac * 100.0,
                        fmt_secs(elapsed as u64),
                        fmt_secs(eta)
                    ),
                    None => format!("{prefix} — {current}/{total} · {} elapsed", fmt_secs(elapsed as u64)),
                };
            }
            ScanStatus::Syncing(current, total) => {
                self.phase = Phase::Syncing;
                let frac = current as f64 / total.max(1) as f64;
                self.progress = frac as f32;
                let now = std::time::Instant::now();
                let sample = self.hashing_sample.get_or_insert((current, now));
                let window = now.duration_since(sample.1).as_secs_f64();
                let rate = if window >= 1.0 { (current - sample.0) as f64 / window } else { 0.0 };
                let elapsed = self
                    .job_start
                    .map(|t| now.duration_since(t).as_secs_f64())
                    .unwrap_or(0.0);
                let eta = if rate > 0.0 {
                    Some(((total - current) as f64 / rate) as u64)
                } else if frac > 0.02 {
                    Some((elapsed / frac * (1.0 - frac)) as u64)
                } else {
                    None
                };
                self.status = match eta {
                    Some(eta) => format!(
                        "♻️ Syncing — {current}/{total} operations · {:.0}% · {} elapsed · ETA ~{}",
                        frac * 100.0,
                        fmt_secs(elapsed as u64),
                        fmt_secs(eta)
                    ),
                    None => format!("♻️ Syncing — {current}/{total} operations · {} elapsed", fmt_secs(elapsed as u64)),
                };
            }
            ScanStatus::Complete => {
                if self.phase == Phase::Syncing {
                    self.phase = Phase::Idle;
                    self.status = "✅ Sync complete — run the comparison again to verify".into();
                    // Results are stale after a sync; drop them so the user
                    // cannot re-sync outdated data.
                    self.results = None;
                    self.insights = None;
                    self.deep = None;
                    self.filter = None;
                    self.rebuild_rows();
                }
                self.progress = 1.0;
                self.job = None;
                self.job_start = None;
                self.hashing_sample = None;
            }
            ScanStatus::Error(e) => {
                self.phase = Phase::Idle;
                self.status = format!("❌ {e}");
                self.status_error = true;
                self.job = None;
                self.job_start = None;
                self.hashing_sample = None;
            }
        }
    }

    fn apply_results(&mut self, results: CompareResult) {
        self.phase = Phase::Idle;
        self.status = "✅ Comparison complete".into();
        self.progress = 1.0;
        self.status_error = false;
        self.job = None;
        self.job_start = None;
        self.hashing_sample = None;
        self.results = Some(results);
        self.insights = self.results.as_ref().map(Insights::from_result);
        self.filter = None;
        self.show_insights = self.settings.auto_insights;
        self.tab = Tab::MissingInDest;
        self.table = TableState::new();
        self.deep = None;
        self.rebuild_rows();
    }

    fn apply_deep(&mut self, check: insights::DeepCheck) {
        self.phase = Phase::Idle;
        self.progress = 1.0;
        self.job = None;
        self.job_start = None;
        self.hashing_sample = None;
        self.show_insights = true;
        if check.doomed_unique.is_empty() {
            self.status = "🛡 Backup check complete — no unique content in the destination would be lost".into();
        } else {
            self.status = format!(
                "🛡 Backup check complete — ⚠️ {} file(s) exist only in the destination",
                check.doomed_unique.len()
            );
            self.status_error = true;
        }
        self.deep = Some(check);
    }

    /// Rebuild the flat rows the table reads, for the active tab.
    fn rebuild_rows(&mut self) {
        match (&self.results, self.tab) {
            (Some(r), Tab::MissingInDest) => self.rows = r.missing_in_dest.clone(),
            (Some(r), Tab::MissingInSource) => self.rows = r.missing_in_source.clone(),
            (Some(r), Tab::Different) => self.pairs = r.different_content.clone(),
            (None, _) => {
                self.rows.clear();
                self.pairs.clear();
            }
        }
        self.rebuild_display();
    }

    /// Rebuild the grouped display model from rows/pairs.
    fn rebuild_display(&mut self) {
        self.display.clear();
        let n = match self.tab {
            Tab::Different => self.pairs.len(),
            _ => self.rows.len(),
        };

        // Insights-panel filter, applied before grouping.
        let keep = |st: &OmniApp, i: usize| match &st.filter {
            None => true,
            Some(Filter::Folder(dir)) => {
                let rel = st.rel_of(i);
                rel.starts_with(&format!("{dir}/"))
            }
            Some(Filter::Type(label)) => insights::classify(st.rel_of(i)).0 == *label,
        };

        let indices: Vec<usize> = (0..n).filter(|i| keep(self, *i)).collect();

        if !self.grouped || indices.is_empty() {
            for i in indices {
                self.display.push(DisplayRow::File(i));
            }
            if self.display.is_empty() {
                self.display.push(DisplayRow::Folder("(no matches)".into(), 0));
            }
            return;
        }

        // Group by parent folder, folders sorted, files sorted within each.
        let mut groups: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
        for i in indices {
            let rel = self.rel_of(i).to_owned();
            let dir = rel.split_once('/').map(|(d, _)| d).unwrap_or("(root)").to_owned();
            groups.entry(dir).or_default().push(i);
        }
        for (dir, mut indices) in groups {
            indices.sort_by(|a, b| {
                let ra = self.rel_of(*a);
                let rb = self.rel_of(*b);
                ra.rsplit('/').next().unwrap_or("").cmp(rb.rsplit('/').next().unwrap_or(""))
            });
            self.display.push(DisplayRow::Folder(dir.clone(), indices.len()));
            if self.collapsed.contains(&dir) {
                continue;
            }
            for i in indices {
                self.display.push(DisplayRow::File(i));
            }
        }
    }

    /// Set or clear an insights-panel filter (clicking the active one clears it).
    fn toggle_filter(&mut self, f: Filter) {
        if self.filter.as_ref() == Some(&f) {
            self.filter = None;
        } else {
            self.filter = Some(f);
        }
        self.rebuild_display();
    }

    /// Relative path of visible row `i` for the active tab.
    fn rel_of(&self, i: usize) -> &str {
        match self.tab {
            Tab::Different => self.pairs.get(i).map(|p| p.0.rel_path.as_str()).unwrap_or(""),
            _ => self.rows.get(i).map(|f| f.rel_path.as_str()).unwrap_or(""),
        }
    }

    fn toggle_folder(&mut self, dir: &str) {
        if !self.collapsed.remove(dir) {
            let _ = self.collapsed.insert(dir.to_owned());
        }
        self.rebuild_display();
    }

    // -- actions ------------------------------------------------------------

    fn start_comparison(&mut self) {
        let source = PathBuf::from(self.source.text.trim());
        let dest = PathBuf::from(self.dest.text.trim());

        if !source.is_dir() || !dest.is_dir() {
            self.status = "❌ Both paths must exist and be folders".into();
            self.status_error = true;
            return;
        }
        if same_path(&source, &dest) {
            self.status = "❌ Source and Destination are the same folder".into();
            self.status_error = true;
            return;
        }
        if source.starts_with(&dest) || dest.starts_with(&source) {
            self.status = "❌ One folder is inside the other — pick two independent folders".into();
            self.status_error = true;
            return;
        }

        self.phase = Phase::Scanning;
        self.progress = 0.0;
        self.results = None;
        self.insights = None;
        self.deep = None;
        self.filter = None;
        self.rebuild_rows();
        self.status = "Starting...".into();
        self.status_error = false;
        self.job = Some(JobKind::Compare);
        self.job_start = Some(std::time::Instant::now());
        self.hashing_sample = None;
        send_job(Job::Compare { source: source.clone(), dest: dest.clone(), check_content: !self.settings.fast_compare });

        // Remember the folders for next launch, if the user opted in.
        if self.settings.remember_folders {
            self.settings.last_source = source.to_string_lossy().to_string();
            self.settings.last_dest = dest.to_string_lossy().to_string();
            self.persist_settings();
        }
    }

    fn start_sync(&mut self) {
        let Some(results) = self.results.clone() else { return };
        let dest = PathBuf::from(self.dest.text.trim());
        self.phase = Phase::Syncing;
        self.progress = 0.0;
        self.status = "♻️ Starting sync...".into();
        self.status_error = false;
        self.job = Some(JobKind::Sync);
        self.job_start = Some(std::time::Instant::now());
        self.hashing_sample = None;
        send_job(Job::Sync { dest, results, delete_extra: self.delete_extra, preserve_mtime: self.settings.preserve_mtime });
    }

    fn request_sync(&mut self) {
        if self.delete_extra {
            self.confirm_sync = true;
        } else {
            self.start_sync();
        }
    }

    fn start_deep_check(&mut self) {
        let source = PathBuf::from(self.source.text.trim());
        let dest = PathBuf::from(self.dest.text.trim());
        if !source.is_dir() || !dest.is_dir() {
            self.status = "❌ Folders must exist to run the backup check".into();
            self.status_error = true;
            return;
        }
        self.phase = Phase::Scanning;
        self.progress = 0.0;
        self.status = "🛡 Hashing every file on both sides...".into();
        self.status_error = false;
        self.job = Some(JobKind::DeepCheck);
        self.job_start = Some(std::time::Instant::now());
        self.hashing_sample = None;
        send_job(Job::DeepCheck { source, dest });
    }

    /// Reveal a file (or folder) in macOS Finder with it selected.
    fn reveal_in_finder(&mut self, path: &Path) {
        if !path.exists() {
            self.status = format!("❌ Not found on disk: {}", path.display());
            self.status_error = true;
            return;
        }
        match std::process::Command::new("open").arg("-R").arg(path).spawn() {
            Ok(_) => {
                self.status = format!("👁 Revealed in Finder: {}", path.display());
                self.status_error = false;
            }
            Err(e) => {
                self.status = format!("❌ Could not open Finder: {e}");
                self.status_error = true;
            }
        }
    }

    fn open_diff(&mut self, row_id: &str) {
        let Some(results) = &self.results else { return };
        let Some((src, dest)) = results.different_content.iter().find(|(s, _)| s.rel_path == row_id)
        else { return };

        self.diff_title = src.rel_path.clone();
        self.image_error = None;
        self.img_src = None;
        self.img_dest = None;

        let name = src.rel_path.to_lowercase();
        let image_exts = ["png", "jpg", "jpeg", "bmp", "gif", "webp", "ico", "tiff"];
        let is_image = image_exts.iter().any(|e| name.rsplit('.').next() == Some(e));
        self.diff_is_image = is_image;

        if is_image {
            let Some(cache) = self.images.as_ref() else {
                self.image_error = Some("Image loading is not ready yet.".into());
                self.screen = Screen::Diff;
                return;
            };
            self.img_src = Some(cache.load(std::fs::read(&src.path).unwrap_or_default()));
            self.img_dest = Some(cache.load(std::fs::read(&dest.path).unwrap_or_default()));
            self.screen = Screen::Diff;
            return;
        }

        if name.ends_with(".ds_store") || name.ends_with("thumbs.db") {
            self.image_error = Some("System file (binary). Comparison skipped.".into());
            self.screen = Screen::Diff;
            return;
        }

        let (Ok(src_txt), Ok(dest_txt)) = (std::fs::read_to_string(&src.path), std::fs::read_to_string(&dest.path)) else {
            self.image_error = Some("Binary file detected (or invalid encoding). Text comparison unavailable.".into());
            self.screen = Screen::Diff;
            return;
        };

        let mut diff = DiffState::new(vec![FileDiff::from_texts(&src.rel_path, &src_txt, &dest_txt)]);
        diff.set_mode(DiffMode::Split);
        self.diff = Some(diff);
        self.screen = Screen::Diff;
    }

    // -- key routing ---------------------------------------------------------

    fn on_key(s: &mut OmniApp, ev: &KeyEvent, tx: &mut Tx) -> bool {
        if s.screen == Screen::Diff {
            if let Some(diff) = s.diff.as_mut() {
                if diff.key(ev, tx) {
                    return true;
                }
            }
            if ev.key == rune::Key::Escape {
                s.screen = Screen::Main;
                return true;
            }
        }
        false
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

/// The exact cost of a mirror sync, for the confirmation dialog.
fn mirror_warning(s: &OmniApp) -> String {
    let Some(r) = &s.results else { return "Nothing to sync yet.".into() };
    let copies = r.missing_in_dest.len() + r.different_content.len();
    let delete_count = r.missing_in_source.len();
    let delete_bytes: u64 = r.missing_in_source.iter().map(|f| f.size).sum();

    let mut lines = vec![
        format!("Copy {copies} file(s) to the destination."),
        format!(
            "Delete {delete_count} destination-only file(s) ({}).",
            humansize::format_size(delete_bytes, humansize::DECIMAL)
        ),
    ];

    match &s.deep {
        Some(d) if delete_count > 0 => {
            if d.doomed_unique.is_empty() {
                lines.push("🛡 Check passed: every file to be deleted also".into());
                lines.push("exists in the source (under another name).".into());
            } else {
                lines.push(format!(
                    "🛡 Check: {} of them exist ONLY in the destination —",
                    d.doomed_unique.len()
                ));
                lines.push("deleting them loses that content forever.".into());
            }
        }
        Some(_) => {}
        None if delete_count > 0 => {
            lines.push("⚠️ Not verified yet — run the 🛡 Backup check first.".into());
            lines.push("It shows whether the deleted content exists in the source under another name.".into());
        }
        None => {}
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------
// View
// ---------------------------------------------------------------------------

fn view(s: &OmniApp, cx: &mut Cx<OmniApp>) -> El {
    let k = Kit::neutral(s.settings.theme.resolve(cx.system_dark()));
    let mut layer = Vec::new();

    // macOS full-size-content windows draw under the traffic lights; clear them.
    let inset = cx.titlebar_inset();

    let body = match s.screen {
        Screen::Main => main_screen(s, &k, inset),
        Screen::Diff => diff_screen(s, &k, inset),
    };

    // Settings dialog: appearance + engine preferences.
    if s.show_settings {
        let desc = |t: String| text(t).font(k.ty.ui).color(k.c.text_faint).wrap();
        let mut settings_body = col().gap(14.0).w_full().padding(16.0);

        settings_body = settings_body.child(text("Appearance").font(k.ty.ui_medium).color(k.c.text_base));
        settings_body = settings_body.child(
            SegmentedControl::new(&Theme::LABELS, Some(s.settings.theme.index()))
                .width(None)
                .on_pick(|st: &mut OmniApp, i, _| {
                    st.settings.theme = Theme::from_index(i);
                    st.persist_settings();
                })
                .build(&k),
        );

        settings_body = settings_body.child(
            col()
                .w_full()
                .gap(4.0)
                .child(text("Comparison").font(k.ty.ui_medium).color(k.c.text_base))
                .child(
                    Checkbox::new(s.settings.fast_compare)
                        .label("Fast comparison (size & date only)")
                        .on_toggle(|st: &mut OmniApp| {
                            st.settings.fast_compare = !st.settings.fast_compare;
                            st.persist_settings();
                        })
                        .build(&k),
                )
                .child(desc("Skips Blake3 hashing — much faster, but files with equal size and date count as identical.".into()))
                .child(
                    Checkbox::new(s.settings.auto_insights)
                        .label("Open Insights after each comparison")
                        .on_toggle(|st: &mut OmniApp| {
                            st.settings.auto_insights = !st.settings.auto_insights;
                            st.persist_settings();
                        })
                        .build(&k),
                ),
        );

        settings_body = settings_body.child(
            col()
                .w_full()
                .gap(4.0)
                .child(text("Sync").font(k.ty.ui_medium).color(k.c.text_base))
                .child(
                    Checkbox::new(s.settings.preserve_mtime)
                        .label("Preserve modification dates when copying")
                        .on_toggle(|st: &mut OmniApp| {
                            st.settings.preserve_mtime = !st.settings.preserve_mtime;
                            st.persist_settings();
                        })
                        .build(&k),
                )
                .child(desc("Copied photos keep their capture date, so libraries still sort correctly. Recommended for backups.".into())),
        );

        settings_body = settings_body.child(
            col()
                .w_full()
                .gap(4.0)
                .child(text("General").font(k.ty.ui_medium).color(k.c.text_base))
                .child(
                    Checkbox::new(s.settings.remember_folders)
                        .label("Remember the last used folders")
                        .on_toggle(|st: &mut OmniApp| {
                            st.settings.remember_folders = !st.settings.remember_folders;
                            st.persist_settings();
                        })
                        .build(&k),
                )
                .child(desc("Pre-fills the source and destination fields on launch.".into())),
        );

        settings_body = settings_body.child(
            row()
                .gap(8.0)
                .justify_end()
                .child(
                    Button::new("Reset to defaults")
                        .variant(ButtonVariant::Ghost)
                        .on_press(|st: &mut OmniApp| {
                            st.settings = Settings::default();
                            st.persist_settings();
                        })
                        .build(&k),
                )
                .child(
                    Button::new("Done")
                        .variant(ButtonVariant::Submit)
                        .on_press(|st: &mut OmniApp| st.show_settings = false)
                        .build(&k),
                ),
        );

        Dialog::new("settings", "⚙️ Settings")
            .open(true)
            .fit()
            .width(430.0)
            .child(settings_body)
            .on_close(|st: &mut OmniApp| st.show_settings = false)
            .build(&k, &mut layer);
    }

    // Confirm-destructive-sync dialog: state the exact cost, and the deep
    // check's data-loss verdict when one has been run.
    if s.confirm_sync {
        let description = mirror_warning(s);
        Dialog::new("confirm-sync", "⚠️ Mirror sync")
            .open(true)
            .fit()
            .description(&description)
            .child(
                row()
                    .gap(8.0)
                    .justify_end()
                    .child(Button::new("Cancel").variant(ButtonVariant::Neutral).on_press(|st: &mut OmniApp| st.confirm_sync = false).build(&k))
                    .child(Button::new("Delete & sync").variant(ButtonVariant::Danger).on_press(|st: &mut OmniApp| {
                        st.confirm_sync = false;
                        st.start_sync();
                    }).build(&k)),
            )
            .on_close(|st: &mut OmniApp| st.confirm_sync = false)
            .build(&k, &mut layer);
    }

    rune_kit::root(body, layer)
}

fn main_screen(s: &OmniApp, k: &Kit, inset: f32) -> El {
    let busy = s.phase != Phase::Idle;
    let mut card = col()
        .key("config")
        .w_full()
        .padding(16.0)
        .gap(10.0)
        .radius(10.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_muted);

    // Source / destination path fields with native folder pickers.
    let kc = k.clone();
    let source_row = row()
        .w_full()
        .gap(8.0)
        .items_center()
        .child(text("Source").font(k.ty.ui).color(k.c.text_muted).w(92.0))
        .child(TextInput::new(&s.source).placeholder("/path/to/source").on_edit(|st: &mut OmniApp| &mut st.source).build(k).grow(1.0))
        .child(
            el().key("source-browse").child(
            IconButton::new(k.glyph.chevron_down).variant(IconButtonVariant::Neutral).label("Browse for source folder")
                .on_press_tx(move |_: &mut OmniApp, tx: &mut Tx| {
                    tx.open_folder(
                        rune::FileDialogOptions { title: "Pick the source folder".into(), ..Default::default() },
                        move |st: &mut OmniApp, answer, _| {
                            match answer {
                                Ok(Some(path)) => st.source = rune::TextEdit::new(path.to_string_lossy().to_string()),
                                Ok(None) => {} // cancelled
                                Err(e) => {
                                    st.status = format!("❌ Folder picker failed: {e}");
                                    st.status_error = true;
                                }
                            }
                        },
                    );
                    let _ = &kc;
                })
                .build(k))
        );
    let dest_row = row()
        .w_full()
        .gap(8.0)
        .items_center()
        .child(text("Destination").font(k.ty.ui).color(k.c.text_muted).w(92.0))
        .child(TextInput::new(&s.dest).placeholder("/path/to/destination").on_edit(|st: &mut OmniApp| &mut st.dest).build(k).grow(1.0))
        .child(
            el().key("dest-browse").child(
            IconButton::new(k.glyph.chevron_down).variant(IconButtonVariant::Neutral).label("Browse for destination folder")
                .on_press_tx(|_: &mut OmniApp, tx: &mut Tx| {
                    tx.open_folder(
                        rune::FileDialogOptions { title: "Pick the destination folder".into(), ..Default::default() },
                        move |st: &mut OmniApp, answer, _| {
                            match answer {
                                Ok(Some(path)) => st.dest = rune::TextEdit::new(path.to_string_lossy().to_string()),
                                Ok(None) => {} // cancelled
                                Err(e) => {
                                    st.status = format!("❌ Folder picker failed: {e}");
                                    st.status_error = true;
                                }
                            }
                        },
                    );
                })
                .build(k))
        );
    card = card.child(source_row).child(dest_row);

    card = card.child(
        Checkbox::new(s.delete_extra)
            .label("Delete extra files in destination (Mirror Mode)")
            .on_toggle(|st: &mut OmniApp| st.delete_extra = !st.delete_extra)
            .build(k),
    );

    let status_color = if s.status_error {
        k.c.danger.fg
    } else if s.status.starts_with("✅") || s.status.starts_with("🛡") {
        k.c.success.fg
    } else {
        k.c.text_muted
    };
    let status_row = row()
        .w_full()
        .gap(8.0)
        .items_center()
        .child(text(&s.status).font(k.ty.ui).color(status_color));
    card = card.child(status_row);

    // Action row.
    let mut actions = row().w_full().gap(10.0).items_center();
    let compare = el().key("compare").child(
        Button::new(if busy { "Working…" } else { "🔍 Start comparison" })
            .variant(ButtonVariant::Submit)
            .disabled(busy)
            .on_press(|st: &mut OmniApp| st.start_comparison())
            .build(k),
    );
    actions = actions.child(compare);

    if busy {
        let progress = if s.phase == Phase::Syncing || s.phase == Phase::Hashing {
            Progress::new(Some(s.progress))
        } else {
            Progress::new(None)
        };
        actions = actions.child(progress.label(&s.status).build(k).grow(1.0));
    } else if s.results.is_some() {
        actions = actions
            .child(el().grow(1.0))
            .child(
                Button::new("⚡ Sync to destination")
                    .variant(ButtonVariant::Contrast)
                    .on_press(|st: &mut OmniApp| st.request_sync())
                    .build(k),
            );
    }
    card = card.child(actions);

    // Results tabs + table.
    let mut results_area = col().w_full().grow(1.0).min_h(0.0).gap(8.0);
    if let Some(results) = &s.results {
        let counts = [
            results.missing_in_dest.len(),
            results.missing_in_source.len(),
            results.different_content.len(),
        ];
        let labels = [
            format!("Missing in Dest ({})", counts[0]),
            format!("Extra in Dest ({})", counts[1]),
            format!("Different ({})", counts[2]),
        ];
        let refs: [&str; 3] = [labels[0].as_str(), labels[1].as_str(), labels[2].as_str()];
        let tabs = SegmentedControl::new(&refs, Some(tab_index(s.tab)))
            .width(None)
            .on_pick(|st: &mut OmniApp, i, _| {
                st.tab = tab_from_index(i);
                st.rebuild_rows();
            })
            .build(k);
        results_area = results_area.child(tabs);

        // Toolbar: grouping + insights toggles + the backup-safety check.
        let mut toolbar = row()
            .w_full()
            .gap(10.0)
            .items_center()
            .child(
                Checkbox::new(s.grouped)
                    .label("Group by folder")
                    .on_toggle(|st: &mut OmniApp| {
                        st.grouped = !st.grouped;
                        st.rebuild_display();
                    })
                    .build(k),
            )
            .child(row().grow(1.0));

        // Active insights-panel filter, shown as a clearable chip.
        if let Some(f) = &s.filter {
            toolbar = toolbar.child(
                el().key("filter-clear").child(
                    Button::new(format!("🔎 {} ✕", f.label()))
                        .variant(ButtonVariant::Contrast)
                        .on_press(|st: &mut OmniApp| st.filter = None)
                        .build(k),
                ),
            );
        }

        toolbar = toolbar
            .child(
                el().key("insights-toggle").child(
                    Button::new(if s.show_insights { "📊 Hide insights" } else { "📊 Insights" })
                        .variant(ButtonVariant::Ghost)
                        .on_press(|st: &mut OmniApp| st.show_insights = !st.show_insights)
                        .build(k),
                ),
            )
            .child(
                el().key("deep-check").child(
                    Button::new(if s.deep.is_some() { "🛡 Re-run backup check" } else { "🛡 Backup check" })
                        .variant(ButtonVariant::Outline)
                        .disabled(busy)
                        .on_press(|st: &mut OmniApp| st.start_deep_check())
                        .build(k),
                ),
            );
        results_area = results_area.child(toolbar);

        if s.show_insights {
            results_area = results_area.child(
                col()
                    .w_full()
                    .max_h(260.0)
                    .min_h(0.0)
                    .scroll(rune::ScrollSpec { stick_bottom: false })
                    .child(insights_panel(s, k)),
            );
        }

        let table = results_table(s, k);
        results_area = results_area.child(table);
    } else {
        results_area = results_area.child(
            col()
                .grow(1.0)
                .center()
                .gap(6.0)
                .child(text("Compare two folders").font(k.ty.ui_medium).color(k.c.text_base))
                .child(
                    text("Pick a source and a destination above. The comparison shows what is new, what changed, and how much of your backup is already safe.")
                        .font(k.ty.ui)
                        .color(k.c.text_faint),
                ),
        );
    }

    let header = row()
        .w_full()
        .h(48.0)
        .pl(16.0 + inset)
        .pr(16.0)
        .gap(10.0)
        .items_center()
        .child(text("🚀 OmniDiff Pro").font(k.ty.ui_medium).color(k.c.text_base))
        .child(text("v2.0.0 · parallel Blake3 · Rune UI").font(k.ty.ui).color(k.c.text_faint))
        .child(row().grow(1.0))
        .child(
            el().key("settings").child(
                IconButton::new(&SETTINGS)
                    .variant(IconButtonVariant::Neutral)
                    .label("Open settings")
                    .on_press(|st: &mut OmniApp| st.show_settings = true)
                    .build(k),
            ),
        );

    col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(header)
        .child(col().w_full().grow(1.0).min_h(0.0).padding(16.0).gap(12.0).child(card).child(results_area))
        .key("main")
}

fn results_table(s: &OmniApp, k: &Kit) -> El {
    let kc = k.clone();
    let base = Table::new(
        |st: &OmniApp| &st.table,
        |st: &mut OmniApp| &mut st.table,
        |st: &OmniApp| st.display.len(),
        |st: &OmniApp, i| row_id(st, i),
    )
    .column(TableColumn::new("path", "Path").flex(1.0))
    .zebra(true)
    .name("Results")
    .cell(move |st: &OmniApp, i, col| match st.display.get(i) {
        Some(DisplayRow::Folder(dir, count)) if col == "path" => Cell::el(
            row()
                .gap(6.0)
                .items_center()
                .child(text(if st.collapsed.contains(dir) { "▸" } else { "▾" }).color(kc.c.text_faint))
                .child(text(format!("📁 {dir}")).font(kc.ty.ui_medium).color(kc.c.text_base))
                .child(text(if *count == 1 { "1 file".to_owned() } else { format!("{count} files") }).font(kc.ty.ui).color(kc.c.text_muted)),
        ),
        Some(DisplayRow::Folder(..)) => Cell::text(""),
        Some(DisplayRow::File(i)) => match st.tab {
            Tab::Different => match col {
                "path" => Cell::text(st.pairs.get(*i).map(|p| p.0.rel_path.clone()).unwrap_or_default()).mono(),
                "src" => Cell::text(st.pairs.get(*i).map(|p| humansize::format_size(p.0.size, humansize::DECIMAL)).unwrap_or_default()).mono(),
                "dest" => Cell::text(st.pairs.get(*i).map(|p| humansize::format_size(p.1.size, humansize::DECIMAL)).unwrap_or_default()).mono(),
                "actions" => {
                    let id = st.pairs.get(*i).map(|p| p.0.rel_path.clone()).unwrap_or_default();
                    let reveal_path = st.pairs.get(*i).map(|p| p.0.path.clone());
                    Cell::el(
                        row()
                            .gap(4.0)
                            .child(
                                Button::new("View diff")
                                    .variant(ButtonVariant::Outline)
                                    .on_press(move |st: &mut OmniApp| st.open_diff(&id))
                                    .build(&kc),
                            )
                            .child(reveal_button(&kc, reveal_path, "Show source file in Finder")),
                    )
                }
                _ => Cell::text(""),
            },
            _ => match col {
                "path" => Cell::text(st.rows.get(*i).map(|f| f.rel_path.clone()).unwrap_or_default()).mono(),
                "size" => Cell::text(st.rows.get(*i).map(|f| humansize::format_size(f.size, humansize::DECIMAL)).unwrap_or_default()).mono(),
                "modified" => Cell::text(st.rows.get(*i).map(|f| format_time(f.modified)).unwrap_or_default()),
                "reveal" => {
                    let reveal_path = st.rows.get(*i).map(|f| f.path.clone());
                    Cell::el(reveal_button(&kc, reveal_path, "Show file in Finder"))
                }
                _ => Cell::text(""),
            },
        },
        None => Cell::text(""),
    })
    .on_select(|st: &mut OmniApp, id: &str, _| {
        if let Some(dir) = id.strip_prefix("dir:") {
            st.toggle_folder(dir);
        }
    })
    .on_open(|st: &mut OmniApp, id: &str, _| {
        if let Some(rel) = id.strip_prefix("file:") {
            if st.tab == Tab::Different {
                st.open_diff(rel);
            } else if let Some(path) = st.rows.iter().find(|f| f.rel_path == rel).map(|f| f.path.clone()) {
                st.reveal_in_finder(&path);
            }
        }
    });

    let table = match s.tab {
        Tab::Different => base
            .column(TableColumn::new("src", "Source size").width(110.0))
            .column(TableColumn::new("dest", "Destination size").width(130.0))
            .column(TableColumn::new("actions", "").width(190.0)),
        _ => base
            .column(TableColumn::new("size", "Size").width(110.0))
            .column(TableColumn::new("modified", "Modified").width(150.0))
            .column(TableColumn::new("reveal", "").width(56.0)),
    };

    table.build(s, k).grow(1.0).min_h(0.0)
}

/// Small icon button that reveals a file in Finder.
fn reveal_button(k: &Kit, path: Option<PathBuf>, label: &'static str) -> El {
    match path {
        Some(path) => IconButton::new(&FINDER)
            .variant(IconButtonVariant::Neutral)
            .label(label)
            .on_press(move |st: &mut OmniApp| st.reveal_in_finder(&path))
            .build(k),
        None => el(),
    }
}

/// Stable id for a display row: folder headers toggle, file rows open.
fn row_id(s: &OmniApp, i: usize) -> std::borrow::Cow<'static, str> {
    match s.display.get(i) {
        Some(DisplayRow::Folder(dir, _)) => std::borrow::Cow::Owned(format!("dir:{dir}")),
        Some(DisplayRow::File(i)) => std::borrow::Cow::Owned(format!("file:{}", s.rel_of(*i))),
        None => std::borrow::Cow::Borrowed("row"),
    }
}

// ---------------------------------------------------------------------------
// Insights panel — rule-based analysis, no models involved
// ---------------------------------------------------------------------------

/// A thin horizontal meter filled to `frac` (0..1).
fn meter(k: &Kit, frac: f32, fill: Color) -> El {
    row()
        .w_full()
        .h(6.0)
        .radius(3.0)
        .fill(k.c.layer_03)
        .child(el().w_frac(frac.clamp(0.02, 1.0)).h(6.0).radius(3.0).fill(fill))
}

/// A clickable full-width row inside the insights panel.
fn clickable_row(key: String, content: El, on_press: impl Fn(&mut OmniApp) + 'static) -> El {
    el()
        .key(key)
        .w_full()
        .cursor(rune::Cursor::Pointer)
        .on_press(on_press)
        .child(content)
}

fn insights_panel(s: &OmniApp, k: &Kit) -> El {
    let Some(ins) = &s.insights else { return col() };
    let mut panel = col()
        .key("insights")
        .w_full()
        .padding(14.0)
        .gap(10.0)
        .radius(10.0)
        .fill(k.c.layer_01)
        .border(1.0, k.c.border_muted);

    let new_total: usize = ins.folders.iter().map(|f| f.new_files).sum();
    let extra_total: usize = ins.folders.iter().map(|f| f.extra_files).sum();
    let changed_total: usize = ins.folders.iter().map(|f| f.changed_files).sum();

    // -- Backup coverage ----------------------------------------------------
    let cov_pct = ins.coverage() * 100.0;
    let byte_pct = ins.coverage_bytes() * 100.0;
    let cov_color = if byte_pct >= 99.9 { k.c.success.fg } else { k.c.bg_accent };
    panel = panel.child(
        row()
            .w_full()
            .gap(10.0)
            .items_center()
            .child(text("🛡 Backup coverage").font(k.ty.ui_medium).color(k.c.text_base))
            .child(
                text(format!(
                    "{:.0}% of files · {:.0}% of content already in the destination",
                    cov_pct, byte_pct
                ))
                .font(k.ty.ui)
                .color(k.c.text_muted)
                .wrap(),
            ),
    );
    panel = panel.child(meter(k, ins.coverage_bytes(), cov_color));
    panel = panel.child(
        text(format!(
            "{} new · {} extra · {} changed · {} already backed up (of {} source files, {} total)",
            new_total,
            extra_total,
            changed_total,
            ins.identical,
            ins.source_files,
            humansize::format_size(ins.source_bytes, humansize::DECIMAL)
        ))
        .font(k.ty.ui)
        .color(k.c.text_muted)
        .wrap(),
    );

    // -- Folder breakdown (click to filter the table) ------------------------
    if !ins.folders.is_empty() {
        panel = panel.child(
            text("Where the changes live — click a folder to filter the list below")
                .font(k.ty.ui)
                .color(k.c.text_faint),
        );
        let max_involved = ins.folders.iter().map(|f| f.involved()).max().unwrap_or(1) as f32;
        for f in ins.folders.iter().take(8) {
            let mut detail = Vec::new();
            if f.new_files > 0 { detail.push(format!("{} new", f.new_files)); }
            if f.extra_files > 0 { detail.push(format!("{} extra", f.extra_files)); }
            if f.changed_files > 0 { detail.push(format!("{} changed", f.changed_files)); }
            let dir = f.name.clone();
            let active = s.filter == Some(Filter::Folder(dir.clone()));
            let row = row()
                .w_full()
                .gap(10.0)
                .items_center()
                .child(
                    text(if active { format!("📁 {dir} ✕") } else { format!("📁 {dir}") })
                        .font(k.ty.ui)
                        .color(if active { k.c.text_accent } else { k.c.text_base })
                        .min_w(120.0)
                        .wrap(),
                )
                .child(row().grow(1.0).child(el().w_frac(f.involved() as f32 / max_involved).h(4.0).radius(2.0).fill(k.c.bg_accent)))
                .child(text(detail.join(" · ")).font(k.ty.ui).color(k.c.text_muted).min_w(110.0).wrap());
            panel = panel.child(clickable_row(
                format!("folder-{dir}"),
                row,
                move |st: &mut OmniApp| st.toggle_filter(Filter::Folder(dir.clone())),
            ));
        }
    }

    // -- Type breakdown (click to filter) ------------------------------------
    if !ins.types.is_empty() {
        panel = panel.child(
            text("By type — click to filter the list below").font(k.ty.ui).color(k.c.text_faint),
        );
        let mut types_col = col().w_full().gap(4.0);
        for t in ins.types.iter().take(5) {
            let label = t.label.to_owned();
            let active = s.filter == Some(Filter::Type(label.clone()));
            let row = row()
                .w_full()
                .gap(10.0)
                .items_center()
                .child(
                    text(if active {
                        format!("{} {} ✕", t.emoji, t.label)
                    } else {
                        format!("{} {}", t.emoji, t.label)
                    })
                    .font(k.ty.ui)
                    .color(if active { k.c.text_accent } else { k.c.text_base })
                    .min_w(110.0),
                )
                .child(text(format!("{} files · {}", t.files, humansize::format_size(t.bytes, humansize::DECIMAL))).font(k.ty.ui).color(k.c.text_muted));
            types_col = types_col.child(clickable_row(
                format!("type-{label}"),
                row,
                move |st: &mut OmniApp| st.toggle_filter(Filter::Type(label.clone())),
            ));
        }
        panel = panel.child(types_col);
    }

    // -- Attention: largest new files (click to reveal) ----------------------
    if !ins.biggest_new.is_empty() {
        panel = panel.child(text("Largest new files — click to show in Finder").font(k.ty.ui).color(k.c.text_faint));
        let mut files_col = col().w_full().gap(4.0);
        for f in ins.biggest_new.iter().take(4) {
            let path = f.path.clone();
            let row = row()
                .w_full()
                .gap(10.0)
                .items_center()
                .child(
                    el()
                        .w_frac(0.06)
                        .h(1.0),
                )
                .child(text(&f.rel_path).font(k.ty.ui).color(k.c.text_accent).min_w(140.0).wrap())
                .child(text(humansize::format_size(f.size, humansize::DECIMAL)).font(k.ty.ui).color(k.c.text_muted));
            files_col = files_col.child(clickable_row(
                format!("new-{}", f.rel_path),
                row,
                move |st: &mut OmniApp| st.reveal_in_finder(&path),
            ));
        }
        panel = panel.child(files_col);
    }

    // -- Deep check (backup safety) -----------------------------------------
    panel = panel.child(row().w_full().h(1.0).fill(k.c.border_muted));
    match &s.deep {
        None => {
            panel = panel.child(
                text("The 🛡 Backup check (in the toolbar) hashes every file on both sides (Blake3, parallel) to find renamed photos, duplicates, and exactly what mirror sync would destroy.")
                    .font(k.ty.ui)
                    .color(k.c.text_faint)
                    .wrap(),
            );
        }
        Some(d) => {
            let content_pct = if d.total_bytes > 0 { d.covered_bytes as f64 / d.total_bytes as f64 * 100.0 } else { 100.0 };
            panel = panel.child(
                text(format!(
                    "🛡 Content coverage (names ignored): {:.1}% of the source's volume exists in the destination ({} files hashed)",
                    content_pct, d.hashed
                ))
                .font(k.ty.ui)
                .color(k.c.text_base)
                .wrap(),
            );
            panel = panel.child(meter(k, if d.total_bytes > 0 { d.covered_bytes as f32 / d.total_bytes as f32 } else { 1.0 }, if content_pct >= 99.9 { k.c.success.fg } else { k.c.bg_accent }));

            if d.renamed_total > 0 {
                panel = panel.child(
                    text(format!(
                        "🔁 {} file(s) already backed up under different names — click to show in Finder:",
                        d.renamed_total
                    ))
                    .font(k.ty.ui)
                    .color(k.c.text_muted)
                    .wrap(),
                );
                let mut renamed_col = col().w_full().gap(4.0);
                for (src, dest) in d.renamed_matches.iter().take(3) {
                    let path = dest.path.clone();
                    let row = row()
                        .w_full()
                        .gap(10.0)
                        .items_center()
                        .child(el().w_frac(0.06).h(1.0))
                        .child(
                            text(format!("{} ⇢ {}", src.rel_path, dest.rel_path))
                                .font(k.ty.ui)
                                .color(k.c.text_accent)
                                .min_w(140.0)
                                .wrap(),
                        );
                    renamed_col = renamed_col.child(clickable_row(
                        format!("renamed-{}", dest.rel_path),
                        row,
                        move |st: &mut OmniApp| st.reveal_in_finder(&path),
                    ));
                }
                panel = panel.child(renamed_col);
            }

            if d.doomed_total() > 0 {
                let tone = if d.doomed_unique.is_empty() { k.c.text_muted } else { k.c.danger.fg };
                panel = panel.child(
                    text(format!(
                        "♻️ Mirror sync would delete {} destination-only file(s): {} safe (content kept in source under another name), {} LOST permanently",
                        d.doomed_total(),
                        d.doomed_preserved.len(),
                        d.doomed_unique.len()
                    ))
                    .font(k.ty.ui)
                    .color(tone)
                    .wrap(),
                );
                if !d.doomed_unique.is_empty() {
                    panel = panel.child(
                        text("Would be gone forever — click to show in Finder:")
                            .font(k.ty.ui)
                            .color(k.c.danger.fg),
                    );
                    let mut doomed_col = col().w_full().gap(4.0);
                    for f in d.doomed_unique.iter().take(4) {
                        let path = f.path.clone();
                        let row = row()
                            .w_full()
                            .gap(10.0)
                            .items_center()
                            .child(el().w_frac(0.06).h(1.0))
                            .child(text(&f.rel_path).font(k.ty.ui).color(k.c.danger.fg).min_w(140.0).wrap())
                            .child(text(humansize::format_size(f.size, humansize::DECIMAL)).font(k.ty.ui).color(k.c.text_muted));
                        doomed_col = doomed_col.child(clickable_row(
                            format!("doomed-{}", f.rel_path),
                            row,
                            move |st: &mut OmniApp| st.reveal_in_finder(&path),
                        ));
                    }
                    panel = panel.child(doomed_col);
                }
            } else {
                panel = panel.child(
                    text("♻️ Mirror sync would delete nothing unique — every destination-only file's content survives in the source.")
                        .font(k.ty.ui)
                        .color(k.c.success.fg)
                        .wrap(),
                );
            }

            if !d.unbacked_by_month.is_empty() {
                let timeline = d
                    .unbacked_by_month
                    .iter()
                    .take(5)
                    .map(|b| format!("{} ({} files · {})", b.label, b.files, humansize::format_size(b.bytes, humansize::DECIMAL)))
                    .collect::<Vec<_>>()
                    .join(", ");
                panel = panel.child(
                    text(format!("📅 Not yet backed up, by date: {timeline}"))
                        .font(k.ty.ui)
                        .color(k.c.text_muted)
                        .wrap(),
                );
            }

            let wasted = d.wasted_bytes();
            if wasted > 0 {
                panel = panel.child(
                    text(format!(
                        "🧹 Exact duplicate content inside the folders: {} could be reclaimed ({} source + {} destination duplicate group(s))",
                        humansize::format_size(wasted, humansize::DECIMAL),
                        d.src_dupes.len(),
                        d.dest_dupes.len()
                    ))
                    .font(k.ty.ui)
                    .color(k.c.text_muted)
                    .wrap(),
                );
            }
        }
    }

    panel
}

fn tab_index(t: Tab) -> usize {
    match t {
        Tab::MissingInDest => 0,
        Tab::MissingInSource => 1,
        Tab::Different => 2,
    }
}

fn tab_from_index(i: usize) -> Tab {
    match i {
        0 => Tab::MissingInDest,
        1 => Tab::MissingInSource,
        _ => Tab::Different,
    }
}

fn format_time(ts: u64) -> String {
    if let Some(dt) = chrono::DateTime::from_timestamp(ts as i64, 0) {
        dt.format("%Y-%m-%d %H:%M").to_string()
    } else {
        "-".into()
    }
}

/// Seconds as "42s" or "3m 05s".
fn fmt_secs(secs: u64) -> String {
    if secs >= 60 {
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}s")
    }
}

// ---------------------------------------------------------------------------
// Diff screen
// ---------------------------------------------------------------------------

fn diff_screen(s: &OmniApp, k: &Kit, inset: f32) -> El {
    // Both sides of the diff exist on disk; offer Finder reveal for each.
    let paths: Option<(PathBuf, PathBuf)> = s.results.as_ref().and_then(|r| {
        r.different_content
            .iter()
            .find(|(src, _)| src.rel_path == s.diff_title)
            .map(|(src, dest)| (src.path.clone(), dest.path.clone()))
    });
    let kc = k.clone();
    let mut bar = row()
        .w_full()
        .h(48.0)
        .pl(8.0 + inset)
        .pr(16.0)
        .gap(10.0)
        .items_center()
        .fill(k.c.layer_01)
        .child(
            Button::new("← Results")
                .variant(ButtonVariant::Ghost)
                .on_press(|st: &mut OmniApp| st.screen = Screen::Main)
                .build(k),
        )
        .child(text(&s.diff_title).font(k.ty.ui_medium).color(k.c.text_base))
        .child(row().grow(1.0));

    if let Some((src_path, dest_path)) = paths {
        bar = bar
            .child(
                IconButton::new(&FINDER)
                    .variant(IconButtonVariant::Neutral)
                    .label("Show source file in Finder")
                    .on_press(move |st: &mut OmniApp| st.reveal_in_finder(&src_path))
                    .build(&kc),
            )
            .child(
                IconButton::new(&FINDER)
                    .variant(IconButtonVariant::Neutral)
                    .label("Show destination file in Finder")
                    .on_press(move |st: &mut OmniApp| st.reveal_in_finder(&dest_path))
                    .build(&kc),
            );
    }

    let body: El = if s.diff_is_image {
        image_compare(s, k)
    } else if let Some(err) = &s.image_error {
        col()
            .grow(1.0)
            .center()
            .child(text(err).font(k.ty.ui).color(k.c.danger.fg))
    } else if s.diff.is_some() {
        DiffView::new(|st: &OmniApp| st.diff.as_ref().unwrap(), |st: &mut OmniApp| st.diff.as_mut().unwrap())
            .build(s, k)
            .grow(1.0)
            .min_h(0.0)
            .key("diff-view")
    } else {
        col().grow(1.0)
    };

    col()
        .w_full()
        .h_full()
        .fill(k.c.bg_base)
        .child(bar)
        .child(body)
}

fn image_compare(s: &OmniApp, k: &Kit) -> El {
    let pane = |label: &'static str, img: Option<&Image>| {
        col()
            .grow(1.0)
            .min_h(0.0)
            .gap(6.0)
            .padding(10.0)
            .center()
            .child(text(label).font(k.ty.ui).color(k.c.text_muted))
            .child(match img {
                Some(img) => el().grow(1.0).min_h(0.0).w_full().center().child(
                    el()
                        .pixels_fit(img.pixels(), rune::SurfaceFit::Fit)
                        .w_full()
                        .h_full(),
                ),
                None => text("Failed to load image").font(k.ty.ui).color(k.c.danger.fg),
            })
    };
    row()
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .children([
            pane("Source", s.img_src.as_ref()),
            pane("Destination", s.img_dest.as_ref()),
        ])
        .key("image-compare")
}

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

pub struct OmniBoot;

// -- Snapshot scenes (headless testing) --------------------------------------

/// Real folders with content, used by the end-to-end scene.
fn make_e2e_dirs() -> (PathBuf, PathBuf) {
    use std::fs;
    let base = std::env::temp_dir().join(format!("omnidiff2_e2e_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let src = base.join("src");
    let dest = base.join("dest");
    for root in [&src, &dest] {
        fs::create_dir_all(root.join("sub")).unwrap();
    }
    fs::write(src.join("same.txt"), "identical content\nline two\n").unwrap();
    fs::write(dest.join("same.txt"), "identical content\nline two\n").unwrap();
    fs::write(src.join("sub/changed.rs"), "fn main() {\n    println!(\"v1\");\n}\n").unwrap();
    fs::write(dest.join("sub/changed.rs"), "fn main() {\n    println!(\"v2\");\n    eprintln!(\"new\");\n}\n").unwrap();
    fs::write(src.join("only_src.md"), "# source only\n").unwrap();
    fs::write(dest.join("only_dest.md"), "# destination only\n").unwrap();
    (src, dest)
}

fn scene_results(s: &mut OmniApp) {
    // Snapshots must not depend on the user's saved settings.
    s.settings = Settings::default();
    let (src, dest) = make_e2e_dirs();
    s.source = rune::TextEdit::new(src.to_string_lossy().to_string());
    s.dest = rune::TextEdit::new(dest.to_string_lossy().to_string());
    let (results, _) = rayon::join(
        || scanner::run_comparison(src.clone(), dest.clone(), true, |_| {}),
        || (),
    );
    s.apply_results(results.unwrap());
}

fn scene_diff(s: &mut OmniApp) {
    scene_results(s);
    if let Some(results) = &s.results {
        if let Some((src_entry, dest_entry)) = results.different_content.first() {
            let src_txt = std::fs::read_to_string(&src_entry.path).unwrap_or_default();
            let dest_txt = std::fs::read_to_string(&dest_entry.path).unwrap_or_default();
            let mut diff = DiffState::new(vec![FileDiff::from_texts(&src_entry.rel_path, &src_txt, &dest_txt)]);
            diff.set_mode(DiffMode::Split);
            s.diff = Some(diff);
            s.diff_title = src_entry.rel_path.clone();
            s.screen = Screen::Diff;
        }
    }
}

fn scene_confirm(s: &mut OmniApp) {
    scene_results(s);
    s.delete_extra = true;
    s.confirm_sync = true;
}

/// A photo-backup scenario: renamed duplicates across sides, one unique
/// destination-only video, and a new photo. Used for the insights + backup
/// check scenes.
fn make_backup_dirs() -> (PathBuf, PathBuf) {
    use std::fs;
    let base = std::env::temp_dir().join(format!("omnidiff2_backup_{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    let src = base.join("new_backup");
    let dest = base.join("old_backup");
    fs::create_dir_all(src.join("DCIM/Camera")).unwrap();
    fs::create_dir_all(dest.join("DCIM/Camera")).unwrap();
    fs::create_dir_all(dest.join("WhatsApp/Media")).unwrap();

    // Identical in both.
    fs::write(src.join("DCIM/Camera/IMG_0001.jpg"), "photo: beach sunset".repeat(50)).unwrap();
    fs::write(dest.join("DCIM/Camera/IMG_0001.jpg"), "photo: beach sunset".repeat(50)).unwrap();
    // Renamed overlap: same content, different names.
    fs::write(src.join("DCIM/Camera/IMG_0042.jpg"), "photo: birthday cake".repeat(60)).unwrap();
    fs::write(dest.join("DCIM/Camera/IMG_2026_10_04.jpg"), "photo: birthday cake".repeat(60)).unwrap();
    // New photo: only in the new backup.
    fs::write(src.join("DCIM/Camera/IMG_0043.jpg"), "photo: new year fireworks".repeat(70)).unwrap();
    // Unique destination-only video: mirror sync would lose this content.
    fs::write(dest.join("WhatsApp/Media/VID_2025_12_24.mp4"), "video: christmas dinner".repeat(80)).unwrap();
    (src, dest)
}

fn scene_insights(s: &mut OmniApp) {
    s.settings = Settings::default();
    let (src, dest) = make_backup_dirs();
    s.source = rune::TextEdit::new(src.to_string_lossy().to_string());
    s.dest = rune::TextEdit::new(dest.to_string_lossy().to_string());
    let (results, _) = rayon::join(
        || scanner::run_comparison(src.clone(), dest.clone(), true, |_| {}),
        || (),
    );
    s.apply_results(results.unwrap());
    s.show_insights = true;
}

fn scene_backup(s: &mut OmniApp) {
    scene_insights(s);
    let source = PathBuf::from(s.source.text.trim());
    let dest = PathBuf::from(s.dest.text.trim());
    if let Ok(check) = insights::deep_check(source, dest, |_| {}) {
        s.apply_deep(check);
    }
}

/// Insights open with an active folder filter (set by clicking a folder row).
fn scene_filtered(s: &mut OmniApp) {
    scene_backup(s);
    s.filter = Some(Filter::Folder("DCIM".into()));
    s.rebuild_display();
}

fn scene_e2e(s: &mut OmniApp) {
    s.settings = Settings::default();
    let (src, dest) = make_e2e_dirs();
    s.source = rune::TextEdit::new(src.to_string_lossy().to_string());
    s.dest = rune::TextEdit::new(dest.to_string_lossy().to_string());
}

/// Results with one folder group already collapsed.
fn scene_collapsed(s: &mut OmniApp) {
    scene_results(s);
    s.collapsed.insert("sub".to_owned());
    s.rebuild_display();
}

/// The Different tab of the results table.
fn scene_different(s: &mut OmniApp) {
    scene_results(s);
    s.tab = Tab::Different;
    s.rebuild_rows();
}

/// The settings dialog.
fn scene_settings(s: &mut OmniApp) {
    scene_backup(s);
    s.show_settings = true;
}

impl OmniBoot {
    pub fn run() {
        let spec = AppSpec::<OmniApp> {
            title: "OmniDiff Pro",
            size: (960.0, 720.0),
            min_size: Some((720.0, 520.0)),
            scenes: &[
                ("results", scene_results),
                ("diff", scene_diff),
                ("confirm", scene_confirm),
                ("insights", scene_insights),
                ("backup", scene_backup),
                ("collapsed", scene_collapsed),
                ("different", scene_different),
                ("filtered", scene_filtered),
                ("settings", scene_settings),
                ("e2e", scene_e2e),
            ],
            setup: Some(|s: &mut OmniApp, waker| {
                s.images = Some(ImageCache::new(waker));
            }),
            on_key: Some(OmniApp::on_key),
            families: ("Inter", "ui-monospace, Inter"),
            ..AppSpec::new("OmniDiff Pro", (960.0, 720.0), OmniApp::new, view)
        };

        let app = App::new(spec);
        let _ = HANDLE.set(app.handle());
        JOBS.get_or_init(|| {
            let (tx, rx) = channel();
            let _ = JOBS_RX.set(std::sync::Mutex::new(rx));
            tx
        });
        spawn_worker();
        app.run();
    }
}

// Keep the rev constant referenced so it stays checked at compile time.
#[allow(dead_code)]
fn rune_rev_check() -> &'static str {
    RUNE_REV
}
