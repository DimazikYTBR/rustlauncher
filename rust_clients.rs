//! Профили для запуска нативных Rust-клиентов (например, horizon.exe или воксельный клиент).

use crate::launcher;
use crate::mojang::Shared;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::process::Command;
use tokio::sync::oneshot;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct RustClientProfile {
    pub name: String,
    /// Путь к исполняемому файлу клиента.
    pub exe_path: String,
    /// Аргументы; доступны подстановки {username}, {uuid}, {game_dir}. Кавычки "…" поддерживаются.
    pub args: String,
    /// Рабочая папка; если пусто — папка с exe.
    pub working_dir: String,
}

pub fn default_profiles() -> Vec<RustClientProfile> {
    vec![RustClientProfile {
        name: "Horizon".to_string(),
        exe_path: String::new(),
        args: "--username {username}".to_string(),
        working_dir: String::new(),
    }]
}

impl RustClientProfile {
    pub fn validate(&self) -> Result<()> {
        if self.exe_path.trim().is_empty() {
            bail!("не указан путь к исполняемому файлу клиента (например, horizon.exe)");
        }
        if !Path::new(self.exe_path.trim()).is_file() {
            bail!("файл не найден: {}", self.exe_path);
        }
        Ok(())
    }
}

/// Разбор строки аргументов с поддержкой двойных кавычек.
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                has = true;
            }
            c if c.is_whitespace() && !in_q => {
                if has || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        out.push(cur);
    }
    out
}

pub async fn launch_client(
    profile: RustClientProfile,
    username: String,
    uuid: String,
    game_dir: PathBuf,
    shared: Arc<Shared>,
    kill_rx: oneshot::Receiver<()>,
) -> Result<()> {
    profile.validate()?;
    let exe = PathBuf::from(profile.exe_path.trim());
    let gd = game_dir.display().to_string();
    let args: Vec<String> = split_args(&profile.args)
        .into_iter()
        .map(|a| a.replace("{username}", &username).replace("{uuid}", &uuid).replace("{game_dir}", &gd))
        .collect();

    let wd = if !profile.working_dir.trim().is_empty() {
        PathBuf::from(profile.working_dir.trim())
    } else {
        exe.parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."))
    };

    shared.log(format!("[CLIENT] запуск {} {}", exe.display(), args.join(" ")));
    let mut cmd = Command::new(&exe);
    cmd.args(&args).current_dir(&wd);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW

    shared.client_running.store(true, Ordering::SeqCst);
    let prefix = format!("[{}] ", profile.name);
    launcher::run_process(cmd, &prefix, &shared, &shared.client_running, kill_rx).await?;
    Ok(())
}