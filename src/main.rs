#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod fabric;
mod launcher;
mod mojang;
mod rust_clients;

use eframe::egui;
use launcher::Settings;
use mojang::{lk, Shared};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::oneshot;

#[derive(PartialEq, Clone, Copy)]
enum Tab {
    Play,
    Versions,
    Settings,
    RustClients,
    Console,
}

struct ModEntry {
    path: PathBuf,
    name: String,
    enabled: bool,
}

struct App {
    rt: Option<tokio::runtime::Runtime>,
    handle: tokio::runtime::Handle,
    http: reqwest::Client,
    shared: Arc<Shared>,
    root: PathBuf,
    settings: Settings,
    tab: Tab,
    game_kill: Option<oneshot::Sender<()>>,
    client_kill: Option<oneshot::Sender<()>>,
    installed: Vec<String>,
    mods: Vec<ModEntry>,
    last_scan: Instant,
    version_search: String,
    was_started: bool,
    minimized_by_us: bool,
    notice: String,
}

fn scan_installed(root: &Path) -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(root.join("versions")) {
        for e in rd.flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            if e.path().join(format!("{id}.json")).is_file() {
                v.push(id);
            }
        }
    }
    v.sort();
    v.reverse();
    v
}

fn scan_mods(game_dir: &Path) -> Vec<ModEntry> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir(game_dir.join("mods")) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let lower = name.to_lowercase();
            if lower.ends_with(".jar") {
                v.push(ModEntry { path: e.path(), name, enabled: true });
            } else if lower.ends_with(".jar.disabled") {
                v.push(ModEntry { path: e.path(), name, enabled: false });
            }
        }
    }
    v.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    v
}

fn open_folder(p: &Path) {
    let _ = std::fs::create_dir_all(p);
    let _ = std::process::Command::new("explorer").arg(p).spawn();
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, rt: tokio::runtime::Runtime) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let root = launcher::root_dir();
        let _ = std::fs::create_dir_all(&root);
        let settings = Settings::load(&root);
        let _ = settings.save(&root);
        let handle = rt.handle().clone();
        let http = rt.block_on(async { mojang::http_client() });
        let shared = Shared::new();
        let installed = scan_installed(&root);
        let mods = scan_mods(&settings.game_dir);
        let app = App {
            rt: Some(rt),
            handle,
            http,
            shared,
            root,
            settings,
            tab: Tab::Play,
            game_kill: None,
            client_kill: None,
            installed,
            mods,
            last_scan: Instant::now(),
            version_search: String::new(),
            was_started: false,
            minimized_by_us: false,
            notice: String::new(),
        };
        app.refresh_manifest();
        app
    }

    fn refresh_manifest(&self) {
        let (shared, http, root) = (self.shared.clone(), self.http.clone(), self.root.clone());
        self.handle.spawn(async move {
            match mojang::fetch_manifest(&http, &root).await {
                Ok(m) => {
                    *lk(&shared.manifest) = Some(m);
                    shared.log("[LAUNCHER] список версий загружен");
                }
                Err(e) => shared.log(format!("[ERROR] список версий: {e:#}")),
            }
        });
    }

    fn start_game(&mut self) {
        if self.shared.game_running.load(Ordering::SeqCst) {
            return;
        }
        self.settings.username = self.settings.username.trim().to_string();
        if !launcher::valid_username(&self.settings.username) {
            self.notice = "Ник: 3–16 символов, латиница, цифры и «_»".into();
            return;
        }
        if self.settings.selected_version.is_empty() {
            self.notice = "Выберите версию".into();
            return;
        }
        self.notice.clear();
        let _ = self.settings.save(&self.root);

        let (tx, rx) = oneshot::channel();
        self.game_kill = Some(tx);
        self.shared.game_started.store(false, Ordering::SeqCst);
        self.shared.game_running.store(true, Ordering::SeqCst);

        let (shared, http, settings, root) =
            (self.shared.clone(), self.http.clone(), self.settings.clone(), self.root.clone());
        self.handle.spawn(async move {
            if let Err(e) = launcher::launch_game(shared.clone(), http, settings, root, rx).await {
                shared.log(format!("[ERROR] {e:#}"));
                shared.set_status(format!("Ошибка: {e}"));
            }
            shared.game_started.store(false, Ordering::SeqCst);
            shared.game_running.store(false, Ordering::SeqCst);
        });
    }

    /// Ставит последний стабильный Fabric Loader для выбранной ванильной версии.
    fn install_fabric(&mut self) {
        let mc = self.settings.selected_version.clone();
        let known = lk(&self.shared.manifest)
            .as_ref()
            .map(|m| m.versions.iter().any(|v| v.id == mc))
            .unwrap_or(false);
        if !known {
            self.notice = "Выберите ванильную версию Minecraft из списка".into();
            return;
        }
        self.notice.clear();
        let (shared, http, root) = (self.shared.clone(), self.http.clone(), self.root.clone());
        shared.set_status(format!("Установка Fabric для {mc}…"));
        self.handle.spawn(async move {
            match fabric::install(&http, &root, &mc, None).await {
                Ok(id) => {
                    shared.log(format!("[LAUNCHER] Fabric установлен: {id}"));
                    shared.set_status(format!("Fabric установлен: {id}"));
                    *lk(&shared.pending_select) = Some(id);
                }
                Err(e) => {
                    shared.log(format!("[ERROR] установка Fabric: {e:#}"));
                    shared.set_status("Не удалось установить Fabric");
                }
            }
        });
    }

    fn stop_game(&mut self) {
        if let Some(tx) = self.game_kill.take() {
            let _ = tx.send(());
        }
    }

    fn start_client(&mut self) {
        if self.shared.client_running.load(Ordering::SeqCst) {
            return;
        }
        let Some(profile) = self.settings.rust_clients.get(self.settings.selected_rust_client).cloned() else {
            return;
        };
        if let Err(e) = profile.validate() {
            self.notice = format!("{e}");
            return;
        }
        self.notice.clear();
        let _ = self.settings.save(&self.root);
        let (tx, rx) = oneshot::channel();
        self.client_kill = Some(tx);
        self.shared.client_running.store(true, Ordering::SeqCst);
        let (shared, settings) = (self.shared.clone(), self.settings.clone());
        self.tab = Tab::Console;
        self.handle.spawn(async move {
            if let Err(e) = launcher::launch_rust_client(profile, settings, shared.clone(), rx).await {
                shared.log(format!("[ERROR] {e:#}"));
            }
            shared.client_running.store(false, Ordering::SeqCst);
        });
    }

    // ───────────── Вкладки ─────────────

    fn play_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Играть");
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label("Ник:");
            ui.add(egui::TextEdit::singleline(&mut self.settings.username).desired_width(200.0));
        });
        let auth = launcher::make_auth(&self.settings);
        ui.label(format!(
            "{} UUID: {}",
            if self.settings.offline_mode { "Offline" } else { "Профиль" },
            auth.uuid
        ));
        ui.add_space(6.0);

        let mut list = self.installed.clone();
        if let Some(m) = lk(&self.shared.manifest).as_ref() {
            for v in m.versions.iter().filter(|v| v.kind == "release").take(40) {
                if !list.contains(&v.id) {
                    list.push(v.id.clone());
                }
            }
        }
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Версия")
                .selected_text(if self.settings.selected_version.is_empty() {
                    "—".to_string()
                } else {
                    self.settings.selected_version.clone()
                })
                .width(220.0)
                .show_ui(ui, |ui| {
                    for id in &list {
                        let mark = if self.installed.contains(id) { " (установлена)" } else { "" };
                        ui.selectable_value(&mut self.settings.selected_version, id.clone(), format!("{id}{mark}"));
                    }
                });
        });
        ui.label("Другие версии, снапшоты и моды — на вкладке «Версии и моды».");
        ui.add_space(12.0);

        let running = self.shared.game_running.load(Ordering::SeqCst);
        if !running {
            if ui.add_sized([200.0, 44.0], egui::Button::new("ИГРАТЬ")).clicked() {
                self.start_game();
            }
        } else {
            if ui.add_sized([200.0, 44.0], egui::Button::new("ОСТАНОВИТЬ")).clicked() {
                self.stop_game();
            }
            if !self.shared.game_started.load(Ordering::SeqCst) {
                ui.add_space(6.0);
                ui.add(egui::ProgressBar::new(self.shared.progress()).show_percentage());
            }
        }
        if !self.notice.is_empty() {
            ui.add_space(6.0);
            ui.colored_label(egui::Color32::LIGHT_RED, &self.notice);
        }
    }

    fn versions_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Версии");
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.settings.show_snapshots, "Снапшоты");
            ui.checkbox(&mut self.settings.show_old, "Alpha/Beta");
            if ui.button("Обновить").clicked() {
                self.refresh_manifest();
            }
        });
        ui.horizontal(|ui| {
            ui.label("Поиск:");
            ui.text_edit_singleline(&mut self.version_search);
        });
        ui.label(format!("Выбрана: {}", self.settings.selected_version));
        if ui.button("Установить Fabric для выбранной версии").clicked() {
            self.install_fabric();
        }
        if !self.notice.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &self.notice);
        }
        ui.separator();

        let search = self.version_search.trim().to_lowercase();
        let mut chosen: Option<String> = None;
        let row_h = ui.spacing().interact_size.y + ui.spacing().item_spacing.y;
        let guard = lk(&self.shared.manifest);
        match guard.as_ref() {
            None => {
                ui.label("Список версий ещё не загружен (нет сети?). Установленные версии доступны на вкладке «Играть».");
            }
            Some(m) => {
                let idx: Vec<usize> = m
                    .versions
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| match v.kind.as_str() {
                        "release" => true,
                        "snapshot" => self.settings.show_snapshots,
                        _ => self.settings.show_old,
                    })
                    .filter(|(_, v)| search.is_empty() || v.id.to_lowercase().contains(&search))
                    .map(|(i, _)| i)
                    .collect();
                egui::ScrollArea::vertical().id_salt("versions_scroll").auto_shrink([false; 2]).show_rows(
                    ui,
                    row_h,
                    idx.len(),
                    |ui, range| {
                        for r in range {
                            let v = &m.versions[idx[r]];
                            ui.horizontal(|ui| {
                                ui.label(&v.id);
                                ui.weak(&v.kind);
                                if self.installed.contains(&v.id) {
                                    ui.colored_label(egui::Color32::LIGHT_GREEN, "установлена");
                                }
                                if ui.small_button("Выбрать").clicked() {
                                    chosen = Some(v.id.clone());
                                }
                            });
                        }
                    },
                );
            }
        }
        drop(guard);
        if let Some(c) = chosen {
            self.settings.selected_version = c;
        }
    }

    fn mods_panel(&mut self, ui: &mut egui::Ui) {
        ui.heading("Моды");
        let mods_dir = self.settings.game_dir.join("mods");
        ui.label(format!("Папка: {}", mods_dir.display()));
        ui.horizontal(|ui| {
            if ui.button("Открыть папку").clicked() {
                open_folder(&mods_dir);
            }
            if ui.button("Добавить .jar…").clicked() {
                if let Some(files) = rfd::FileDialog::new().add_filter("Mod", &["jar"]).pick_files() {
                    let _ = std::fs::create_dir_all(&mods_dir);
                    for f in files {
                        if let Some(n) = f.file_name() {
                            let _ = std::fs::copy(&f, mods_dir.join(n));
                        }
                    }
                    self.mods = scan_mods(&self.settings.game_dir);
                }
            }
            if ui.button("Обновить").clicked() {
                self.mods = scan_mods(&self.settings.game_dir);
            }
        });
        ui.weak("Моды работают только с версией-загрузчиком (Fabric/Quilt/Forge), положенной в versions/<id>/<id>.json.");
        ui.separator();

        let mut toggle: Option<usize> = None;
        let mut delete: Option<usize> = None;
        egui::ScrollArea::vertical().id_salt("mods_scroll").auto_shrink([false; 2]).show(ui, |ui| {
            if self.mods.is_empty() {
                ui.label("Модов нет.");
            }
            for (i, m) in self.mods.iter().enumerate() {
                ui.horizontal(|ui| {
                    let mut en = m.enabled;
                    if ui.checkbox(&mut en, "").changed() {
                        toggle = Some(i);
                    }
                    ui.label(m.name.trim_end_matches(".disabled"));
                    if ui.small_button("Удалить").clicked() {
                        delete = Some(i);
                    }
                });
            }
        });
        if let Some(i) = toggle {
            let m = &self.mods[i];
            let new = if m.enabled {
                let mut s = m.path.as_os_str().to_owned();
                s.push(".disabled");
                PathBuf::from(s)
            } else {
                PathBuf::from(m.path.to_string_lossy().trim_end_matches(".disabled").to_string())
            };
            let _ = std::fs::rename(&m.path, new);
            self.mods = scan_mods(&self.settings.game_dir);
        } else if let Some(i) = delete {
            let _ = std::fs::remove_file(&self.mods[i].path);
            self.mods = scan_mods(&self.settings.game_dir);
        }
    }

    fn settings_tab(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.heading("Память");
            ui.add(egui::Slider::new(&mut self.settings.ram_mb, 1024..=16384).step_by(256.0).text("Макс. RAM (-Xmx), МБ"));
            ui.add(egui::Slider::new(&mut self.settings.xms_mb, 256..=4096).step_by(256.0).text("Старт. RAM (-Xms), МБ"));
            ui.add_space(8.0);

            ui.heading("Java");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.settings.java_path).desired_width(420.0));
                if ui.button("Обзор…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().add_filter("java", &["exe"]).pick_file() {
                        self.settings.java_path = p.to_string_lossy().into_owned();
                    }
                }
                if ui.button("Найти").clicked() {
                    self.settings.java_path = launcher::find_java();
                }
            });
            ui.weak("1.17+ требует Java 17, 1.20.5+ — Java 21, старые версии — Java 8.");
            ui.add_space(8.0);

            ui.heading("JVM-флаги");
            ui.add(
                egui::TextEdit::multiline(&mut self.settings.jvm_flags)
                    .desired_rows(6)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace),
            );
            if ui.button("Сбросить на G1GC по умолчанию").clicked() {
                self.settings.jvm_flags = launcher::DEFAULT_JVM_FLAGS.to_string();
            }
            ui.add_space(8.0);

            ui.heading("Аккаунт");
            ui.checkbox(&mut self.settings.offline_mode, "Оффлайн-режим (UUID генерируется из ника)");
            if !self.settings.offline_mode {
                ui.horizontal(|ui| {
                    ui.label("UUID:");
                    ui.text_edit_singleline(&mut self.settings.custom_uuid);
                });
                ui.horizontal(|ui| {
                    ui.label("Access token:");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.access_token).password(true));
                });
            }
            ui.add_space(8.0);

            ui.heading("Папки и поведение");
            ui.horizontal(|ui| {
                ui.label("Папка игры:");
                let mut s = self.settings.game_dir.to_string_lossy().into_owned();
                if ui.add(egui::TextEdit::singleline(&mut s).desired_width(380.0)).changed() {
                    self.settings.game_dir = PathBuf::from(s);
                }
                if ui.button("Обзор…").clicked() {
                    if let Some(p) = rfd::FileDialog::new().pick_folder() {
                        self.settings.game_dir = p;
                    }
                }
                if ui.button("Открыть").clicked() {
                    open_folder(&self.settings.game_dir);
                }
            });
            ui.checkbox(&mut self.settings.minimize_on_launch, "Сворачивать лаунчер после запуска игры");
            ui.checkbox(&mut self.settings.kill_on_exit, "Закрывать игру вместе с лаунчером");
            ui.add_space(8.0);
            if ui.button("Сохранить настройки").clicked() {
                self.notice = match self.settings.save(&self.root) {
                    Ok(()) => "Настройки сохранены".into(),
                    Err(e) => format!("Ошибка сохранения: {e}"),
                };
            }
            if !self.notice.is_empty() {
                ui.label(&self.notice);
            }
        });
    }

    fn clients_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("Нативные Rust-клиенты");
        ui.label("Запуск готовых бинарников (horizon.exe, кастомный воксельный клиент и т.п.). Вывод идёт во вкладку «Консоль».");
        ui.add_space(6.0);

        let names: Vec<String> = self.settings.rust_clients.iter().map(|p| p.name.clone()).collect();
        let (mut add, mut remove) = (false, false);
        ui.horizontal(|ui| {
            egui::ComboBox::from_label("Профиль")
                .selected_text(names.get(self.settings.selected_rust_client).cloned().unwrap_or_default())
                .show_ui(ui, |ui| {
                    for (i, n) in names.iter().enumerate() {
                        ui.selectable_value(&mut self.settings.selected_rust_client, i, n);
                    }
                });
            add = ui.button("+ Профиль").clicked();
            remove = ui.button("Удалить").clicked();
        });
        if add {
            self.settings.rust_clients.push(rust_clients::RustClientProfile {
                name: format!("Клиент {}", names.len() + 1),
                ..Default::default()
            });
            self.settings.selected_rust_client = self.settings.rust_clients.len() - 1;
        }
        if remove && self.settings.rust_clients.len() > 1 {
            self.settings.rust_clients.remove(self.settings.selected_rust_client);
            self.settings.selected_rust_client = 0;
        }
        let idx = self.settings.selected_rust_client.min(self.settings.rust_clients.len().saturating_sub(1));
        self.settings.selected_rust_client = idx;

        if let Some(p) = self.settings.rust_clients.get_mut(idx) {
            ui.horizontal(|ui| {
                ui.label("Название:");
                ui.text_edit_singleline(&mut p.name);
            });
            ui.horizontal(|ui| {
                ui.label("Исполняемый файл:");
                ui.add(egui::TextEdit::singleline(&mut p.exe_path).desired_width(380.0));
                if ui.button("Обзор…").clicked() {
                    if let Some(f) = rfd::FileDialog::new().add_filter("exe", &["exe"]).pick_file() {
                        p.exe_path = f.to_string_lossy().into_owned();
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.label("Аргументы:");
                ui.add(egui::TextEdit::singleline(&mut p.args).desired_width(380.0));
            });
            ui.weak("Подстановки: {username}, {uuid}, {game_dir}");
            ui.horizontal(|ui| {
                ui.label("Рабочая папка:");
                ui.add(egui::TextEdit::singleline(&mut p.working_dir).desired_width(380.0).hint_text("по умолчанию — папка exe"));
            });
        }
        ui.add_space(10.0);

        let running = self.shared.client_running.load(Ordering::SeqCst);
        if !running {
            if ui.add_sized([200.0, 40.0], egui::Button::new("Запустить клиент")).clicked() {
                self.start_client();
            }
        } else if ui.add_sized([200.0, 40.0], egui::Button::new("Остановить клиент")).clicked() {
            if let Some(tx) = self.client_kill.take() {
                let _ = tx.send(());
            }
        }
        if !self.notice.is_empty() {
            ui.colored_label(egui::Color32::LIGHT_RED, &self.notice);
        }
    }

    fn console_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("Очистить").clicked() {
                lk(&self.shared.logs).clear();
            }
            if ui.button("Копировать всё").clicked() {
                let text = lk(&self.shared.logs).join("\n");
                ui.ctx().copy_text(text);
            }
        });
        ui.separator();
        let row_h = ui.text_style_height(&egui::TextStyle::Monospace);
        let logs = lk(&self.shared.logs);
        egui::ScrollArea::both()
            .id_salt("console_scroll")
            .stick_to_bottom(true)
            .auto_shrink([false; 2])
            .show_rows(ui, row_h, logs.len(), |ui, range| {
                for i in range {
                    let line = &logs[i];
                    let color = if line.contains("[ERROR]") || line.contains("/ERROR]") || line.contains("Exception") {
                        egui::Color32::LIGHT_RED
                    } else if line.contains("[WARN]") || line.contains("/WARN]") {
                        egui::Color32::YELLOW
                    } else {
                        ui.visuals().text_color()
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(line).monospace().color(color))
                            .wrap_mode(egui::TextWrapMode::Extend),
                    );
                }
            });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.settings.selected_version.is_empty() {
            if let Some(m) = lk(&self.shared.manifest).as_ref() {
                self.settings.selected_version = m.latest.release.clone();
            }
        }
        let pending = lk(&self.shared.pending_select).take();
        if let Some(id) = pending {
            self.settings.selected_version = id;
            self.installed = scan_installed(&self.root);
        }
        if self.last_scan.elapsed() > Duration::from_secs(2) {
            self.installed = scan_installed(&self.root);
            if self.tab == Tab::Versions {
                self.mods = scan_mods(&self.settings.game_dir);
            }
            self.last_scan = Instant::now();
        }

        // Сворачивание после запуска / восстановление после закрытия игры.
        let started = self.shared.game_started.load(Ordering::SeqCst);
        if started && !self.was_started {
            if self.settings.minimize_on_launch {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true));
                self.minimized_by_us = true;
            } else {
                self.tab = Tab::Console;
            }
        }
        if !started && self.was_started && self.minimized_by_us {
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            self.minimized_by_us = false;
        }
        self.was_started = started;

        egui::TopBottomPanel::top("tabs").show(ctx, |ui| {
            ui.horizontal(|ui| {
                for (t, n) in [
                    (Tab::Play, "Играть"),
                    (Tab::Versions, "Версии и моды"),
                    (Tab::Settings, "Настройки"),
                    (Tab::RustClients, "Rust-клиенты"),
                    (Tab::Console, "Консоль"),
                ] {
                    if ui.selectable_value(&mut self.tab, t, n).clicked() {
                        self.notice.clear();
                        if t == Tab::Versions {
                            self.mods = scan_mods(&self.settings.game_dir);
                        }
                    }
                }
            });
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label(self.shared.status());
                if self.shared.game_running.load(Ordering::SeqCst) && !self.shared.game_started.load(Ordering::SeqCst) {
                    ui.add(egui::ProgressBar::new(self.shared.progress()).desired_width(260.0).show_percentage());
                }
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Play => self.play_tab(ui),
            Tab::Versions => {
                ui.columns(2, |cols| {
                    self.versions_panel(&mut cols[0]);
                    self.mods_panel(&mut cols[1]);
                });
            }
            Tab::Settings => self.settings_tab(ui),
            Tab::RustClients => self.clients_tab(ui),
            Tab::Console => self.console_tab(ui),
        });

        let busy = self.shared.game_running.load(Ordering::SeqCst) || self.shared.client_running.load(Ordering::SeqCst);
        ctx.request_repaint_after(Duration::from_millis(if busy { 50 } else { 250 }));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        let _ = self.settings.save(&self.root);
        if self.settings.kill_on_exit {
            if let Some(tx) = self.game_kill.take() {
                let _ = tx.send(());
            }
        }
        if let Some(tx) = self.client_kill.take() {
            let _ = tx.send(());
        }
        if let Some(rt) = self.rt.take() {
            // Даём задачам время корректно завершить дочерние процессы и освободить ресурсы.
            rt.block_on(async { tokio::time::sleep(Duration::from_millis(500)).await });
            rt.shutdown_timeout(Duration::from_millis(500));
        }
    }
}

fn main() -> eframe::Result<()> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .expect("не удалось запустить tokio");

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("RustLauncher")
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([760.0, 480.0]),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        ..Default::default()
    };

    eframe::run_native("RustLauncher", options, Box::new(move |cc| Ok(Box::new(App::new(cc, rt)))))
}