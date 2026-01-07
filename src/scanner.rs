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

/// Short-circuit hashing: first 16KB and last 16KB
pub fn calculate_partial_hash(path: &Path) -> Option<[u8; 32]> {
    let mut file = File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0; 16384];

    // Read head
    let head_count = file.read(&mut buffer).ok()?;
    hasher.update(&buffer[..head_count]);

    // Read tail if file is large enough to have a separate tail
    if len > 32768 {
        file.seek(SeekFrom::End(-16384)).ok()?;
        let tail_count = file.read(&mut buffer).ok()?;
        hasher.update(&buffer[..tail_count]);
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

                // Stage 1: Head/Tail Short-circuit
                let src_partial = calculate_partial_hash(&src.path)?;
                let dest_partial = calculate_partial_hash(&dest.path)?;
                
                if src_partial != dest_partial {
                    return Some((src.clone(), dest.clone()));
                }

                // Stage 2: Full content verify if partial match
                let src_hash = calculate_hash(&src.path)?;
                let dest_hash = calculate_hash(&dest.path)?;

                if src_hash != dest_hash {
                    let mut src_clone = src.clone();
                    src_clone.hash = Some(src_hash);
                    let mut dest_clone = dest.clone();
                    dest_clone.hash = Some(dest_hash);
                    Some((src_clone, dest_clone))
                } else {
                    None
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

    // Run Copy/Update in Parallel
    tasks.into_par_iter().for_each(|(from, to, _)| {
        let c = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if c % 10 == 0 || c == total {
            tx.send(ScanStatus::Syncing(c, total)).ok();
        }

        // Ensure parent directory exists
        if let Some(parent) = to.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        
        let _ = std::fs::copy(from, to);
    });

    // Run Deletions in Parallel (if any)
    delete_tasks.into_par_iter().for_each(|path| {
        let c = counter.fetch_add(1, Ordering::Relaxed) + 1;
        if c % 10 == 0 || c == total {
            tx.send(ScanStatus::Syncing(c, total)).ok();
        }
        let _ = std::fs::remove_file(path);
    });

    tx.send(ScanStatus::Complete).ok();
    Ok(())
}
