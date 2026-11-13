//! Rule-based analysis — no models, no guessing: arithmetic over the comparison
//! result plus full-content hashing for the backup-safety check.
//!
//! Two layers:
//! * [`Insights`] — computed instantly from a [`CompareResult`] (no extra I/O).
//! * [`DeepCheck`] — an on-demand pass that full-Blake3-hashes everything on
//!   both sides and answers the questions name-based comparison cannot:
//!   renamed files, duplicate photos, and what a mirror sync would really
//!   destroy.

use std::collections::HashMap;
use std::path::Path;

use exif::Value;
use rayon::prelude::*;

use crate::scanner::{self, CompareResult, FileEntry, ScanStatus};

// ---------------------------------------------------------------------------
// Cheap insights from the comparison result
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FolderStat {
    /// First path component ("DCIM"), or "(root)" for top-level files.
    pub name: String,
    pub new_files: usize,     // in source, missing in destination
    pub extra_files: usize,   // in destination only
    pub changed_files: usize, // same name, different content
    pub new_bytes: u64,
    pub extra_bytes: u64,
    pub changed_bytes: u64,
}

impl FolderStat {
    pub fn involved(&self) -> usize {
        self.new_files + self.extra_files + self.changed_files
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct TypeStat {
    pub label: &'static str,
    pub emoji: &'static str,
    pub files: usize,
    pub bytes: u64,
}

/// Instant, rule-based read of what the comparison found.
#[derive(Clone, Debug, Default)]
pub struct Insights {
    pub source_files: usize,
    pub dest_files: usize,
    /// Same relative path and same content.
    pub identical: usize,
    pub identical_bytes: u64,
    pub source_bytes: u64,
    pub dest_bytes: u64,
    /// Sorted by how many differences they hold, most-involved first.
    pub folders: Vec<FolderStat>,
    /// Files involved in the differences (new + extra + changed), by kind.
    pub types: Vec<TypeStat>,
    /// Top new (source-only) files by size, largest first.
    pub biggest_new: Vec<FileEntry>,
    /// Top changed files by source size, largest first.
    pub biggest_changed: Vec<(FileEntry, FileEntry)>,
}

impl Insights {
    pub fn from_result(r: &CompareResult) -> Self {
        let mut folders: HashMap<String, FolderStat> = HashMap::new();
        let mut types: HashMap<(&'static str, &'static str), (usize, u64)> = HashMap::new();
        let mut source_bytes = 0u64;
        let mut dest_bytes = 0u64;

        let mut folder = |rel: &str| -> String {
            rel.split_once('/')
                .map(|(dir, _)| dir.to_owned())
                .unwrap_or_else(|| "(root)".to_owned())
        };
        let mut add_type = |bytes: u64, n: &mut usize, b: &mut u64| {
            *n += 1;
            *b += bytes;
        };

        for e in &r.missing_in_dest {
            source_bytes += e.size;
            let f = folder(&e.rel_path);
            let st = folders.entry(f).or_default();
            st.name = folder(&e.rel_path);
            st.new_files += 1;
            st.new_bytes += e.size;
            let kind = classify(&e.rel_path);
            let t = types.entry((kind.0, kind.1)).or_insert((0, 0));
            add_type(e.size, &mut t.0, &mut t.1);
        }
        for e in &r.missing_in_source {
            dest_bytes += e.size;
            let f = folder(&e.rel_path);
            let st = folders.entry(f).or_default();
            st.name = folder(&e.rel_path);
            st.extra_files += 1;
            st.extra_bytes += e.size;
            let kind = classify(&e.rel_path);
            let t = types.entry((kind.0, kind.1)).or_insert((0, 0));
            add_type(e.size, &mut t.0, &mut t.1);
        }
        for (src, dest) in &r.different_content {
            source_bytes += src.size;
            dest_bytes += dest.size;
            let f = folder(&src.rel_path);
            let st = folders.entry(f).or_default();
            st.name = folder(&src.rel_path);
            st.changed_files += 1;
            st.changed_bytes += src.size;
            let kind = classify(&src.rel_path);
            let t = types.entry((kind.0, kind.1)).or_insert((0, 0));
            add_type(src.size.max(dest.size), &mut t.0, &mut t.1);
        }

        let mut folders: Vec<FolderStat> = folders.into_values().collect();
        folders.sort_by(|a, b| b.involved().cmp(&a.involved()).then(a.name.cmp(&b.name)));

        let mut types: Vec<TypeStat> = types
            .into_iter()
            .map(|((label, emoji), (files, bytes))| TypeStat { label, emoji, files, bytes })
            .collect();
        types.sort_by(|a, b| b.bytes.cmp(&a.bytes).then(b.files.cmp(&a.files)));

        let mut biggest_new = r.missing_in_dest.clone();
        biggest_new.sort_by(|a, b| b.size.cmp(&a.size));
        biggest_new.truncate(5);

        let mut biggest_changed = r.different_content.clone();
        biggest_changed.sort_by(|a, b| b.0.size.cmp(&a.0.size));
        biggest_changed.truncate(5);

        // Total volume of each side = identical content + the differences.
        source_bytes += r.identical_bytes;
        dest_bytes += r.identical_bytes;

        Insights {
            source_files: r.source_count,
            dest_files: r.dest_count,
            identical: r.identical,
            identical_bytes: r.identical_bytes,
            source_bytes,
            dest_bytes,
            folders,
            types,
            biggest_new,
            biggest_changed,
        }
    }

    /// Fraction of source files already mirrored by name and content.
    pub fn coverage(&self) -> f32 {
        if self.source_files == 0 { return 1.0 }
        self.identical as f32 / self.source_files as f32
    }

    /// Byte-weighted coverage: how much of the source's volume is already there.
    pub fn coverage_bytes(&self) -> f32 {
        if self.source_bytes == 0 { return 1.0 }
        self.identical_bytes as f32 / self.source_bytes as f32
    }
}

/// (label, emoji) by extension; media kinds matter most for backup workflows.
pub fn classify(rel_path: &str) -> (&'static str, &'static str) {
    let ext = rel_path.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    const PHOTOS: &[&str] = &["jpg", "jpeg", "heic", "heif", "png", "gif", "webp", "dng", "cr2", "cr3", "nef", "arw", "rw2", "orf", "tiff", "tif", "bmp", "avif"];
    const VIDEOS: &[&str] = &["mp4", "mov", "mkv", "avi", "webm", "m4v", "3gp", "3g2", "mts", "ts", "wmv", "flv"];
    const AUDIO: &[&str] = &["mp3", "wav", "flac", "aac", "ogg", "m4a", "opus", "wma", "aiff"];
    const DOCS: &[&str] = &["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "md", "rtf", "pages", "numbers", "key", "csv", "epub"];
    const CODE: &[&str] = &["rs", "js", "ts", "tsx", "jsx", "py", "java", "c", "cpp", "h", "hpp", "go", "rb", "swift", "kt", "sh", "json", "xml", "html", "css", "yaml", "yml", "toml", "sql"];
    const ARCHIVES: &[&str] = &["zip", "tar", "gz", "bz2", "xz", "7z", "rar", "dmg", "iso", "apk"];
    if PHOTOS.contains(&ext.as_str()) { ("Photos", "🖼") }
    else if VIDEOS.contains(&ext.as_str()) { ("Videos", "🎬") }
    else if AUDIO.contains(&ext.as_str()) { ("Audio", "🎵") }
    else if DOCS.contains(&ext.as_str()) { ("Documents", "📄") }
    else if CODE.contains(&ext.as_str()) { ("Code", "🧩") }
    else if ARCHIVES.contains(&ext.as_str()) { ("Archives", "📦") }
    else { ("Other", "📁") }
}

// ---------------------------------------------------------------------------
// Deep check: full-content hashing of both sides (on demand)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DupeGroup {
    pub hash: String,
    pub files: Vec<FileEntry>,
    /// Bytes used by the redundant copies beyond the first.
    pub wasted_bytes: u64,
}

/// Result of the backup-safety deep check.
#[derive(Clone, Debug, Default)]
pub struct DeepCheck {
    /// Destination files whose content exists in the source under a
    /// different name — "already backed up, just renamed" (capped list).
    pub renamed_matches: Vec<(FileEntry, FileEntry)>,
    /// Total renamed matches found (the list above may be capped).
    pub renamed_total: usize,
    /// Mirror-deletable files (destination-only) whose content exists in the
    /// source under another name: deleting them loses nothing.
    pub doomed_preserved: Vec<(FileEntry, FileEntry)>,
    /// Mirror-deletable files whose content exists NOWHERE in the source:
    /// deleting them is permanent data loss.
    pub doomed_unique: Vec<FileEntry>,
    /// Exact duplicate content within the source folder.
    pub src_dupes: Vec<DupeGroup>,
    /// Exact duplicate content within the destination folder.
    pub dest_dupes: Vec<DupeGroup>,
    /// Source files whose content is not in the destination, bucketed by
    /// month of date-taken (EXIF) or file date: "Oct 2026: 234 photos".
    pub unbacked_by_month: Vec<MonthBucket>,
    /// Byte-weighted content coverage: how much of the source's unique
    /// content the destination already holds, regardless of file names.
    pub covered_bytes: u64,
    pub total_bytes: u64,
    /// Files hashed on each side.
    pub hashed: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonthBucket {
    pub label: String, // "Oct 2026"
    pub files: usize,
    pub bytes: u64,
}

impl DeepCheck {
    /// Files mirror mode would delete.
    pub fn doomed_total(&self) -> usize {
        self.doomed_preserved.len() + self.doomed_unique.len()
    }

    /// Bytes of the unique-content source files not yet backed up.
    pub fn unbacked_bytes(&self) -> u64 {
        self.total_bytes - self.covered_bytes
    }

    /// Duplicate storage wasted within each side, combined.
    pub fn wasted_bytes(&self) -> u64 {
        self.src_dupes.iter().map(|d| d.wasted_bytes).sum::<u64>()
            + self.dest_dupes.iter().map(|d| d.wasted_bytes).sum::<u64>()
    }
}

const RENAMED_CAP: usize = 500;
const DUPE_CAP: usize = 50;

/// Full-hash every file on both sides and derive backup-safety facts.
///
/// Progress is reported through the same [`ScanStatus`] channel as a scan
/// (`Hashing(c, total)`), so the UI needs no new status plumbing.
pub fn deep_check(
    source: std::path::PathBuf,
    dest: std::path::PathBuf,
    mut on_status: impl FnMut(ScanStatus) + Send,
) -> Result<DeepCheck, String> {
    if !source.is_dir() || !dest.is_dir() {
        return Err("Both paths must exist and be folders".into());
    }

    on_status(ScanStatus::ScanningBoth);
    let (source_files, dest_files) = rayon::join(
        || scanner::scan_folder(&source),
        || scanner::scan_folder(&dest),
    );

    // Paths already present on the source side, for the doomed-file split.
    let source_paths: std::collections::HashSet<String> = source_files.keys().cloned().collect();
    let total = source_files.len() + dest_files.len();
    let report = std::sync::Mutex::new(&mut on_status);
    let counter = std::sync::atomic::AtomicUsize::new(0);

    // Hash everything; for source files also capture the month for the
    // "unbacked photos" timeline.
    let hash_side = |files: HashMap<String, FileEntry>, with_month: bool| -> (
        HashMap<String, Vec<FileEntry>>,
        Vec<(FileEntry, String, Option<String>)>,
    ) {
        let hashed: Vec<(FileEntry, String, Option<String>)> = files
            .into_par_iter()
            .map(|(_, entry)| {
                let c = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if c % 25 == 0 || c == total {
                    scanner_status(&report, ScanStatus::Hashing(c, total));
                }
                let hash = scanner::calculate_hash(&entry.path)
                    .unwrap_or_else(|| format!("unreadable:{}", entry.rel_path));
                let month = if with_month { month_of(&entry.path, entry.modified) } else { None };
                (entry, hash, month)
            })
            .collect();

        let mut map: HashMap<String, Vec<FileEntry>> = HashMap::new();
        for (entry, hash, _) in &hashed {
            map.entry(hash.clone()).or_default().push(entry.clone());
        }
        for v in map.values_mut() {
            v.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        }
        (map, hashed)
    };

    let (src_map, src_hashed) = hash_side(source_files, true);
    let (dest_map, _) = hash_side(dest_files, false);
    let dest_hashes: HashMap<&str, &str> = dest_map
        .iter()
        .flat_map(|(hash, v)| v.iter().map(move |e| (e.rel_path.as_str(), hash.as_str())))
        .collect();

    let hashed = src_map.values().map(Vec::len).sum::<usize>()
        + dest_map.values().map(Vec::len).sum::<usize>();

    // Renamed matches: destination content that the source already holds
    // under a different name (and not under its own name as well).
    let mut renamed_matches = Vec::new();
    let mut renamed_total = 0usize;
    for (hash, dest_entries) in &dest_map {
        let Some(src_entries) = src_map.get(hash) else { continue };
        for de in dest_entries {
            if src_entries.iter().any(|se| se.rel_path == de.rel_path) {
                continue; // present under its own name: identical, not renamed
            }
            if let Some(se) = src_entries.iter().find(|se| se.rel_path != de.rel_path) {
                renamed_total += 1;
                if renamed_matches.len() < RENAMED_CAP {
                    renamed_matches.push((se.clone(), de.clone()));
                }
            }
        }
    }

    // Mirror-safety split of the destination-only files (what mirror sync
    // would delete): is their content preserved somewhere in the source?
    let mut doomed_preserved = Vec::new();
    let mut doomed_unique = Vec::new();
    for dest_entries in dest_map.values() {
        for de in dest_entries {
            if source_paths.contains(&de.rel_path) {
                continue; // also in source; mirror does not delete these
            }
            let hash = dest_hashes.get(de.rel_path.as_str()).copied().unwrap_or("");
            match src_map
                .get(hash)
                .and_then(|srcs| srcs.iter().find(|se| se.rel_path != de.rel_path))
            {
                Some(se) => {
                    if doomed_preserved.len() < RENAMED_CAP {
                        doomed_preserved.push((se.clone(), de.clone()));
                    }
                }
                None => doomed_unique.push(de.clone()),
            }
        }
    }
    doomed_unique.sort_by(|a, b| b.size.cmp(&a.size));

    // Exact duplicate content within each side.
    let dupes_of = |map: &HashMap<String, Vec<FileEntry>>| -> Vec<DupeGroup> {
        let mut groups: Vec<DupeGroup> = map
            .iter()
            .filter(|(_, v)| v.len() > 1)
            .map(|(hash, files)| DupeGroup {
                hash: hash.clone(),
                wasted_bytes: files.iter().skip(1).map(|f| f.size).sum(),
                files: files.clone(),
            })
            .collect();
        groups.sort_by(|a, b| b.wasted_bytes.cmp(&a.wasted_bytes));
        groups.truncate(DUPE_CAP);
        groups
    };
    let src_dupes = dupes_of(&src_map);
    let dest_dupes = dupes_of(&dest_map);

    // Unbacked content timeline: source files whose content is nowhere in
    // the destination, bucketed by date-taken (EXIF) or file date.
    let mut buckets: HashMap<String, MonthBucket> = HashMap::new();
    let mut covered_bytes = 0u64;
    let mut total_bytes = 0u64;
    for (entry, hash, month) in src_hashed {
        let present = dest_map.contains_key(&hash);
        total_bytes += entry.size;
        if present {
            covered_bytes += entry.size;
        } else {
            let label = month.unwrap_or_else(|| "Unknown date".into());
            let b = buckets.entry(label.clone()).or_insert_with(|| MonthBucket {
                label,
                files: 0,
                bytes: 0,
            });
            b.files += 1;
            b.bytes += entry.size;
        }
    }
    let mut unbacked_by_month: Vec<MonthBucket> = buckets.into_values().collect();
    unbacked_by_month.sort_by(|a, b| b.label.cmp(&a.label)); // newest first

    Ok(DeepCheck {
        renamed_matches,
        renamed_total,
        doomed_preserved,
        doomed_unique,
        src_dupes,
        dest_dupes,
        unbacked_by_month,
        covered_bytes,
        total_bytes,
        hashed,
    })
}

fn scanner_status<F: FnMut(ScanStatus)>(report: &std::sync::Mutex<F>, status: ScanStatus) {
    if let Ok(mut cb) = report.lock() {
        cb(status);
    }
}

/// Month label for a file: EXIF date-taken when readable, else the file's
/// modification date. `None` when neither yields a sane timestamp.
fn month_of(path: &Path, modified: u64) -> Option<String> {
    if let Some(secs) = exif_date_taken(path) {
        return month_label(secs);
    }
    if modified > 0 {
        return month_label(modified as i64);
    }
    None
}

/// Parse the EXIF DateTimeOriginal/DateTime of an image into a Unix timestamp.
fn exif_date_taken(path: &Path) -> Option<i64> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !["jpg", "jpeg", "tiff", "tif", "dng", "heic"].contains(&ext.as_str()) {
        return None;
    }
    let file = std::fs::File::open(path).ok()?;
    let exif = exif::Reader::new()
        .read_from_container(&mut std::io::BufReader::new(&file))
        .ok()?;
    let field = exif
        .get_field(exif::Tag::DateTimeOriginal, exif::In::PRIMARY)
        .or_else(|| exif.get_field(exif::Tag::DateTime, exif::In::PRIMARY))?;
    // EXIF dates are ASCII like "2026:10:15 08:23:01".
    let Value::Ascii(vec) = &field.value else { return None };
    let dt = exif::DateTime::from_ascii(vec.first()?).ok()?;
    if !(1..=12).contains(&dt.month) || !(1..=31).contains(&dt.day) {
        return None;
    }
    use chrono::TimeZone;
    chrono::Utc
        .with_ymd_and_hms(dt.year as i32, dt.month as u32, dt.day as u32, 12, 0, 0)
        .single()
        .map(|t| t.timestamp())
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

fn month_label(secs: i64) -> Option<String> {
    use chrono::Datelike;
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| {
        format!("{} {}", MONTHS[(dt.month() as usize - 1) % 12], dt.year())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scanner::FileEntry;
    use std::fs;
    use std::path::PathBuf;

    fn entry(rel: &str, size: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(rel),
            rel_path: rel.to_owned(),
            size,
            modified: 0,
            hash: None,
        }
    }

    fn fixture() -> CompareResult {
        CompareResult {
            missing_in_dest: vec![entry("DCIM/IMG_001.jpg", 3_000_000), entry("notes.txt", 100)],
            missing_in_source: vec![entry("DCIM/IMG_002.jpg", 4_000_000), entry("WhatsApp/Media/doc.pdf", 500_000)],
            different_content: vec![(
                entry("DCIM/IMG_003.jpg", 2_000_000),
                entry("DCIM/IMG_003.jpg", 2_000_000),
            )],
            identical: 10,
            identical_bytes: 20_000_000,
            source_count: 13,
            dest_count: 12,
            ..Default::default()
        }
    }

    #[test]
    fn insights_group_by_folder_and_type() {
        let ins = Insights::from_result(&fixture());
        // The most-involved folder comes first and holds the photo counts.
        assert_eq!(ins.folders[0].name, "DCIM");
        assert_eq!(ins.folders[0].new_files, 1);
        assert_eq!(ins.folders[0].extra_files, 1);
        assert_eq!(ins.folders[0].changed_files, 1);
        assert!(ins.folders.iter().any(|f| f.name == "WhatsApp"));
        assert!(ins.folders.iter().any(|f| f.name == "(root)"));

        // Photos dominate the type breakdown by bytes.
        assert_eq!(ins.types[0].label, "Photos");
        assert_eq!(ins.types[0].files, 3);

        // Coverage: 10 identical of 13 source files; byte-weighted over the
        // source's total volume (identical + new + changed).
        assert!((ins.coverage() - 10.0 / 13.0).abs() < 1e-6);
        assert!((ins.coverage_bytes() - 20_000_000.0 / 25_000_100.0).abs() < 0.01);
    }

    #[test]
    fn empty_result_is_fully_covered() {
        let ins = Insights::from_result(&CompareResult {
            identical: 0,
            source_count: 0,
            ..Default::default()
        });
        assert_eq!(ins.coverage(), 1.0);
        assert_eq!(ins.coverage_bytes(), 1.0);
    }

    #[test]
    fn classify_kinds() {
        assert_eq!(classify("DCIM/IMG_1.heic").0, "Photos");
        assert_eq!(classify("DCIM/VID_1.mp4").0, "Videos");
        assert_eq!(classify("doc/report.pdf").0, "Documents");
        assert_eq!(classify("src/main.rs").0, "Code");
        assert_eq!(classify("backup.tar.gz").0, "Archives");
        assert_eq!(classify("mystery.xyz").0, "Other");
        assert_eq!(classify("README").0, "Other");
    }

    #[test]
    fn deep_check_finds_renamed_content_and_doomed_files() {
        let root = std::env::temp_dir().join(format!("omnidiff2_insights_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let dest = root.join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();

        // Same content, different name: already backed up, renamed.
        fs::write(src.join("IMG_0001.jpg"), b"photo content here").unwrap();
        fs::write(dest.join("IMG_20261113_01.jpg"), b"photo content here").unwrap();
        // Content only in the destination: mirror delete loses it forever.
        fs::write(dest.join("unique_video.mp4"), b"unique destination content").unwrap();
        // Content only in the source: not yet backed up.
        fs::write(src.join("new_photo.jpg"), b"brand new photo").unwrap();

        let check = deep_check(src.clone(), dest.clone(), |_| {}).unwrap();

        // The renamed photo is recognized by content.
        assert_eq!(check.renamed_total, 1);
        assert_eq!(check.renamed_matches[0].0.rel_path, "IMG_0001.jpg");
        assert_eq!(check.renamed_matches[0].1.rel_path, "IMG_20261113_01.jpg");

        // Both destination-only files would be deleted by mirror sync, but
        // only the unique video is real data loss — the renamed photo's
        // content survives in the source under IMG_0001.jpg.
        assert_eq!(check.doomed_unique.len(), 1);
        assert_eq!(check.doomed_unique[0].rel_path, "unique_video.mp4");
        assert_eq!(check.doomed_preserved.len(), 1);
        assert_eq!(check.doomed_preserved[0].1.rel_path, "IMG_20261113_01.jpg");

        // The new photo is unbacked content; the renamed one is covered.
        assert_eq!(check.covered_bytes, b"photo content here".len() as u64);
        assert_eq!(check.unbacked_by_month.len(), 1);
        assert_eq!(check.unbacked_by_month[0].files, 1);

        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn deep_check_detects_within_side_duplicates() {
        let root = std::env::temp_dir().join(format!("omnidiff2_dupe_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let src = root.join("src");
        let dest = root.join("dest");
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dest).unwrap();

        fs::write(src.join("a.jpg"), b"duplicate content").unwrap();
        fs::write(src.join("b.jpg"), b"duplicate content").unwrap();
        fs::write(src.join("c.jpg"), b"duplicate content").unwrap();
        fs::write(src.join("solo.txt"), b"unique").unwrap();

        let check = deep_check(src, dest, |_| {}).unwrap();
        assert_eq!(check.src_dupes.len(), 1);
        assert_eq!(check.src_dupes[0].files.len(), 3);
        assert_eq!(check.src_dupes[0].wasted_bytes, b"duplicate content".len() as u64 * 2);
        assert_eq!(check.wasted_bytes(), b"duplicate content".len() as u64 * 2);
    }

    #[test]
    fn month_labels_format() {
        assert_eq!(month_label(1_762_224_000).as_deref(), Some("Nov 2025"));
        assert_eq!(month_label(0).as_deref(), Some("Jan 1970"));
        assert_eq!(month_label(-1).as_deref(), Some("Dec 1969"));
    }

    #[test]
    fn deep_check_rejects_bad_paths() {
        let err = deep_check(PathBuf::from("/nonexistent-omnidiff"), PathBuf::from("/also-nope"), |_| {});
        assert!(err.is_err());
    }
}
