//! Работа с официальными серверами Mojang: манифест версий, client.jar,
//! библиотеки, ассеты, нативные библиотеки (.dll). Параллельная загрузка с проверкой SHA1.

use anyhow::{anyhow, bail, Context, Result};
use futures::{stream, StreamExt};
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
pub const ASSETS_URL: &str = "https://resources.download.minecraft.net";
const DEFAULT_LIB_REPO: &str = "https://libraries.minecraft.net/";
const MAX_LOG_LINES: usize = 5000;
const PARALLEL_DOWNLOADS: usize = 24;

// ───────────────────────── Общее состояние (UI <-> фоновые задачи) ─────────────────────────

pub fn lk<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub struct Shared {
    pub logs: Mutex<Vec<String>>,
    pub status: Mutex<String>,
    pub total: AtomicU64,
    pub done: AtomicU64,
    /// Идёт подготовка/запуск игры (от нажатия «Играть» до завершения процесса).
    pub game_running: AtomicBool,
    /// Процесс игры реально запущен.
    pub game_started: AtomicBool,
    pub client_running: AtomicBool,
    pub manifest: Mutex<Option<VersionManifest>>,
    /// Версия, которую фоновая задача (например, установка Fabric) просит выбрать в UI.
    pub pending_select: Mutex<Option<String>>,
}

impl Shared {
    pub fn new() -> Arc<Self> {
        Arc::new(Shared {
            logs: Mutex::new(Vec::new()),
            status: Mutex::new("Готов".to_string()),
            total: AtomicU64::new(0),
            done: AtomicU64::new(0),
            game_running: AtomicBool::new(false),
            game_started: AtomicBool::new(false),
            client_running: AtomicBool::new(false),
            manifest: Mutex::new(None),
            pending_select: Mutex::new(None),
        })
    }

    pub fn log(&self, line: impl Into<String>) {
        let mut logs = lk(&self.logs);
        logs.push(line.into());
        if logs.len() > MAX_LOG_LINES {
            let extra = logs.len() - MAX_LOG_LINES;
            logs.drain(..extra);
        }
    }

    pub fn set_status(&self, s: impl Into<String>) {
        *lk(&self.status) = s.into();
    }

    pub fn status(&self) -> String {
        lk(&self.status).clone()
    }

    pub fn progress(&self) -> f32 {
        let total = self.total.load(Ordering::Relaxed);
        if total == 0 {
            return 0.0;
        }
        (self.done.load(Ordering::Relaxed) as f32 / total as f32).clamp(0.0, 1.0)
    }
}

// ───────────────────────── Структуры JSON ─────────────────────────

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionManifest {
    pub latest: Latest,
    pub versions: Vec<VersionEntry>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Latest {
    pub release: String,
    pub snapshot: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VersionEntry {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub url: String,
    pub sha1: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    pub main_class: String,
    pub minecraft_arguments: Option<String>,
    pub arguments: Option<Arguments>,
    pub asset_index: Option<AssetIndexRef>,
    pub assets: Option<String>,
    pub downloads: Option<Downloads>,
    pub libraries: Vec<Library>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub inherits_from: Option<String>,
    pub java_version: Option<JavaVersion>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub major_version: u32,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Arguments {
    pub game: Vec<Arg>,
    pub jvm: Vec<Arg>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Arg {
    Plain(String),
    Conditional { rules: Vec<Rule>, value: StrOrVec },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum StrOrVec {
    One(String),
    Many(Vec<String>),
}

impl Arg {
    pub fn resolve(&self) -> Vec<String> {
        match self {
            Arg::Plain(s) => vec![s.clone()],
            Arg::Conditional { rules, value } => {
                if rules_ok(rules) {
                    match value {
                        StrOrVec::One(s) => vec![s.clone()],
                        StrOrVec::Many(v) => v.clone(),
                    }
                } else {
                    Vec::new()
                }
            }
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub action: String,
    #[serde(default)]
    pub os: Option<OsRule>,
    #[serde(default)]
    pub features: Option<HashMap<String, bool>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct OsRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub arch: Option<String>,
}

impl Rule {
    fn matches(&self) -> bool {
        // Фичи (demo, custom resolution, quick play) лаунчер не включает.
        if let Some(f) = &self.features {
            if !f.is_empty() {
                return false;
            }
        }
        if let Some(os) = &self.os {
            if let Some(n) = &os.name {
                if n != "windows" {
                    return false;
                }
            }
            // Целевая платформа — Windows x64.
            if let Some(a) = &os.arch {
                if a == "x86" || a == "arm64" || a == "aarch64" {
                    return false;
                }
            }
        }
        true
    }
}

pub fn rules_ok(rules: &[Rule]) -> bool {
    if rules.is_empty() {
        return true;
    }
    let mut allowed = false;
    for r in rules {
        if r.matches() {
            allowed = r.action == "allow";
        }
    }
    allowed
}

pub fn rules_allow(rules: &Option<Vec<Rule>>) -> bool {
    match rules {
        None => true,
        Some(r) => rules_ok(r),
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Artifact {
    pub path: String,
    pub sha1: Option<String>,
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Downloads {
    pub client: Option<Artifact>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct LibDownloads {
    pub artifact: Option<Artifact>,
    pub classifiers: Option<HashMap<String, Artifact>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Library {
    pub name: String,
    pub url: Option<String>,
    pub downloads: Option<LibDownloads>,
    pub rules: Option<Vec<Rule>>,
    pub natives: Option<HashMap<String, String>>,
    pub extract: Option<Extract>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct Extract {
    pub exclude: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AssetIndexRef {
    pub id: String,
    pub sha1: Option<String>,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AssetIndex {
    pub objects: HashMap<String, AssetObject>,
    pub map_to_resources: bool,
    #[serde(rename = "virtual")]
    pub is_virtual: bool,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct AssetObject {
    pub hash: String,
    pub size: u64,
}

// ───────────────────────── HTTP, манифест, загрузка файлов ─────────────────────────

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("RustLauncher/0.1")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .pool_max_idle_per_host(PARALLEL_DOWNLOADS)
        .build()
        .expect("не удалось создать HTTP-клиент")
}

/// Скачивает манифест версий; при ошибке сети использует кэш на диске.
pub async fn fetch_manifest(client: &reqwest::Client, root: &Path) -> Result<VersionManifest> {
    let cache = root.join("version_manifest_v2.json");
    let net = async {
        let resp = client.get(MANIFEST_URL).send().await?.error_for_status()?;
        resp.text().await
    }
    .await;
    match net {
        Ok(text) => {
            let m: VersionManifest = serde_json::from_str(&text).context("разбор манифеста")?;
            tokio::fs::create_dir_all(root).await.ok();
            tokio::fs::write(&cache, &text).await.ok();
            Ok(m)
        }
        Err(e) => {
            let data = tokio::fs::read(&cache)
                .await
                .map_err(|_| anyhow!("не удалось получить манифест версий: {e}"))?;
            Ok(serde_json::from_slice(&data).context("разбор кэша манифеста")?)
        }
    }
}

pub async fn sha1_file(path: &Path) -> Result<String> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<String> {
        use std::io::Read;
        let mut f = std::fs::File::open(&p)?;
        let mut h = Sha1::new();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            h.update(&buf[..n]);
        }
        Ok(hex::encode(h.finalize()))
    })
    .await?
}

async fn download_once(client: &reqwest::Client, url: &str, path: &Path, sha1: Option<&str>) -> Result<()> {
    let resp = client.get(url).send().await?.error_for_status()?;
    let bytes = resp.bytes().await?;
    if let Some(h) = sha1 {
        let got = hex::encode(Sha1::digest(&bytes));
        if !got.eq_ignore_ascii_case(h) {
            bail!("несовпадение SHA1 (ожидалось {h}, получено {got})");
        }
    }
    let tmp = PathBuf::from(format!("{}.part", path.display()));
    tokio::fs::write(&tmp, &bytes).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

/// Гарантирует наличие файла: если он есть и SHA1 совпадает — ничего не качает.
/// Возвращает true, если файл был скачан.
pub async fn ensure_file(client: &reqwest::Client, url: &str, path: &Path, sha1: Option<&str>) -> Result<bool> {
    if tokio::fs::try_exists(path).await.unwrap_or(false) {
        match sha1 {
            Some(h) => {
                if sha1_file(path).await.map(|x| x.eq_ignore_ascii_case(h)).unwrap_or(false) {
                    return Ok(false);
                }
            }
            None => return Ok(false),
        }
    }
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut last: Option<anyhow::Error> = None;
    for attempt in 0..3u64 {
        match download_once(client, url, path, sha1).await {
            Ok(()) => return Ok(true),
            Err(e) => {
                last = Some(e);
                tokio::time::sleep(Duration::from_millis(500 * (attempt + 1))).await;
            }
        }
    }
    Err(last
        .unwrap_or_else(|| anyhow!("неизвестная ошибка"))
        .context(format!("загрузка {url}")))
}

#[derive(Clone)]
struct Job {
    url: String,
    path: PathBuf,
    sha1: Option<String>,
}

fn queue(jobs: &mut Vec<Job>, a: &Artifact, path: &Path, shared: &Shared) {
    if a.url.is_empty() {
        if !path.exists() {
            shared.log(format!("[WARN] файл отсутствует и нет URL для загрузки: {}", path.display()));
        }
    } else {
        jobs.push(Job { url: a.url.clone(), path: path.to_path_buf(), sha1: a.sha1.clone() });
    }
}

async fn run_jobs(client: &reqwest::Client, mut jobs: Vec<Job>, shared: &Arc<Shared>) -> Result<()> {
    let mut seen = HashSet::new();
    jobs.retain(|j| seen.insert(j.path.clone()));
    shared.total.fetch_add(jobs.len() as u64, Ordering::Relaxed);
    let errors = Arc::new(Mutex::new(0usize));

    stream::iter(jobs)
        .map(|job| {
            let client = client.clone();
            let shared = shared.clone();
            let errors = errors.clone();
            async move {
                if let Err(e) = ensure_file(&client, &job.url, &job.path, job.sha1.as_deref()).await {
                    shared.log(format!("[ERROR] {}: {e:#}", job.path.display()));
                    *lk(&errors) += 1;
                }
                shared.done.fetch_add(1, Ordering::Relaxed);
            }
        })
        .buffer_unordered(PARALLEL_DOWNLOADS)
        .for_each(|_| async {})
        .await;

    let n = *lk(&errors);
    if n > 0 {
        bail!("не удалось скачать файлов: {n}");
    }
    Ok(())
}

// ───────────────────────── Подготовка версии ─────────────────────────

pub struct Prepared {
    pub json: VersionJson,
    pub classpath: Vec<PathBuf>,
    pub natives_dir: PathBuf,
    pub assets_dir: PathBuf,
    pub assets_id: String,
}

fn lookup(shared: &Shared, id: &str) -> Option<VersionEntry> {
    lk(&shared.manifest)
        .as_ref()
        .and_then(|m| m.versions.iter().find(|v| v.id == id).cloned())
}

async fn load_version_json(shared: &Shared, client: &reqwest::Client, root: &Path, id: &str) -> Result<VersionJson> {
    let path = root.join("versions").join(id).join(format!("{id}.json"));
    if let Some(e) = lookup(shared, id) {
        ensure_file(client, &e.url, &path, Some(&e.sha1)).await?;
    } else if !path.exists() {
        bail!("версия {id} не установлена, а манифест недоступен или не содержит её");
    }
    let data = tokio::fs::read(&path).await?;
    serde_json::from_slice(&data).with_context(|| format!("разбор {}", path.display()))
}

fn merge(mut parent: VersionJson, child: VersionJson) -> VersionJson {
    let mut libs = child.libraries;
    libs.extend(parent.libraries);
    parent.libraries = libs;
    if !child.main_class.is_empty() {
        parent.main_class = child.main_class;
    }
    if let Some(a) = child.arguments {
        let pa = parent.arguments.take().unwrap_or_default();
        parent.arguments = Some(Arguments {
            game: [pa.game, a.game].concat(),
            jvm: [pa.jvm, a.jvm].concat(),
        });
    }
    if child.minecraft_arguments.is_some() {
        parent.minecraft_arguments = child.minecraft_arguments;
    }
    if child.asset_index.is_some() {
        parent.asset_index = child.asset_index;
    }
    if child.assets.is_some() {
        parent.assets = child.assets;
    }
    if child.java_version.is_some() {
        parent.java_version = child.java_version;
    }
    if child.kind.is_some() {
        parent.kind = child.kind;
    }
    parent.id = child.id;
    parent.inherits_from = None;
    parent
}

fn maven_path(name: &str) -> Option<String> {
    let (coords, ext) = match name.split_once('@') {
        Some((c, e)) => (c, e),
        None => (name, "jar"),
    };
    let p: Vec<&str> = coords.split(':').collect();
    if p.len() < 3 {
        return None;
    }
    let (g, a, v) = (p[0].replace('.', "/"), p[1], p[2]);
    let file = match p.get(3) {
        Some(c) => format!("{a}-{v}-{c}.{ext}"),
        None => format!("{a}-{v}.{ext}"),
    };
    Some(format!("{g}/{a}/{v}/{file}"))
}

fn lib_key(name: &str) -> String {
    let p: Vec<&str> = name.split(':').collect();
    format!("{}:{}:{}", p.first().unwrap_or(&""), p.get(1).unwrap_or(&""), p.get(3).unwrap_or(&""))
}

fn lib_artifact(lib: &Library) -> Option<Artifact> {
    if let Some(d) = &lib.downloads {
        return d.artifact.clone();
    }
    // Библиотека без блока downloads (Fabric/Quilt и т.п.): maven-координаты.
    let path = maven_path(&lib.name)?;
    let mut base = lib.url.clone().unwrap_or_else(|| DEFAULT_LIB_REPO.to_string());
    if !base.ends_with('/') {
        base.push('/');
    }
    Some(Artifact { url: format!("{base}{path}"), path, sha1: None, size: 0 })
}

fn extract_natives(dir: &Path, jars: &[(PathBuf, Vec<String>)]) -> Result<()> {
    std::fs::create_dir_all(dir)?;
    for (jar, excl) in jars {
        let f = std::fs::File::open(jar).with_context(|| format!("открытие {}", jar.display()))?;
        let mut z = zip::ZipArchive::new(f).with_context(|| format!("zip {}", jar.display()))?;
        for i in 0..z.len() {
            let mut e = z.by_index(i)?;
            if e.is_dir() {
                continue;
            }
            let name = e.name().to_string();
            if !name.to_ascii_lowercase().ends_with(".dll") {
                continue;
            }
            if excl.iter().any(|x| name.starts_with(x.as_str())) {
                continue;
            }
            let Some(fname) = Path::new(&name).file_name() else { continue };
            let out = dir.join(fname);
            if let Ok(md) = std::fs::metadata(&out) {
                if md.len() == e.size() {
                    continue;
                }
            }
            let mut o = std::fs::File::create(&out)?;
            std::io::copy(&mut e, &mut o)?;
        }
    }
    Ok(())
}

fn copy_assets_to(root: &Path, dest: &Path, idx: &AssetIndex) -> Result<()> {
    for (name, o) in &idx.objects {
        if o.hash.len() < 2 {
            continue;
        }
        let src = root.join("assets").join("objects").join(&o.hash[..2]).join(&o.hash);
        let dst = dest.join(name);
        if dst.exists() {
            continue;
        }
        if let Some(p) = dst.parent() {
            std::fs::create_dir_all(p)?;
        }
        std::fs::copy(&src, &dst)?;
    }
    Ok(())
}

/// Скачивает/проверяет всё необходимое для запуска версии `id` и распаковывает natives.
/// Поддерживает `inheritsFrom` (Fabric/Quilt/Forge-профили, положенные в versions/<id>/<id>.json).
pub async fn prepare_version(
    shared: &Arc<Shared>,
    client: &reqwest::Client,
    root: &Path,
    game_dir: &Path,
    id: &str,
) -> Result<Prepared> {
    shared.done.store(0, Ordering::Relaxed);
    shared.total.store(0, Ordering::Relaxed);
    shared.set_status(format!("Версия {id}: чтение описания"));

    // Цепочка наследования: ребёнок -> ... -> ванильная версия.
    let mut chain = Vec::new();
    let mut cur = id.to_string();
    loop {
        let j = load_version_json(shared, client, root, &cur).await?;
        let next = j.inherits_from.clone();
        chain.push(j);
        match next {
            Some(n) => {
                if chain.len() > 4 {
                    bail!("слишком глубокая цепочка inheritsFrom");
                }
                cur = n;
            }
            None => break,
        }
    }
    let mut json = chain.pop().expect("цепочка не пуста");
    let jar_id = json.id.clone();
    while let Some(child) = chain.pop() {
        json = merge(json, child);
    }
    json.id = id.to_string();
    if json.main_class.is_empty() {
        bail!("в описании версии нет mainClass");
    }

    let libs_dir = root.join("libraries");
    let mut jobs: Vec<Job> = Vec::new();
    let mut classpath: Vec<PathBuf> = Vec::new();
    let mut seen_keys: HashSet<String> = HashSet::new();
    let mut native_jars: Vec<(PathBuf, Vec<String>)> = Vec::new();

    for lib in &json.libraries {
        if !rules_allow(&lib.rules) {
            continue;
        }
        let excludes = lib.extract.as_ref().map(|e| e.exclude.clone()).unwrap_or_default();

        // Старый формат natives (classifiers).
        if let Some(nm) = &lib.natives {
            if let Some(k) = nm.get("windows") {
                let k = k.replace("${arch}", "64");
                if let Some(a) = lib
                    .downloads
                    .as_ref()
                    .and_then(|d| d.classifiers.as_ref())
                    .and_then(|c| c.get(&k))
                {
                    let p = libs_dir.join(&a.path);
                    queue(&mut jobs, a, &p, shared);
                    native_jars.push((p, excludes.clone()));
                }
            }
        }

        if let Some(a) = lib_artifact(lib) {
            if !seen_keys.insert(lib_key(&lib.name)) {
                continue;
            }
            let p = libs_dir.join(&a.path);
            queue(&mut jobs, &a, &p, shared);
            classpath.push(p.clone());
            // Новый формат: natives — отдельная библиотека "…:natives-windows".
            if lib.name.contains(":natives-windows") && !lib.name.ends_with("-arm64") && !lib.name.ends_with("-x86") {
                native_jars.push((p, excludes));
            }
        }
    }

    // client.jar
    let jar_path = root.join("versions").join(&jar_id).join(format!("{jar_id}.jar"));
    match json.downloads.as_ref().and_then(|d| d.client.as_ref()) {
        Some(c) => queue(&mut jobs, c, &jar_path, shared),
        None => {
            if !jar_path.exists() {
                bail!("нет client.jar для {jar_id}");
            }
        }
    }
    classpath.push(jar_path);

    // Ассеты
    let mut assets_dir = root.join("assets");
    let mut assets_id = json.assets.clone().unwrap_or_else(|| "legacy".to_string());
    let mut index: Option<AssetIndex> = None;
    if let Some(ai) = &json.asset_index {
        assets_id = ai.id.clone();
        let ip = root.join("assets").join("indexes").join(format!("{}.json", ai.id));
        shared.set_status("Загрузка индекса ассетов");
        ensure_file(client, &ai.url, &ip, ai.sha1.as_deref()).await?;
        let idx: AssetIndex = serde_json::from_slice(&tokio::fs::read(&ip).await?).context("разбор индекса ассетов")?;
        for o in idx.objects.values() {
            if o.hash.len() < 2 {
                continue;
            }
            let pre = &o.hash[..2];
            jobs.push(Job {
                url: format!("{ASSETS_URL}/{pre}/{}", o.hash),
                path: root.join("assets").join("objects").join(pre).join(&o.hash),
                sha1: Some(o.hash.clone()),
            });
        }
        index = Some(idx);
    }

    shared.set_status("Проверка и загрузка файлов");
    shared.log(format!("[LAUNCHER] файлов к проверке: {}", jobs.len()));
    run_jobs(client, jobs, shared).await?;

    // Natives
    shared.set_status("Распаковка нативных библиотек");
    let natives_dir = root.join("versions").join(id).join("natives");
    {
        let nd = natives_dir.clone();
        tokio::task::spawn_blocking(move || extract_natives(&nd, &native_jars)).await??;
    }

    // Старые версии: virtual / map_to_resources
    if let Some(idx) = index {
        if idx.is_virtual {
            assets_dir = root.join("assets").join("virtual").join("legacy");
            let (r, d) = (root.to_path_buf(), assets_dir.clone());
            tokio::task::spawn_blocking(move || copy_assets_to(&r, &d, &idx)).await??;
        } else if idx.map_to_resources {
            assets_dir = game_dir.join("resources");
            let (r, d) = (root.to_path_buf(), assets_dir.clone());
            tokio::task::spawn_blocking(move || copy_assets_to(&r, &d, &idx)).await??;
        }
    }

    shared.set_status("Файлы готовы");
    Ok(Prepared { json, classpath, natives_dir, assets_dir, assets_id })
}