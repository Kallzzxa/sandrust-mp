//! NYX Launcher: an eframe/egui front-end for `samp-launcher` (SA:MP 0.3.7 / 0.3.DL-R1).
//!
//! Design follows the NYX Launcher mockup: dark left panel with the NYX
//! logo, "NYX LAUNCHER" title and a blue "Connect To Server" button; city
//! sunset banner on the right; frameless window with rounded corners.
//!
//! Connect goes to play.swdm.my.id:7777 as 0.3.DL-R1.
//! There is no version picker: `samp-launcher` asks the server for its
//! version first and joins as 0.3.7-R4 when the server cannot take a DL
//! client (compat switch), so the same button works for both.
//!
//! The GUI does not launch the game itself: it shells out to
//! `samp-launcher.exe` next to it (same folder) and streams that process's
//! output into the status line (full log: `nyx-launcher.log`). It also
//! points the child at `data\auth_table.txt` next to the exe when the file
//! exists, so the user never has to set `SAMP_RS_AUTH_TABLE` by hand.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, Color32, FontId, Pos2, Rect, RichText, Rounding, Sense, Stroke, Vec2};

/// Server the Connect button joins.
const DEFAULT_HOST: &str = "play.swdm.my.id";
const DEFAULT_PORT: u16 = 7777;
const USERNAME_FILE: &str = "username.txt";
const LOG_FILE: &str = "nyx-launcher.log";

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([860.0, 520.0])
            .with_decorations(false) // Hilangkan title bar bawaan Windows
            .with_transparent(true) // Aktifkan transparansi untuk sudut melengkung
            .with_resizable(false),
        ..Default::default()
    };

    eframe::run_native(
        "NYX Launcher",
        options,
        Box::new(|cc| {
            // Inisialisasi loader gambar (PNG & JPG)
            egui_extras::install_image_loaders(&cc.egui_ctx);
            // Font tebal buat judul (default egui cuma punya font tipis, jadi
            // .strong() pun tetap kelihatan kurus).
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("TitleBold".to_owned(), egui::FontData::from_static(include_bytes!("../assets/TitleBold.ttf")));
            fonts.families.entry(egui::FontFamily::Name("TitleBold".into())).or_default().push("TitleBold".to_owned());
            cc.egui_ctx.set_fonts(fonts);
            Ok(Box::new(NyxLauncher::new()))
        }),
    )
}

fn exe_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

/// Messages from the child-process pump threads to the UI thread.
enum ChildMsg {
    Line(String),
    Exited,
}

struct NyxLauncher {
    username: String,
    status_text: String,
    launching: bool,
    rx: Option<Receiver<ChildMsg>>,
}

impl NyxLauncher {
    fn new() -> Self {
        let username =
            std::fs::read_to_string(exe_dir().join(USERNAME_FILE)).map(|s| s.trim().to_string()).unwrap_or_default();
        Self { username, status_text: String::new(), launching: false, rx: None }
    }

    fn on_connect(&mut self) {
        if self.launching {
            return;
        }
        let name = self.username.trim().to_string();
        if name.is_empty() {
            self.status_text = "Masukkan username terlebih dahulu!".to_string();
            return;
        }
        if name.contains(char::is_whitespace) {
            self.status_text = "Nickname tidak boleh pakai spasi.".to_string();
            return;
        }
        let dir = exe_dir();
        let launcher = dir.join("samp-launcher.exe");
        if !launcher.exists() {
            self.status_text = "samp-launcher.exe tidak ketemu di sebelah launcher.".to_string();
            return;
        }
        let gta = dir.join("gta_sa.exe");
        if !gta.exists() {
            self.status_text = "gta_sa.exe tidak ketemu — taruh launcher ini di folder GTA SA.".to_string();
            return;
        }
        // Remember the nickname for next time.
        let _ = std::fs::write(dir.join(USERNAME_FILE), &name);
        let mut args: Vec<String> = vec![
            "--gta".into(),
            gta.to_string_lossy().into_owned(),
            "--dll".into(),
            dir.join("samp_client.dll").to_string_lossy().into_owned(),
            "--name".into(),
            name.clone(),
            "--host".into(),
            DEFAULT_HOST.into(),
            "--port".into(),
            DEFAULT_PORT.to_string(),
        ];
        // Selalu 0.3.DL-R1: samp-launcher yang nego versi sama server — kalau
        // servernya 0.3.7, dia otomatis switch ke 0.3.7-R4.
        args.push("--dl".into());
        self.status_text = format!("Connecting as {name} to {DEFAULT_HOST}:{DEFAULT_PORT} (0.3.DL-R1).");
        let mut cmd = Command::new(&launcher);
        cmd.args(&args).current_dir(&dir).stdout(Stdio::piped()).stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW: jangan buka console hitam
        }
        let auth_table = dir.join("data").join("auth_table.txt");
        if auth_table.exists() {
            cmd.env("SAMP_RS_AUTH_TABLE", &auth_table);
        }
        match cmd.spawn() {
            Ok(mut child) => {
                self.launching = true;
                let (tx, rx) = mpsc::channel();
                self.rx = Some(rx);
                let log_file = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(LOG_FILE)).ok();
                if let Some(out) = child.stdout.take() {
                    let tx = tx.clone();
                    let log = log_file.as_ref().and_then(|f| f.try_clone().ok());
                    std::thread::spawn(move || pump_log(Box::new(out), tx, log));
                }
                if let Some(err) = child.stderr.take() {
                    let tx = tx.clone();
                    let log = log_file.as_ref().and_then(|f| f.try_clone().ok());
                    std::thread::spawn(move || pump_log(Box::new(err), tx, log));
                }
                std::thread::spawn(move || {
                    let _ = child.wait();
                    let _ = tx.send(ChildMsg::Exited);
                });
            }
            Err(e) => {
                self.status_text = format!("Could not start samp-launcher.exe: {}.", e);
            }
        }
    }
}

/// Stream one child-process pipe to the UI (latest line) and the log file.
fn pump_log(stream: Box<dyn std::io::Read + Send>, tx: mpsc::Sender<ChildMsg>, mut log: Option<File>) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        match line {
            Ok(line) => {
                if let Some(f) = log.as_mut() {
                    let _ = writeln!(f, "{}", line);
                }
                if tx.send(ChildMsg::Line(line)).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
}

impl eframe::App for NyxLauncher {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Background transparan agar corner radius window terlihat mulus
        egui::Rgba::TRANSPARENT.to_array()
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Drain child-process messages (log lines, exit notice).
        let mut msgs = Vec::new();
        if let Some(rx) = &self.rx {
            while let Ok(msg) = rx.try_recv() {
                msgs.push(msg);
            }
        }
        for msg in msgs {
            match msg {
                ChildMsg::Line(line) => {
                    self.status_text = line;
                }
                ChildMsg::Exited => {
                    self.launching = false;
                    self.rx = None;
                    if self.status_text.is_empty() {
                        self.status_text = "launcher exited.".to_string();
                    }
                }
            }
        }
        if self.launching {
            // Keep repainting while the child runs so log lines show up.
            ctx.request_repaint_after(Duration::from_millis(100));
        }

        let panel_frame = egui::Frame::none()
            .fill(Color32::from_rgb(38, 39, 41))
            .rounding(Rounding::same(14.0))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(55, 55, 58)));

        egui::CentralPanel::default().frame(panel_frame).show(ctx, |ui| {
            let full_rect = ui.max_rect();
            let left_width = 330.0;

            // Area drag window (bisa geser window dengan klik-tahan area kosong atas)
            let drag_rect = Rect::from_min_size(full_rect.min, Vec2::new(full_rect.width() - 90.0, 50.0));
            let drag_response = ui.interact(drag_rect, ui.id().with("window_drag"), Sense::drag());
            if drag_response.drag_started() {
                ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
            }

            // ==========================================
            // 1. PANEL KANAN: GAMBAR KOTA LOS SANTOS
            // ==========================================
            let right_rect =
                Rect::from_min_max(Pos2::new(full_rect.min.x + left_width, full_rect.min.y), full_rect.max);

            // Render gambar background kanan dengan sudut kanan melengkung
            egui::Image::new(egui::include_image!("../assets/banner.jpg"))
                .rounding(Rounding { nw: 0.0, ne: 14.0, sw: 0.0, se: 14.0 })
                .paint_at(ui, right_rect);

            // ==========================================
            // 2. PANEL KIRI: KONTROL LAUNCHER
            // ==========================================
            let left_rect = Rect::from_min_size(full_rect.min, Vec2::new(left_width, full_rect.height()));

            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| {
                // Logo NYX kecil di pojok kiri atas
                let logo_rect = Rect::from_min_size(
                    Pos2::new(left_rect.min.x + 16.0, left_rect.min.y + 16.0),
                    Vec2::new(42.0, 42.0),
                );
                egui::Image::new(egui::include_image!("../assets/logo.png")).paint_at(ui, logo_rect);

                ui.vertical_centered(|ui| {
                    ui.add_space(95.0);

                    // Judul: NYX LAUNCHER (font bold embed, bukan default egui yang tipis)
                    ui.label(RichText::new("NYX").font(title_font(32.0)).color(Color32::from_rgb(0, 136, 255)));
                    ui.add_space(-6.0);
                    ui.label(RichText::new("LAUNCHER").font(title_font(30.0)).color(Color32::WHITE));

                    ui.add_space(6.0);
                    ui.label(
                        RichText::new("San Andreas Multiplayer Costum Launcher")
                            .font(FontId::proportional(11.5))
                            .color(Color32::from_rgb(175, 178, 182)),
                    );

                    ui.add_space(55.0);

                    // Input Username dengan efek gradient abu-abu
                    let input_size = Vec2::new(255.0, 44.0);
                    let (input_rect, _) = ui.allocate_exact_size(input_size, Sense::hover());

                    draw_horizontal_gradient(
                        ui.painter(),
                        input_rect,
                        Rounding::same(12.0),
                        Color32::from_rgb(72, 73, 76),
                        Color32::from_rgb(155, 156, 160),
                    );

                    let text_edit_rect = input_rect.shrink2(Vec2::new(14.0, 10.0));
                    ui.allocate_new_ui(egui::UiBuilder::new().max_rect(text_edit_rect), |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.username)
                                .frame(false)
                                .font(FontId::proportional(14.0))
                                .text_color(Color32::WHITE)
                                .hint_text(RichText::new("Username").color(Color32::from_rgb(220, 220, 225))),
                        );
                    });

                    ui.add_space(14.0);

                    // Tombol Connect To Server
                    let btn_size = Vec2::new(255.0, 44.0);
                    let (btn_rect, btn_response) = ui.allocate_exact_size(btn_size, Sense::click());

                    let btn_color = if self.launching {
                        Color32::from_rgb(70, 90, 110)
                    } else if btn_response.hovered() {
                        Color32::from_rgb(25, 150, 255)
                    } else {
                        Color32::from_rgb(8, 132, 232)
                    };

                    ui.painter().rect_filled(btn_rect, Rounding::same(12.0), btn_color);
                    ui.painter().text(
                        btn_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        if self.launching { "Connecting..." } else { "Connect To Server" },
                        title_font(15.0),
                        Color32::WHITE,
                    );

                    if btn_response.clicked() {
                        self.on_connect();
                    }

                    ui.add_space(10.0);

                    if !self.status_text.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            RichText::new(&self.status_text)
                                .font(FontId::proportional(11.5))
                                .color(Color32::from_rgb(100, 190, 255)),
                        );
                    }
                });

                // Footer Credit di bawah panel kiri
                ui.painter().text(
                    Pos2::new(left_rect.center().x, left_rect.max.y - 16.0),
                    egui::Align2::CENTER_CENTER,
                    "Made By Kallzzxa With Love",
                    FontId::proportional(10.5),
                    Color32::from_rgb(90, 92, 96),
                );
            });

            // ==========================================
            // 3. TOMBOL WINDOW CONTROL (MINIMIZE & CLOSE)
            // ==========================================
            let close_rect =
                Rect::from_min_size(Pos2::new(full_rect.max.x - 42.0, full_rect.min.y + 14.0), Vec2::new(28.0, 24.0));
            let min_rect =
                Rect::from_min_size(Pos2::new(full_rect.max.x - 76.0, full_rect.min.y + 14.0), Vec2::new(28.0, 24.0));

            // Tombol Minimize (-)
            let min_resp = ui.interact(min_rect, ui.id().with("min_btn"), Sense::click());
            let min_bg = if min_resp.hovered() {
                Color32::from_rgba_unmultiplied(110, 110, 115, 220)
            } else {
                Color32::from_rgba_unmultiplied(85, 85, 90, 190)
            };
            ui.painter().rect_filled(min_rect, Rounding::same(6.0), min_bg);
            ui.painter().text(
                min_rect.center(),
                egui::Align2::CENTER_CENTER,
                "—",
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            if min_resp.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
            }

            // Tombol Close (X)
            let close_resp = ui.interact(close_rect, ui.id().with("close_btn"), Sense::click());
            let close_bg =
                if close_resp.hovered() { Color32::from_rgb(235, 75, 75) } else { Color32::from_rgb(205, 65, 65) };
            ui.painter().rect_filled(close_rect, Rounding::same(6.0), close_bg);
            ui.painter().text(
                close_rect.center(),
                egui::Align2::CENTER_CENTER,
                "X",
                FontId::proportional(13.0),
                Color32::WHITE,
            );
            if close_resp.clicked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }
}

// Font judul tebal (Liberation Sans Bold, di-embed biar nggak tergantung font Windows).
fn title_font(size: f32) -> FontId {
    FontId::new(size, egui::FontFamily::Name("TitleBold".into()))
}

// Helper untuk menggambar gradien horizontal pada kotak input username
fn draw_horizontal_gradient(
    painter: &egui::Painter,
    rect: Rect,
    rounding: Rounding,
    left_color: Color32,
    right_color: Color32,
) {
    let mut mesh = egui::Mesh::default();
    let steps = 24;

    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        let x = egui::lerp(rect.left()..=rect.right(), t);
        let color = Color32::from_rgba_unmultiplied(
            egui::lerp(left_color.r() as f32..=right_color.r() as f32, t) as u8,
            egui::lerp(left_color.g() as f32..=right_color.g() as f32, t) as u8,
            egui::lerp(left_color.b() as f32..=right_color.b() as f32, t) as u8,
            255,
        );

        mesh.colored_vertex(Pos2::new(x, rect.top()), color);
        mesh.colored_vertex(Pos2::new(x, rect.bottom()), color);

        if i < steps {
            let idx = (i * 2) as u32;
            mesh.add_triangle(idx, idx + 1, idx + 2);
            mesh.add_triangle(idx + 1, idx + 3, idx + 2);
        }
    }

    painter.add(egui::Shape::mesh(mesh));
    // Border halus untuk merapikan sudut melengkung
    painter.rect_stroke(rect, rounding, Stroke::new(1.5_f32, Color32::from_rgb(48, 49, 52)));
}
