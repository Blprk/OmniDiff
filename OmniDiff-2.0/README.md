# 🚀 OmniDiff Pro 2.0

**OmniDiff** is an ultra-fast, industrial-grade folder comparison and synchronization tool built in Rust. It utilizes a parallel engine and the Blake3 hashing algorithm for 10x the performance of traditional diff tools.

Version 2.0 rebuilds the entire interface on [Rune](https://github.com/anomalyco/rune), Anomaly's GPU-rendered native UI framework — the same engine that powers a production diff viewer — while keeping the proven 1.x compare/sync engine.

[![GitHub license](https://img.shields.io/github/license/Blprk/OmniDiff)](https://github.com/Blprk/OmniDiff/blob/main/LICENSE)
[![GitHub release](https://img.shields.io/github/release/Blprk/OmniDiff.svg)](https://github.com/Blprk/OmniDiff/releases)

---

## ✨ What's new in 2.0

- **🎨 All-new GPU-rendered UI** (Rune): one instanced draw call per frame, spring-animated everything, light/dark themes.
- **🛡 Backup-safety check** (built for photo-backup workflows): full-Blake3-hashes every file on both sides and finds **renamed photos** (same content, different name), **exact duplicates** within each folder, and exactly what a mirror sync would destroy — split into *safe to delete* (content survives in the source under another name) and **permanent data loss**.
- **📊 Insights panel** — instant, rule-based analysis: backup coverage (% of files and % of content already in the destination), folder breakdown of where the changes live, file-type breakdown, and the largest new/changed files. No LLM, no guessing — just arithmetic over the hashes. **Everything is clickable**: click a folder or file type to filter the results list, click a file to reveal it in Finder.
- **📅 Photo timeline** — photos not yet backed up, bucketed by date taken (EXIF) or file date: "Oct 2026 (234 files · 1.2 GB)".
- **📁 Folder grouping** — collapsible per-folder group rows in the results table.
- **🔍 Review-grade diff viewer**: split + unified modes, **word-level change highlighting**, full syntax highlighting (80+ languages), hunk folding, keyboard navigation (`j`/`k` hunks, Escape closes).
- **🗂 Virtualized tables**: scroll 100,000+ files at sub-millisecond frame times, with zebra striping and keyboard navigation.
- **👁 Show in Finder** — every row and both sides of a diff viewer have a one-click *reveal in Finder* (with the file selected).
- **🖱 Native folder pickers** integrated with the UI framework's dialog pipeline (no UI freezes while browsing).
- **⚠️ Honest mirror-sync dialog** — states the exact copy/delete counts, bytes to be deleted, and the backup check's data-loss verdict before you confirm.
- **⚙️ Settings** (gear in the title bar): theme (**Follow system / Dark / Light**), fast size-&-date comparison mode, **preserve modification dates when copying** (photo libraries keep sorting by capture date), auto-open the Insights panel, remember last used folders. Saved to `~/Library/Application Support/OmniDiff/settings.json`.
- **⏱ Live progress with ETA** — real completion percentage during hashing and sync, with elapsed time and an ETA estimated from the measured hashing/copy rate.
- **✅ Headless testing**: the UI renders to PNG off-screen, so releases are verified by automated snapshots and scripted click-throughs.
- **🛡 All 1.2 correctness fixes included**: unreadable files are flagged instead of silently treated as identical, sync failures are reported, partial-hash reads are full-read correct, nested folders are rejected.

## 🚀 Performance

| Operation | OmniDiff (Rust) | Standard Tools | Speedup |
| :--- | :--- | :--- | :--- |
| **10,000 files scan** | < 0.5s | ~5-8s | **~10x** |
| **1GB Content Check** | ~0.8s | ~12s | **~15x** |

---

## 💻 Download & Install

### For macOS Users (Apple Silicon)
1. Download `OmniDiff_v2.0.0_macOS.zip` from the [Releases](https://github.com/Blprk/OmniDiff/releases) page.
2. Unzip and move `OmniDiff.app` to your **Applications** folder.
3. Right-click → **Open** the first time (the app is ad-hoc signed, not notarized).
4. Prefer the classic interface? **OmniDiff v1.2.0** (egui) remains available on the Releases page — both versions can be installed side by side.

### For Developers (Build from Source)
```bash
git clone https://github.com/Blprk/OmniDiff.git
cd OmniDiff/OmniDiff-2.0
cargo build --release
./bundle_app.sh
```
Rune is vendored under `rune/` at the exact dev rev the app is developed against, together with Rune's patched `winit`, `wgpu-hal` and `parley_engine` — no network lookups, reproducible builds.

## 🧪 Testing

```bash
cargo test                        # engine + insights regression tests (12)
./target/release/omnidiff -- --scene backup   --snap backup.png   --theme dark
./target/release/omnidiff -- --scene insights  --snap insights.png --theme dark
./target/release/omnidiff -- --scene diff      --snap diff.png     --theme dark
```
Scenes: `results`, `diff`, `confirm`, `insights`, `backup`, `collapsed`, `e2e` (scripted click-through of a real comparison, including the insights panel and the backup-safety check).

---

## 🛠 Tech Stack

- **Core**: Rust (pinned 1.99.1 toolchain)
- **UI**: [Rune](https://github.com/anomalyco/rune) + rune-kit (vendored)
- **Hashing**: Blake3 (SIMD accelerated)
- **Parallelism**: Rayon
- **I/O**: memmap2 (zero-copy)

---

## 📄 License

Distributed under the MIT License. See `LICENSE` for more information.

---
*Developed with ❤️ by Blprk*
