# Folder Compare (Rust Rewrite)

This is a complete rewrite of the Folder Compare tool in Rust, focusing on extreme performance and memory safety.

## 🚀 Performance Features

- **Parallel Directory Walking**: Uses `rayon` and `walkdir` to scan files on multiple threads.
- **Parallel Hashing**: Computes MD5 hashes of multiple files simultaneously.
- **Buffered I/O**: Efficient file reading with large buffers.
- **Native GUI**: Uses `eframe` (egui) for a lightweight, GPU-accelerated interface.

## 🛠 Prerequisites

You need to have **Rust** installed on your machine.

1. **Install Rust**:
   Open a terminal and run:
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```
   Follow the on-screen instructions (default installation is fine).
   
2. **Restart your Shell**:
   Close and open your terminal, or notify the system of the path changes:
   ```bash
   source "$HOME/.cargo/env"
   ```

## 🏃‍♂️ How to Run

1. Navigate to this directory:
   ```bash
   cd rust_rewrite
   ```

2. Run the application (optimized release mode recommended):
   ```bash
   cargo run --release
   ```
   
   *Note: The first run will take a minute to compile dependencies.*

## 📦 Building an Executable

To build a standalone app (binary):

```bash
cargo build --release
```

The binary will be located at `target/release/folder_compare_rust`.

## 🏗 Project Structure

- `src/main.rs`: Entry point.
- `src/app.rs`: GUI implementation (tabs, tables, event loop).
- `src/scanner.rs`: The high-performance core logic.
