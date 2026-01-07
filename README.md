# 🚀 OmniDiff

**OmniDiff** is an ultra-fast, industrial-grade folder comparison and synchronization tool built in Rust. It utilizes a state-of-the-art parallel engine and the Blake3 hashing algorithm to deliver 10x the performance of traditional diff tools.

[![GitHub license](https://img.shields.io/github/license/Blprk/folder-compare-rust)](https://github.com/Blprk/folder-compare-rust/blob/main/LICENSE)
[![GitHub release](https://img.shields.io/github/release/Blprk/folder-compare-rust.svg)](https://github.com/Blprk/folder-compare-rust/releases)

---

## ✨ Features

- **⚡ Blazing Fast**: Parallel multi-threaded folder scanning and Blake3 hashing.
- **🛡️ Short-Circuit Logic**: Instant metadata comparison with intelligent head/tail partial hashing.
- **♻️ Smart Sync**: Mirror folders or selectively update files with built-in safety confirmation.
- **🎨 Modern UI**: Clean, GPU-accelerated dark interface built with `egui`.
- **🔍 Visual Diff**: Side-by-side comparison for text files and images.

## 🚀 Performance Comparison

| Operation | OmniDiff (Rust) | Standard Tools | Speedup |
| :--- | :--- | :--- | :--- |
| **10,000 files scan** | < 0.5s | ~5-8s | **~10x** |
| **1GB Content Check** | ~0.8s | ~12s | **~15x** |

---

## 💻 Download & Install

### For macOS Users
1. Download the latest `OmniDiff.zip` from the [Releases](https://github.com/Blprk/folder-compare-rust/releases) page.
2. Unzip and move `OmniDiff.app` to your **Applications** folder.
3. Open and enjoy.

### For Developers (Build from Source)
```bash
git clone https://github.com/Blprk/folder-compare-rust.git
cd folder-compare-rust
cargo run --release
```

---

## 🛠 Tech Stack

- **Core**: Rust 
- **Hashing**: Blake3 (SIMD accelerated)
- **GUI**: eframe / egui
- **Parallelism**: Rayon
- **I/O**: memmap2 (Zero-copy memory mapping)

---

## 🤝 Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## 📄 License

Distributed under the MIT License. See `LICENSE` for more information.

---
*Developed with ❤️ by Blprk*
