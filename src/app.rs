use eframe::egui;
use std::path::PathBuf;
use std::fs;
use crossbeam_channel::{Receiver, unbounded};
use similar::{ChangeTag, TextDiff};
use std::thread;
use crate::scanner::{self, ScanStatus, CompareResult, FileEntry};
use humansize::{format_size, DECIMAL};
use chrono::DateTime;
use std::time::Duration;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    MissingInDest,
    MissingInSource,
    Different,
}

pub struct FolderCompareApp {
    source: String,
    dest: String,
    check_content: bool,
    
    // State
    status_msg: String,
    is_scanning: bool,
    progress: f32,
    
    // Results
    results: Option<CompareResult>,
    active_tab: Tab,
    
    // Sync logic
    is_syncing: bool,
    delete_extra: bool,
    confirm_sync_open: bool,
    
    // Thread communication
    rx: Option<Receiver<ScanStatus>>,
    result_rx: Option<Receiver<Result<CompareResult, String>>>,

    // Diff View State
    diff_open: bool,
    diff_file_name: String,
    
    // Text Diff
    diff_content: Vec<(String, ChangeTag)>,
    diff_error: Option<String>,
    
    // Image Diff
    diff_mode: DiffMode,
    diff_texture_src: Option<egui::TextureHandle>,
    diff_texture_dest: Option<egui::TextureHandle>,
}

#[derive(PartialEq, Clone, Copy)]
enum DiffMode {
    Text,
    Image,
}

impl Default for FolderCompareApp {
    fn default() -> Self {
        Self {
            source: "".to_owned(),
            dest: "".to_owned(),
            check_content: true,
            status_msg: "Ready".to_owned(),
            is_scanning: false,
            progress: 0.0,
            results: None,
            active_tab: Tab::MissingInDest,
            rx: None,
            result_rx: None,
            is_syncing: false,
            delete_extra: false,
            confirm_sync_open: false,
            diff_open: false,
            diff_file_name: "".to_owned(),
            diff_content: Vec::new(),
            diff_error: None,
            diff_mode: DiffMode::Text,
            diff_texture_src: None,
            diff_texture_dest: None,
        }
    }
}

impl FolderCompareApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Modern Premium Styling
        let mut visuals = egui::Visuals::dark();
        visuals.window_rounding = egui::Rounding::same(12.0);
        visuals.widgets.noninteractive.rounding = egui::Rounding::same(8.0);
        visuals.widgets.active.rounding = egui::Rounding::same(8.0);
        visuals.widgets.inactive.rounding = egui::Rounding::same(8.0);
        visuals.selection.bg_fill = egui::Color32::from_rgb(52, 152, 219); // Premium Blue
        cc.egui_ctx.set_visuals(visuals);
        
        let mut style = (*cc.egui_ctx.style()).clone();
        style.spacing.item_spacing = egui::vec2(10.0, 10.0);
        style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::new(24.0, egui::FontFamily::Proportional));
        cc.egui_ctx.set_style(style);

        Self::default()
    }

    fn start_comparison(&mut self) {
        let source = PathBuf::from(&self.source);
        let dest = PathBuf::from(&self.dest);
        
        if !source.exists() || !dest.exists() {
            self.status_msg = "Error: Paths do not exist".to_owned();
            return;
        }

        self.is_scanning = true;
        self.progress = 0.0;
        self.results = None;
        self.status_msg = "Starting...".to_owned();

        let (tx, rx) = unbounded();
        let (res_tx, res_rx) = unbounded();
        
        self.rx = Some(rx);
        self.result_rx = Some(res_rx);
        
        let check = self.check_content;

        thread::spawn(move || {
            let res = scanner::run_comparison(source, dest, check, tx);
            res_tx.send(res).ok();
        });
    }

    fn start_sync(&mut self) {
        let results = match &self.results {
            Some(r) => r.clone(),
            None => return,
        };
        
        let source = PathBuf::from(&self.source);
        let dest = PathBuf::from(&self.dest);
        let delete_extra = self.delete_extra;

        self.is_syncing = true;
        self.progress = 0.0;
        self.status_msg = "♻️ Starting Sync...".to_owned();

        let (tx, rx) = unbounded();
        self.rx = Some(rx);

        thread::spawn(move || {
            let _ = scanner::run_sync(source, dest, &results, delete_extra, tx);
        });
    }
    
    fn format_time(&self, ts: u64) -> String {
        // Convert timestamp to readable date
        if let Some(dt) = DateTime::from_timestamp(ts as i64, 0) {
            dt.format("%Y-%m-%d %H:%M").to_string()
        } else {
            "-".into()
        }
    }


    fn show_file_list(&self, ui: &mut egui::Ui, files: &[FileEntry]) {
        use egui_extras::{TableBuilder, Column};
        
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::initial(400.0).resizable(true)) // Path
            .column(Column::exact(100.0)) // Size
            .column(Column::remainder()) // Date
            .header(20.0, |mut header| {
                header.col(|ui| { ui.strong("Path"); });
                header.col(|ui| { ui.strong("Size"); });
                header.col(|ui| { ui.strong("Modified"); });
            })
            .body(|mut body| {
                for file in files {
                    body.row(18.0, |mut row| {
                        row.col(|ui| { ui.label(&file.rel_path); });
                        row.col(|ui| { ui.label(format_size(file.size, DECIMAL)); });
                        row.col(|ui| { ui.label(self.format_time(file.modified)); });
                    });
                }
            });
    }
    
    fn show_diff_list(&mut self, ui: &mut egui::Ui, files: &[(FileEntry, FileEntry)]) {
        use egui_extras::{TableBuilder, Column};
        
        TableBuilder::new(ui)
            .striped(true)
            .column(Column::initial(300.0).resizable(true)) // Path
            .column(Column::exact(80.0)) // Src Size
            .column(Column::exact(80.0)) // Dest Size
            .column(Column::remainder()) // Actions
            .header(20.0, |mut header| {
                header.col(|ui| { ui.strong("Path"); });
                header.col(|ui| { ui.strong("Src Size"); });
                header.col(|ui| { ui.strong("Dest Size"); });
                header.col(|ui| { ui.strong("Actions"); });
            })
            .body(|mut body| {
                for (src, dest) in files {
                    body.row(18.0, |mut row| {
                        row.col(|ui| { ui.label(&src.rel_path); });
                        row.col(|ui| { ui.label(format_size(src.size, DECIMAL)); });
                        row.col(|ui| { ui.label(format_size(dest.size, DECIMAL)); });
                        row.col(|ui| { 
                            if ui.button("View Diff").clicked() {
                                self.open_diff_viewer(ui.ctx(), &src.path, &dest.path, &src.rel_path);
                            }
                        });
                    });
                }
            });
    }

    fn open_diff_viewer(&mut self, ctx: &egui::Context, src_path: &PathBuf, dest_path: &PathBuf, name: &str) {
        self.diff_open = true;
        self.diff_file_name = name.to_owned();
        self.diff_error = None;
        self.diff_content.clear();
        
        // Reset image state
        self.diff_texture_src = None;
        self.diff_texture_dest = None;
        self.diff_mode = DiffMode::Text;

        // Check for specific system files
        if name.ends_with(".DS_Store") || name.ends_with("Thumbs.db") {
            self.diff_error = Some("System file (Binary). Comparison skipped.".into());
            return;
        }
        
        // Check for Image
        let ext = name.split('.').last().unwrap_or("").to_lowercase();
        let img_exts = ["png", "jpg", "jpeg", "bmp", "gif", "webp", "ico", "tiff"];
        
        if img_exts.contains(&ext.as_str()) {
            self.diff_mode = DiffMode::Image;
            
            // Helper to load texture
            let load_tex = |path: &PathBuf, label: &str| -> Option<egui::TextureHandle> {
                 let img = image::io::Reader::open(path).ok()?.decode().ok()?;
                 let size = [img.width() as _, img.height() as _];
                 let image_buffer = img.to_rgba8();
                 let pixels = image_buffer.as_flat_samples();
                 let color_image = egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_slice());
                 Some(ctx.load_texture(label, color_image, Default::default()))
            };
            
            self.diff_texture_src = load_tex(src_path, "src_img");
            self.diff_texture_dest = load_tex(dest_path, "dest_img");
            
            if self.diff_texture_src.is_none() || self.diff_texture_dest.is_none() {
                 self.diff_error = Some("Failed to load one or both images.".into());
            }
            return;
        }

        // 1. Try reading as text
        let src_txt = match fs::read_to_string(src_path) {
            Ok(s) => s,
            Err(_) => {
                self.diff_error = Some("Binary file detected (or invalid encoding). Text comparison unavailable.".into());
                return;
            }
        };
        let dest_txt = match fs::read_to_string(dest_path) {
             Ok(s) => s,
            Err(_) => {
                self.diff_error = Some("Binary file detected (or invalid encoding). Text comparison unavailable.".into());
                return;
            }
        };

        let diff = TextDiff::from_lines(&src_txt, &dest_txt);
        
        for change in diff.iter_all_changes() {
            let line = change.value();
            self.diff_content.push((line.trim_end().to_owned(), change.tag()));
        }
    }
}

impl eframe::App for FolderCompareApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll for updates
        if let Some(rx) = &self.rx {
            while let Ok(status) = rx.try_recv() {
                match status {
                    ScanStatus::ScanningSource => { self.status_msg = "📂 Scanning Source...".into(); self.progress = 0.1; },
                    ScanStatus::ScanningDest => { self.status_msg = "📂 Scanning Destination...".into(); self.progress = 0.2; },
                    ScanStatus::ScanningBoth => { self.status_msg = "📂 Scanning Both Folders...".into(); self.progress = 0.15; },
                    ScanStatus::Hashing(current, total) => {
                        self.status_msg = format!("⚡ Verifying Content (Blake3) - {}/{}", current, total);
                        self.progress = 0.4 + (0.6 * (current as f32 / total as f32));
                    },
                    ScanStatus::Syncing(current, total) => {
                        self.status_msg = format!("♻️ Syncing - {}/{} operations", current, total);
                        self.progress = current as f32 / total as f32;
                    },
                    ScanStatus::Complete => { 
                        if self.is_syncing {
                            self.status_msg = "✅ Sync Complete".into();
                            self.is_syncing = false;
                        }
                        self.progress = 1.0; 
                    },
                    ScanStatus::Error(e) => { self.status_msg = format!("❌ Error: {}", e); },
                }
            }
        }
        
        if let Some(rx) = &self.result_rx {
             if let Ok(res) = rx.try_recv() {
                 match res {
                     Ok(data) => {
                         self.results = Some(data);
                         self.status_msg = "✅ Comparison Complete".into();
                     },
                     Err(e) => {
                         self.status_msg = format!("❌ Failed: {}", e);
                     }
                 }
                 self.is_scanning = false;
                 self.rx = None;
                 self.result_rx = None;
             }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // 1. Header
            ui.vertical_centered(|ui| {
                ui.heading("⚡ Folder Compare Pro");
                ui.label(egui::RichText::new("Ultra-fast Rust Engine (Blake3 + Short-Circuit)").color(egui::Color32::GRAY));
            });
            ui.add_space(15.0);

            // 2. Configuration Card
            egui::Frame::group(ui.style())
                .inner_margin(egui::Margin::same(15.0))
                .show(ui, |ui| {
                    ui.label(egui::RichText::new("Configuration").strong());
                    ui.add_space(5.0);
                    
                    egui::Grid::new("inputs_grid").spacing([10.0, 10.0]).striped(false).show(ui, |ui| {
                        // Source
                        ui.label("Source Folder:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.source).desired_width(400.0));
                            if ui.button("📂 Browse").clicked() {
                                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                    self.source = path.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.end_row();

                        // Dest
                        ui.label("Destination Folder:");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut self.dest).desired_width(400.0));
                            if ui.button("📂 Browse").clicked() {
                                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                                    self.dest = path.to_string_lossy().to_string();
                                }
                            }
                        });
                        ui.end_row();
                    });
                    
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut self.delete_extra, "🗑 Delete extra files in destination (Mirror Mode)");
                    });
                    ui.add_space(5.0);
                    ui.label(egui::RichText::new("ℹ️ Deep Content Verification (Blake3 mmap) enabled").small().italics());
                });
            
            ui.add_space(15.0);
            
            // 3. Action Area
            ui.vertical_centered(|ui| {
                let btn = egui::Button::new(egui::RichText::new("🔍 START COMPARISON").size(16.0).strong())
                    .min_size(egui::vec2(200.0, 40.0))
                    .fill(if self.is_scanning { egui::Color32::from_rgb(60, 60, 60) } else { egui::Color32::from_rgb(52, 152, 219) });
                
                if ui.add_enabled(!self.is_scanning, btn).clicked() {
                    self.start_comparison();
                }
                
                ui.add_space(10.0);
                
                if self.is_scanning || self.is_syncing {
                     ui.add(egui::ProgressBar::new(self.progress).show_percentage().animate(true));
                     ui.label(egui::RichText::new(&self.status_msg).strong());
                } else {
                     ui.label(&self.status_msg);

                     if self.results.is_some() {
                         ui.add_space(10.0);
                         let sync_btn = egui::Button::new(egui::RichText::new("⚡ SYNC TO DESTINATION").size(14.0).strong())
                             .min_size(egui::vec2(250.0, 35.0))
                             .fill(egui::Color32::from_rgb(46, 204, 113)); // Premium Green
                         
                         if ui.add(sync_btn).clicked() {
                             if self.delete_extra {
                                 self.confirm_sync_open = true;
                             } else {
                                 self.start_sync();
                             }
                         }
                     }
                }
            });
            
            ui.separator();
            
            // 4. Results Tabs
            if let Some(results) = &self.results {
                ui.horizontal(|ui| {
                    ui.style_mut().spacing.item_spacing.x = 0.0; // Connect tabs
                    
                    let tab_btn = |ui: &mut egui::Ui, text: &str, tab: Tab, active: Tab| {
                        let is_active = tab == active;
                        let btn = egui::Button::new(egui::RichText::new(text).strong().color(
                            if is_active { egui::Color32::WHITE } else { egui::Color32::GRAY }
                        ))
                        .fill(if is_active { egui::Color32::from_rgb(52, 152, 219) } else { egui::Color32::TRANSPARENT })
                        .min_size(egui::vec2(150.0, 30.0));
                        
                        if ui.add(btn).clicked() {
                            Some(tab)
                        } else {
                            None
                        }
                    };

                    if let Some(t) = tab_btn(ui, &format!("Missing in Dest ({})", results.missing_in_dest.len()), Tab::MissingInDest, self.active_tab) {
                        self.active_tab = t;
                    }
                    if let Some(t) = tab_btn(ui, &format!("Extra in Dest ({})", results.missing_in_source.len()), Tab::MissingInSource, self.active_tab) {
                        self.active_tab = t;
                    }
                    if let Some(t) = tab_btn(ui, &format!("Different ({})", results.different_content.len()), Tab::Different, self.active_tab) {
                        self.active_tab = t;
                    }
                });
                
                ui.add_space(10.0);
                
                let active_tab = self.active_tab; // Copy enum
                
                // Clone the data needed for the current view to release the borrow on self.results
                let missing_in_dest = if active_tab == Tab::MissingInDest { Some(results.missing_in_dest.clone()) } else { None };
                let missing_in_source = if active_tab == Tab::MissingInSource { Some(results.missing_in_source.clone()) } else { None };
                let different_content = if active_tab == Tab::Different { Some(results.different_content.clone()) } else { None };

                egui::ScrollArea::vertical().show(ui, |ui| {
                    match active_tab {
                         Tab::MissingInDest => {
                            if let Some(data) = missing_in_dest {
                                self.show_file_list(ui, &data);
                            }
                        },
                        Tab::MissingInSource => {
                            if let Some(data) = missing_in_source {
                                self.show_file_list(ui, &data);
                            }
                        },
                        Tab::Different => {
                             if let Some(data) = different_content {
                                self.show_diff_list(ui, &data);
                            }
                        }
                    }
                });
            }
        });
        
        if self.is_scanning || self.is_syncing {
            ctx.request_repaint_after(Duration::from_millis(16)); // ~60fps throttle
        }

        // Sync Confirmation Modal
        let mut do_sync = false;
        if self.confirm_sync_open {
            egui::Window::new("⚠️ Warning: Destructive Sync")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .show(ctx, |ui| {
                    ui.label("Mirror Mode is enabled. This will PERMANENTLY DELETE files in the destination that do not exist in the source.");
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("PROCEED").clicked() {
                            do_sync = true;
                        }
                        if ui.button("CANCEL").clicked() {
                            self.confirm_sync_open = false;
                        }
                    });
                });
        }
        
        if do_sync {
            self.confirm_sync_open = false;
            self.start_sync();
        }

        // Diff Window Modal
        if self.diff_open {
            egui::Window::new(format!("Diff: {}", self.diff_file_name))
                .open(&mut self.diff_open)
                .default_size([800.0, 600.0])
                .show(ctx, |ui| {
                     if let Some(err) = &self.diff_error {
                         ui.colored_label(egui::Color32::RED, err);
                     } else {

                         // Check Mode
                         if self.diff_mode == DiffMode::Image {
                             // Image Compare View
                             ui.columns(2, |columns| {
                                 columns[0].vertical_centered(|ui| {
                                     ui.label(egui::RichText::new("Source").strong());
                                     if let Some(tex) = &self.diff_texture_src {
                                         ui.image((tex.id(), tex.size_vec2()));
                                     } else {
                                         ui.label("Error loading source image");
                                     }
                                 });
                                 columns[1].vertical_centered(|ui| {
                                     ui.label(egui::RichText::new("Destination").strong());
                                     if let Some(tex) = &self.diff_texture_dest {
                                         ui.image((tex.id(), tex.size_vec2()));
                                     } else {
                                         ui.label("Error loading dest image");
                                     }
                                 });
                             });
                         } else {
                             // Text Diff View
                             egui::ScrollArea::vertical().show(ui, |ui| {
                                 for (line, tag) in &self.diff_content {
                                     let color = match tag {
                                         ChangeTag::Delete => egui::Color32::RED,
                                         ChangeTag::Insert => egui::Color32::GREEN,
                                         ChangeTag::Equal => egui::Color32::GRAY,
                                     };
                                     let prefix = match tag {
                                         ChangeTag::Delete => "- ",
                                         ChangeTag::Insert => "+ ",
                                         ChangeTag::Equal => "  ",
                                     };
                                     ui.colored_label(color, format!("{}{}", prefix, line));
                                 }
                             });
                         }
                     }
                });
        }
    }

}
