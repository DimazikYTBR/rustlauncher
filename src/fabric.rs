//! Установка Fabric Loader: скачивает официальный профиль и кладёт его в versions/<id>/<id>.json.
//! Библиотеки и ванильную основу дальше скачивает mojang::prepare_version (через inheritsFrom).

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;
use std::path::Path;

const META: &str = "https://meta.fabricmc.net/v2/versions/loader";

/// Список версий Fabric Loader для данной версии Minecraft: (версия, stable), новые первыми.
pub async fn fetch_loaders(client: &reqwest::Client, mc: &str) -> Result<Vec<(String, bool)>> {
    let url = format!("{META}/{mc}");
    let v: Value = client.get(&url).send().await?.error_for_status()?.json().await?;
    let arr = v.as_array().ok_or_else(|| anyhow!("неожиданный ответ Fabric Meta"))?;
    Ok(arr
        .iter()
        .filter_map(|e| {
            let l = e.get("loader")?;
            Some((l.get("version")?.as_str()?.to_string(), l.get("stable").and_then(Value::as_bool).unwrap_or(false)))
        })
        .collect())
}

/// Скачивает профиль и сохраняет его. `loader = None` — последняя стабильная версия.
/// Возвращает id установленного профиля (например, fabric-loader-0.16.9-1.21.1).
pub async fn install(client: &reqwest::Client, root: &Path, mc: &str, loader: Option<&str>) -> Result<String> {
    let loader = match loader {
        Some(l) => l.to_string(),
        None => {
            let list = fetch_loaders(client, mc).await?;
            if list.is_empty() {
                bail!("Fabric не поддерживает версию {mc}");
            }
            list.iter().find(|(_, stable)| *stable).unwrap_or(&list[0]).0.clone()
        }
    };
    let url = format!("{META}/{mc}/{loader}/profile/json");
    let text = client.get(&url).send().await?.error_for_status()?.text().await?;
    let v: Value = serde_json::from_str(&text).context("разбор профиля Fabric")?;
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("в профиле Fabric нет id"))?
        .to_string();
    let dir = root.join("versions").join(&id);
    tokio::fs::create_dir_all(&dir).await?;
    tokio::fs::write(dir.join(format!("{id}.json")), text).await?;
    Ok(id)
}