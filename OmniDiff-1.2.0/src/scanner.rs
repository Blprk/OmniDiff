use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use rayon::prelude::*;
use walkdir::WalkDir;
use crossbeam_channel::Sender;
use memmap2::Mmap;

#[derive(Debug, Clone)]
pub struct FileEntry {
    pub path: PathBuf,       // Full path
    pub rel_path: String,    // Relative path key
    pub size: u64,
    pub modified: u64,       // Timestamp
    pub hash: Option<String>,
}

#[derive(Debug, Clone)]
pub enum ScanStatus {
    ScanningSource,
    ScanningDest,
    ScanningBoth,
    Hashing(usize, usize), // current, total
    Syncing(usize, usize), // current, total
    Complete,
    Error(String),
}

#[derive(Debug, Clone, Default)]
pub struct CompareResult {
    pub missing_in_dest: Vec<FileEntry>,
    pub missing_in_source: Vec<FileEntry>,
    pub different_content: Vec<(FileEntry, FileEntry)>, // (Source, Dest)
}

/// Read as many bytes as available up to buffer.len(). `File::read` is not
/// guaranteed to fill the buffer in a single call, so we loop.
fn read_up_to<'a>(file: &mut File, buffer: &'a mut [u8]) -> std::io::Result<&'a [u8]> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(&buffer[..filled])
}

/// Short-circuit hashing: first 16KB and last 16KB
pub fn calculate_partial_hash(path: &Path) -> Option<[u8; 32]> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 16384];

    // Read head
    let head = read_up_to(&mut file, &mut buffer).ok()?;
    hasher.update(head);

    // Read tail if file is large enough to have a separate tail
    if len > 32768 {
        file.seek(SeekFrom::End(-16384)).ok()?;
        let tail = read_up_to(&mut file, &mut buffer).ok()?;
        hasher.update(tail);
    }

    Some(hasher.finalize().into())
}

/// Full hashing using memory mapping for maximum throughput
pub fn calculate_hash(path: &Path) -> Option<String> {
    let file = File::open(path).ok()?;
    let mmap = unsafe { Mmap::map(&file).ok()? };
    let hash = blake3::hash(&mmap);
    Some(hash.to_hex().to_string())
}

pub fn scan_folder(root: &Path) -> HashMap<String, FileEntry> {
    WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .par_bridge()
        .filter_map(|entry| {
            let path = entry.path().to_path_buf();
            let metadata = entry.metadata().ok()?;
            let size = metadata.len();
            let modified = metadata.modified().unwrap_or(UNIX_EPOCH)
                .duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();

            let rel_path = path.strip_prefix(&root).ok()?.to_string_lossy().to_string();

            Some((rel_path.clone(), FileEntry {
                path,
                rel_path,
                size,
                modified,
                hash: None,
            }))
        })
        .collect()
}

pub fn run_comparison(
    source: PathBuf,
    dest: PathBuf,
    check_content: bool,
    tx: Sender<ScanStatus>
) -> Result<CompareResult, String> {
    // 1. Parallel Scanning
    tx.send(ScanStatus::ScanningBoth).ok();
    let (source_files, dest_files) = rayon::join(
        || scan_folder(&source),
        || scan_folder(&dest)
    );

    // 2. Identify candidates for comparison
    let mut missing_in_dest = Vec::new();
    let mut missing_in_source = Vec::new();
    let mut common_files = Vec::new();

    for (rel_path, src_entry) in &source_files {
        if let Some(dest_entry) = dest_files.get(rel_path) {
            common_files.push((src_entry, dest_entry));
        } else {
            missing_in_dest.push(src_entry.clone());
        }
    }

    for (rel_path, dest_entry) in &dest_files {
        if !source_files.contains_key(rel_path) {
            missing_in_source.push(dest_entry.clone());
        }
    }

    let mut different_content = Vec::new();

    if check_content {
        let same_size_candidates: Vec<_> = common_files.into_iter()
            .filter(|(src, dest)| {
                if src.size != dest.size {
                    different_content.push(((*src).clone(), (*dest).clone()));
                    false
                } else {
                    true
                }
            })
            .collect();

        let total_hash = same_size_candidates.len();
        let counter = Arc::new(AtomicUsize::new(0));
        
        let hashed_diffs: Vec<_> = same_size_candidates.into_par_iter()
            .filter_map(|(src, dest)| {
                let c = counter.fetch_add(1, Ordering::Relaxed) + 1;
                if c % 50 == 0 || c == total_hash {
                    tx.send(ScanStatus::Hashing(c, total_hash)).ok();
                }

                // Hash both sides. Returns Some(differs) on success, None when
                // a file could not be opened/read/hashed.
                let hash_result = (|| -> Option<bool> {
                    // Stage 1: Head/Tail Short-circuit
                    let src_partial = calculate_partial_hash(&src.path)?;
                    let dest_partial = calculate_partial_hash(&dest.path)?;

                    if src_partial != dest_partial {
                        return Some(true);
                    }

                    // Stage 2: Full content verify if partial match
                    let src_hash = calculate_hash(&src.path)?;
                    let dest_hash = calculate_hash(&dest.path)?;

                    Some(src_hash != dest_hash)
                })();

                match hash_result {
                    // Identical content.
                    Some(false) => None,
                    // Different content: record the full hashes for the diff viewer.
                    Some(true) => {
                        let mut src_clone = src.clone();
                        let mut dest_clone = dest.clone();
                        if let Some(h) = calculate_hash(&src.path) { src_clone.hash = Some(h); }
                        if let Some(h) = calculate_hash(&dest.path) { dest_clone.hash = Some(h); }
                        Some((src_clone, dest_clone))
                    }
                    // A file we could not read must be flagged as different,
                    // never silently treated as identical (otherwise sync would
                    // skip it and claim the folders match).
                    None => Some((src.clone(), dest.clone())),
                }
            })
            .collect();
            
        different_content.extend(hashed_diffs);
    } else {
        // Shallow comparison
        for (src, dest) in common_files {
            if src.size != dest.size || src.modified != dest.modified {
                 different_content.push((src.clone(), dest.clone()));
            }
        }
    }
    
    tx.send(ScanStatus::Complete).ok();

    Ok(CompareResult {
        missing_in_dest,
        missing_in_source,
        different_content,
    })
}

pub fn run_sync(
    _source_root: PathBuf,
    dest_root: PathBuf,
    results: &CompareResult,
    delete_extra: bool,
    tx: Sender<ScanStatus>
) -> Result<(), String> {
    let mut tasks = Vec::new();

    // 1. Prepare Copy Tasks (Missing in Dest)
    for entry in &results.missing_in_dest {
        let dest_path = dest_root.join(&entry.rel_path);
        tasks.push((entry.path.clone(), dest_path, true)); // (from, to, is_copy)
    }

    // 2. Prepare Update Tasks (Different Content)
    for (src, _dest) in &results.different_content {
        let dest_path = dest_root.join(&src.rel_path);
        tasks.push((src.path.clone(), dest_path, true));
    }

    // 3. Prepare Delete Tasks (Extra in Dest - Optional)
    let mut delete_tasks = Vec::new();
    if delete_extra {
        for entry in &results.missing_in_source {
            delete_tasks.push(entry.path.clone());
        }
    }

    let total = tasks.len() + delete_tasks.len();
    let counter = AtomicUsize::new(0);
    let failures = AtomicUsize::new(0);

    // Run Copy/Update in Parallel
    tasks.into_par_iter().for_each(|(from, to, _)| {
        let c = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if c % 10 == 0 || c == total {
            tx.send(ScanStatus::Syncing(c, total)).ok();
        }

        // Ensure parent directory exists
        if let Some(parent) = to.parent() {
            if let Err(_) = std::fs::create_dir_all(parent) {
                failures.fetch_add(1, Ordering::Relaxed);
                return;
            }
        }

        if std::fs::copy(&from, &to).is_err() {
            failures.fetch_add(1, Ordering::Relaxed);
        }
    });

    // Run Deletions in Parallel (if any)
    delete_tasks.into_par_iter().for_each(|path| {
        let c = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if c % 10 == 0 || c == total {
            tx.send(ScanStatus::Syncing(c, total)).ok();
        }
        if std::fs::remove_file(&path).is_err() {
            failures.fetch_add(1, Ordering::Relaxed);
        }
    });

    let failure_count = failures.load(Ordering::Relaxed);
    if failure_count > 0 {
        tx.send(ScanStatus::Error(format!(
            "Sync finished with {} failed file operation(s)",
            failure_count
        ))).ok();
    } else {
        tx.send(ScanStatus::Complete).ok();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("omnidiff_test_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write_file(root: &Path, rel: &str, data: &[u8]) -> PathBuf {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, data).unwrap();
        path
    }

    #[test]
    fn detects_missing_and_different_files() {
        let src = temp_root("src");
        let dest = temp_root("dest");

        write_file(&src, "same.txt", b"hello");
        write_file(&src, "diff.txt", b"version one");
        write_file(&src, "only_src.txt", b"src only");
        write_file(&dest, "same.txt", b"hello");
        write_file(&dest, "diff.txt", b"version two");
        write_file(&dest, "only_dest.txt", b"dest only");

        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = run_comparison(src.clone(), dest.clone(), true, tx).unwrap();

        assert_eq!(result.missing_in_dest.len(), 1);
        assert_eq!(result.missing_in_dest[0].rel_path, "only_src.txt");
        assert_eq!(result.missing_in_source.len(), 1);
        assert_eq!(result.missing_in_source[0].rel_path, "only_dest.txt");
        assert_eq!(result.different_content.len(), 1);
        assert_eq!(result.different_content[0].0.rel_path, "diff.txt");

        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn identical_folders_have_no_differences() {
        let src = temp_root("same_src");
        let dest = temp_root("same_dest");

        let mut data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        write_file(&src, "big.bin", &data);
        // Mutate a byte in the tail region (beyond the 16KB head) to exercise
        // the head+tail short-circuit plus the full-hash fallback.
        data[150_000] ^= 0xFF;
        write_file(&dest, "big.bin", &data);

        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = run_comparison(src.clone(), dest.clone(), true, tx).unwrap();
        assert_eq!(result.different_content.len(), 1, "tail-region change must be detected");

        // Now make them truly identical and re-compare.
        fs::copy(src.join("big.bin"), dest.join("big.bin")).unwrap();
        let (tx2, _rx2) = crossbeam_channel::unbounded();
        let result2 = run_comparison(src.clone(), dest.clone(), true, tx2).unwrap();
        assert_eq!(result2.different_content.len(), 0);

        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn unreadable_file_is_flagged_as_different() {
        if std::env::var("USER").unwrap_or_default() == "root" {
            return; // root can read anything; the test would not be meaningful
        }
        let src = temp_root("perm_src");
        let dest = temp_root("perm_dest");

        write_file(&src, "ok.txt", b"fine");
        write_file(&dest, "ok.txt", b"fine");
        write_file(&src, "locked.txt", b"secret");
        let locked = write_file(&dest, "locked.txt", b"secret");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let (tx, _rx) = crossbeam_channel::unbounded();
        let result = run_comparison(src.clone(), dest.clone(), true, tx).unwrap();
        assert_eq!(
            result.different_content.len(), 1,
            "unreadable file must be flagged, not silently treated as identical"
        );

        // Restore permissions so cleanup succeeds.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o644)).unwrap();
        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn sync_copies_updates_and_deletes() {
        let src = temp_root("sync_src");
        let dest = temp_root("sync_dest");

        write_file(&src, "a.txt", b"aaa");
        write_file(&src, "sub/b.txt", b"bbb");
        write_file(&dest, "a.txt", b"stale");
        write_file(&dest, "orphan.txt", b"delete me");

        let (tx, _rx) = crossbeam_channel::unbounded();
        let results = run_comparison(src.clone(), dest.clone(), true, tx).unwrap();
        assert_eq!(results.different_content.len(), 1); // a.txt stale content
        assert_eq!(results.missing_in_dest.len(), 1); // sub/b.txt

        let (tx2, _rx2) = crossbeam_channel::unbounded();
        run_sync(src.clone(), dest.clone(), &results, true, tx2).unwrap();

        assert_eq!(fs::read(dest.join("a.txt")).unwrap(), b"aaa");
        assert_eq!(fs::read(dest.join("sub/b.txt")).unwrap(), b"bbb");
        assert!(!dest.join("orphan.txt").exists(), "mirror mode should delete extras");

        // After a sync, a fresh comparison must report zero differences.
        let (tx3, _rx3) = crossbeam_channel::unbounded();
        let verified = run_comparison(src.clone(), dest.clone(), true, tx3).unwrap();
        assert_eq!(verified.different_content.len(), 0);
        assert_eq!(verified.missing_in_dest.len(), 0);
        assert_eq!(verified.missing_in_source.len(), 0);

        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dest).unwrap();
    }

    #[test]
    fn partial_hash_detects_tail_changes() {
        let src = temp_root("ph_src");
        let dest = temp_root("ph_dest");

        let mut data = vec![7u8; 40_000];
        write_file(&src, "f.bin", &data);
        // Change a byte in the tail window only (last 16KB).
        let n = data.len();
        data[n - 1] ^= 0xFF;
        write_file(&dest, "f.bin", &data);

        let a = calculate_partial_hash(&src.join("f.bin")).unwrap();
        let b = calculate_partial_hash(&dest.join("f.bin")).unwrap();
        assert_ne!(a, b);

        fs::remove_dir_all(&src).unwrap();
        fs::remove_dir_all(&dest).unwrap();
    }
}
