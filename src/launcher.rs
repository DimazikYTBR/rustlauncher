//! Настройки, сборка командной строки Java и запуск дочерних процессов.

use crate::mojang::{self, Prepared, Shared};
use crate::rust_clients::{self, RustClientProfile};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command;
use tokio::sync::oneshot;

/// G1GC-флаги для минимизации фризов сборщика мусора (набор Aikar, без AlwaysPreTouch для быстрого старта).
pub const DEFAULT_JVM_FLAGS: &str = "-XX:+UseG1GC -XX:+ParallelRefProcEnabled -XX:MaxGCPauseMillis=200 \
-XX:+UnlockExperimentalVMOptions -XX:+DisableExplicitGC -XX:G1NewSizePercent=30 -XX:G1MaxNewSizePercent=40 \
-XX:G1HeapRegionSize=8M -XX:G1ReservePercent=20 -XX:G1HeapWastePercent=5 -XX:G1MixedGCCountTarget=4 \
-XX:InitiatingHeapOccupancyPercent=15 -XX:G1MixedGCLiveThresholdPercent=90 \
-XX:G1RSetUpdatingPauseTimePercent=5 -XX:SurvivorRatio=32 -XX:+PerfDisableSharedMem \
-XX:MaxTenuringThreshold=1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub username: String,
    pub offline_mode: bool,
    pub custom_uuid: String,
    pub access_token: String,
    pub ram_mb: u32,
    pub xms_mb: u32,
    pub java_path: String,
    pub jvm_flags: String,
    pub game_dir: PathBuf,
    pub selected_version: String,
    pub show_snapshots: bool,
    pub show_old: bool,
    pub minimize_on_launch: bool,
    pub kill_on_exit: bool,
    pub rust_clients: Vec<RustClientProfile>,
    pub selected_rust_client: usize,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            username: "Player".to_string(),
            offline_mode: true,
            custom_uuid: String::new(),
            access_token: "0".to_string(),
            ram_mb: 4096,
            xms_mb: 1024,
            java_path: find_java(),
            jvm_flags: DEFAULT_JVM_FLAGS.to_string(),
            game_dir: root_dir().join("game"),
            selected_version: String::new(),
            show_snapshots: false,
            show_old: false,
            minimize_on_launch: true,
            kill_on_exit: true,
            rust_clients: rust_clients::default_profiles(),
            selected_rust_client: 0,
        }
    }
}

impl Settings {
    pub fn load(root: &Path) -> Settings {
        let path = root.join("settings.json");
        let mut s: Settings = std::fs::read(&path)
            .ok()
            .and_then(|d| serde_json::from_slice(&d).ok())
            .unwrap_or_default();
        if s.java_path.trim().is_empty() {
            s.java_path = find_java();
        }
        if s.rust_clients.is_empty() {
            s.rust_clients = rust_clients::default_profiles();
        }
        s
    }

    pub fn save(&self, root: &Path) -> Result<()> {
        std::fs::create_dir_all(root)?;
        std::fs::write(root.join("settings.json"), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }
}

pub fn root_dir() -> PathBuf {
    dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join(".rustlauncher")
}

pub fn valid_username(n: &str) -> bool {
    (3..=16).contains(&n.len()) && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Валидный offline UUID (версия 3, как у ванильного сервера: md5("OfflinePlayer:" + ник)).
pub fn offline_uuid(name: &str) -> String {
    let mut b = md5::compute(format!("OfflinePlayer:{name}").as_bytes()).0;
    b[6] = (b[6] & 0x0f) | 0x30;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = hex::encode(b);
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

/// Поиск javaw.exe: JAVA_HOME, типичные каталоги установки, иначе просто "javaw".
pub fn find_java() -> String {
    if let Ok(home) = std::env::var("JAVA_HOME") {
        let p = Path::new(&home).join("bin").join("javaw.exe");
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    let bases = [
        r"C:\Program Files\Java",
        r"C:\Program Files\Eclipse Adoptium",
        r"C:\Program Files\Microsoft",
        r"C:\Program Files\Zulu",
        r"C:\Program Files\Amazon Corretto",
        r"C:\Program Files\BellSoft",
        r"C:\Program Files (x86)\Java",
    ];
    let mut found: Vec<PathBuf> = Vec::new();
    for b in bases {
        if let Ok(rd) = std::fs::read_dir(b) {
            for e in rd.flatten() {
                let p = e.path().join("bin").join("javaw.exe");
                if p.exists() {
                    found.push(p);
                }
            }
        }
    }
    found.sort();
    found.reverse();
    found
        .into_iter()
        .next()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "javaw".to_string())
}

pub struct Auth {
    pub uuid: String,
    pub token: String,
    pub user_type: &'static str,
}

pub fn make_auth(s: &Settings) -> Auth {
    if s.offline_mode {
        Auth {
            uuid: offline_uuid(&s.username),
            token: "00000000000000000000000000000000".into(),
            user_type: "legacy",
        }
    } else {
        let uuid = if s.custom_uuid.trim().is_empty() {
            offline_uuid(&s.username)
        } else {
            s.custom_uuid.trim().to_string()
        };
        Auth {
            uuid,
            token: s.access_token.clone(),
            user_type: "msa",
        }
    }
}

/// Собирает полный список аргументов для java: JVM-флаги, classpath, mainClass, игровые аргументы.
pub fn build_args(s: &Settings, p: &Prepared, root: &Path, auth: &Auth) -> Vec<String> {
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = p.classpath.iter().map(|x| x.to_string_lossy().into_owned()).collect::<Vec<_>>().join(sep);
    let natives = p.natives_dir.display().to_string();
    let vars: Vec<(&str, String)> = vec![
        ("auth_player_name", s.username.clone()),
        ("version_name", p.json.id.clone()),
        ("game_directory", s.game_dir.display().to_string()),
        ("assets_root", p.assets_dir.display().to_string()),
        ("game_assets", p.assets_dir.display().to_string()),
        ("assets_index_name", p.assets_id.clone()),
        ("auth_uuid", auth.uuid.clone()),
        ("auth_access_token", auth.token.clone()),
        ("auth_session", format!("token:{}:{}", auth.token, auth.uuid)),
        ("user_type", auth.user_type.to_string()),
        ("user_properties", "{}".to_string()),
        ("version_type", p.json.kind.clone().unwrap_or_else(|| "release".to_string())),
        ("natives_directory", natives.clone()),
        ("launcher_name", "RustLauncher".to_string()),
        ("launcher_version", env!("CARGO_PKG_VERSION").to_string()),
        ("classpath", cp.clone()),
        ("classpath_separator", sep.to_string()),
        ("library_directory", root.join("libraries").display().to_string()),
        ("clientid", "0".to_string()),
        ("auth_xuid", "0".to_string()),
    ];
    let sub = |a: &str| -> String {
        let mut o = a.to_string();
        for (k, v) in &vars {
            o = o.replace(&format!("${{{k}}}"), v);
        }
        o
    };

    let mut args = vec![
        format!("-Xmx{}M", s.ram_mb),
        format!("-Xms{}M", s.xms_mb.min(s.ram_mb)),
    ];
    args.extend(s.jvm_flags.split_whitespace().map(String::from));

    if s.offline_mode {
        args.extend([
            "-Dminecraft.api.auth.host=https://nope.invalid".to_string(),
            "-Dminecraft.api.account.host=https://nope.invalid".to_string(),
            "-Dminecraft.api.session.host=https://nope.invalid".to_string(),
            "-Dminecraft.api.services.host=https://nope.invalid".to_string(),
        ]);
    }

    let jvm: Vec<String> = match &p.json.arguments {
        Some(a) if !a.jvm.is_empty() => a.jvm.iter().flat_map(|x| x.resolve()).map(|x| sub(&x)).collect(),
        _ => vec![
            format!("-Djava.library.path={natives}"),
            "-Dminecraft.launcher.brand=RustLauncher".to_string(),
            "-cp".to_string(),
            cp,
        ],
    };
    args.extend(jvm);
    args.push(p.json.main_class.clone());

    let mut game: Vec<String> = match &p.json.arguments {
        Some(a) => a.game.iter().flat_map(|x| x.resolve()).map(|x| sub(&x)).collect(),
        None => Vec::new(),
    };
    if game.is_empty() {
        if let Some(m) = &p.json.minecraft_arguments {
            game = m.split_whitespace().map(|x| sub(x)).collect();
        }
    }
    args.extend(game);
    args
}

/// Запускает процесс, перехватывает stdout/stderr в лог, ждёт завершения или сигнала остановки.
pub async fn run_process(
    mut cmd: Command,
    prefix: &str,
    shared: &Arc<Shared>,
    started: &AtomicBool,
    kill_rx: oneshot::Receiver<()>,
) -> Result<Option<i32>> {
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("не удалось запустить процесс (проверьте путь к программе/Java)")?;
    started.store(true, Ordering::SeqCst);

    let mut pumps = Vec::new();
    if let Some(o) = child.stdout.take() {
        pumps.push(tokio::spawn(pump(o, shared.clone(), prefix.to_string())));
    }
    if let Some(e) = child.stderr.take() {
        pumps.push(tokio::spawn(pump(e, shared.clone(), prefix.to_string())));
    }

    let mut kill_rx = kill_rx;
    let status = tokio::select! {
        s = child.wait() => s?,
        r = &mut kill_rx => {
            if r.is_ok() {
                shared.log("[LAUNCHER] остановка процесса…");
                let _ = child.kill().await;
            }
            child.wait().await?
        }
    };

    for t in pumps {
        let _ = tokio::time::timeout(Duration::from_secs(2), t).await;
    }
    shared.log(format!("[LAUNCHER] процесс завершён, код выхода: {:?}", status.code()));
    Ok(status.code())
}

async fn pump<R: AsyncRead + Unpin>(r: R, shared: Arc<Shared>, prefix: String) {
    let mut rd = BufReader::new(r);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match rd.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {
                let line = String::from_utf8_lossy(&buf);
                shared.log(format!("{prefix}{}", line.trim_end_matches(['\r', '\n'])));
            }
        }
    }
}

/// Полный цикл: подготовка файлов -> сборка аргументов -> запуск Java.
pub async fn launch_game(
    shared: Arc<Shared>,
    http: reqwest::Client,
    settings: Settings,
    root: PathBuf,
    kill_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let id = settings.selected_version.clone();
    if id.is_empty() {
        bail!("версия не выбрана");
    }
    tokio::fs::create_dir_all(&settings.game_dir).await?;

    let java = settings.java_path.trim().to_string();
    if (java.contains('\\') || java.contains('/')) && !Path::new(&java).exists() {
        bail!("Java не найдена по пути: {java} (укажите путь в «Настройки»)");
    }

    shared.log(format!("[LAUNCHER] запуск версии {id}, игрок {}", settings.username));
    let prepared = mojang::prepare_version(&shared, &http, &root, &settings.game_dir, &id).await?;
    if let Some(jv) = &prepared.json.java_version {
        shared.log(format!("[LAUNCHER] для этой версии нужна Java {}+", jv.major_version));
    }

    let auth = make_auth(&settings);
    let args = build_args(&settings, &prepared, &root, &auth);

    let shown: Vec<String> = args
        .iter()
        .map(|a| if auth.token.len() > 1 { a.replace(&auth.token, "***") } else { a.clone() })
        .collect();
    shared.log(format!("[LAUNCHER] {java} {}", shown.join(" ")));
    shared.set_status("Игра запущена");

    let mut cmd = Command::new(&java);
    cmd.args(&args).current_dir(&settings.game_dir);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    run_process(cmd, "", &shared, &shared.game_started, kill_rx).await?;
    shared.set_status("Игра закрыта");
    Ok(())
}

/// Тонкая обёртка для вкладки «Rust-клиенты».
pub async fn launch_rust_client(
    profile: RustClientProfile,
    settings: Settings,
    shared: Arc<Shared>,
    kill_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let auth = make_auth(&settings);
    rust_clients::launch_client(profile, settings.username.clone(), auth.uuid, settings.game_dir.clone(), shared, kill_rx).await
}