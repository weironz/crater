//! 索引文件与搜索(D-123 第七节)—— registry 管字节,索引管"有什么"。
//!
//! 这不是没想过用 registry 的 API。**OCI 里没有搜索**:内容发现只定义了
//! `tags/list`(某个仓库有哪些版本),"这个 registry 上有哪些包"根本问不出来
//! —— `_catalog` 不在规范里,Docker Hub 还有意禁用它。Helm 遇到同一堵墙,
//! 答案是经典 repo 的静态 `index.yaml`;timoni / Flux / KitOps 干脆不做搜索。
//!
//! 走索引文件另有两处便宜:
//!
//! - **断网也成立。** 索引可以和 `pkg save` 的 tar 一起塞进 U 盘;registry API
//!   在断网机房里根本调不到。
//! - **任何静态 HTTP 都能托管**,包括 rustfs 这类 S3 —— 与 storage-design 的
//!   分层一致:registry 管不可变字节,索引管列举。
//!
//! 索引是**派生数据**,随时能从 registry 重算,所以永远不手改:谁推包谁重跑
//! `crater pkg index`。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};
use crater_core::store::ImageStore;
use serde::{Deserialize, Serialize};

use crate::say;

const API_VERSION: &str = "crater.pkg/v1";

/// 索引里的一个版本条目。
///
/// 记的全是**读 config 就能拿到**的东西(契约、规模、架构),一层都不用下载。
/// 于是给一百个包生成索引只是一百次 manifest+config 往返,几秒钟的事。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// **tag** —— 索引唯一能保证可寻址的东西。
    pub version: String,
    /// 蓝图自己声明的修订号(可与 tag 不同,如 Helm 的 version / appVersion)。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub blueprint_version: String,
    /// 完整 OCI 引用 —— `install` 直接拿它去拉。
    pub reference: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub digest: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    /// 机群契约:不进 inventory 就装不上,搜索结果里就该看见。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fleet: Vec<FleetNeed>,
    #[serde(default)]
    pub params: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub platforms: Vec<String>,
    /// 离线镜像时指向同目录的 `.pkg.tar`(相对路径)。
    ///
    /// **由 `pkg index` 照实填,不是人手写的。** 生成索引时扫一遍输出目录里的
    /// `*.pkg.tar`,读出每个归档自报的引用,对得上才记 —— 记的是"这个 tar
    /// 确实装着这条引用的字节",不是"这里大概应该有个 tar"。
    /// 读它的是 [`resolve`]:U 盘上的包忘了 `pkg load` 时,把"连不上 registry"
    /// 换成"这份字节就在你手上的哪个文件里"。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetNeed {
    pub name: String,
    pub min: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    #[serde(rename = "apiVersion")]
    pub api_version: String,
    pub generated: String,
    /// 包名 → 版本条目(新的排前面)。
    pub entries: BTreeMap<String, Vec<Entry>>,
}

impl Default for Index {
    fn default() -> Self {
        Index {
            api_version: API_VERSION.into(),
            generated: now_rfc3339(),
            entries: BTreeMap::new(),
        }
    }
}

impl Index {
    /// 并入一条,同名同版本则替换 —— 重跑 `pkg index` 是幂等的。
    fn upsert(&mut self, name: &str, e: Entry) {
        let v = self.entries.entry(name.to_string()).or_default();
        v.retain(|x| x.version != e.version);
        v.push(e);
        // semver 降序:读的人先看见最新的那个。
        v.sort_by(|a, b| crate::pkg::semver_key(&b.version).cmp(&crate::pkg::semver_key(&a.version)));
    }
}

// ───────────────────────────── 生成 ─────────────────────────────

/// 从一条 config 契约折出索引条目。
fn entry_of(reference: &str, cfg: &serde_json::Value, platforms: Vec<String>, digest: String) -> (String, Entry) {
    let name = cfg["name"].as_str().unwrap_or("").to_string();
    // **版本取 tag,不取蓝图里的 `version:`。**
    //
    // 索引存在的意义是把"包名 + 版本"翻译成一条能拉的引用,而能拉的只有
    // tag。蓝图的 `version:` 是它自己的修订号,与被装的那个东西的版本
    // 未必一致(library/yq 声明 `version: "1"`,tag 是 yq 的 4.44.3 ——
    // 两个都合理,Helm 也分 chart version 与 appVersion)。
    //
    // 这条是被测试逼出来的:先按蓝图 version 组织时,yq 的 4.44.3 与
    // 4.40.5 双双报成 "1",后一条**静默**覆盖了前一条。
    let version = reference.rsplit(':').next().unwrap_or("latest").to_string();
    let e = Entry {
        version,
        blueprint_version: cfg["version"].as_str().unwrap_or_default().to_string(),
        reference: reference.to_string(),
        digest,
        description: cfg["description"].as_str().unwrap_or("").to_string(),
        fleet: cfg["fleet"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|g| {
                        Some(FleetNeed {
                            name: g["name"].as_str()?.to_string(),
                            min: g["min"].as_u64().unwrap_or(0) as usize,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
        params: cfg["params"].as_array().map(|a| a.len()).unwrap_or(0),
        platforms,
        urls: Vec::new(),
    };
    (name, e)
}

/// `crater pkg index <来源>… [-o out] [--merge]`
///
/// 来源两种:`oci://reg/ns/name`(不带 tag → 走 `tags/list` 收全部版本)、
/// `--store`(本地 store 里的全部蓝图包)。**没有"扫一个 registry"这一种**
/// —— 那正是 OCI 问不出来的东西,假装能问只会在别人的 registry 上失败。
pub async fn index(sources: &[String], from_store: bool, out: &std::path::Path, merge: bool) -> Result<()> {
    let mut idx = if merge {
        if out.exists() {
            // 读不动就**报错**,绝不退回空索引 —— `--merge` 的输入正是 `out`
            // 自己,把"读坏了"当成"没有历史"会把整份历史一次性写没。
            let existing = load_index_file(out)?;
            // 解析得动 ≠ 没坏。截断在 `entries:` 那一行之后的索引是**合法
            // YAML**(`entries` 成了 null,serde 收成空 map),于是 merge 从
            // 零开始、退出 0、把整份历史换成这一版 —— 一点症状都没有。真机
            // 上复现过。
            //
            // 拦得住是因为**空索引根本不该存在**:下面那道 `n == 0` 的闸决定
            // 了 `pkg index` 自己永远写不出一份零个包的索引。所以磁盘上出现
            // 一份,只可能是写坏了或放错了文件。
            //
            // 这条只管 merge 这一处,不下沉进 load_index_file —— 别人发布的
            // 空索引拉回来缓存着是合法的,`repo list` 不该因此报未同步。
            if existing.entries.is_empty() {
                bail!(
                    "{} 是一份零个包的索引 —— `pkg index` 写不出这种东西,\
                     多半是写了一半被打断或放错了文件。并进去会把历史清空,不干了",
                    out.display()
                );
            }
            say!("并入 {}({} 个包)", out.display(), existing.entries.len());
            existing
        } else {
            // `--merge` 指着一个不存在的文件只有两种可能:第一次发布(正常),
            // 或者 CI 里 `-o` 写错了 / 取上一版索引的那步没成(事故)。这里
            // 分不出是哪种,但后者的后果与索引损坏完全一样 —— 整份历史被这
            // 一版顶掉,而输出看起来和一次成功的 merge 一模一样。
            //
            // 所以不能一声不吭。仍然不报错:第一次发布必须能跑,否则每条
            // 流水线都要为"第一次"多写一个分支。
            crate::oops!("  · --merge 的 {} 不存在 —— 当作首次发布,从空索引开始", out.display());
            Index::default()
        }
    } else {
        Index::default()
    };
    let mut n = 0usize;

    if from_store {
        let store = ImageStore::open()?;
        for img in store.list()? {
            let Ok(m) = store.resolve_manifest(&img.reference) else { continue };
            let Some(cfg) = crate::pkg::config_of(&store, &m) else { continue };
            let plats = crater_core::store::platforms_of(&m);
            let (name, e) = entry_of(&img.reference, &cfg, plats, img.digest.clone());
            if name.is_empty() {
                continue;
            }
            say!("  + {name} {} ← 本地 store", e.version);
            idx.upsert(&name, e);
            n += 1;
        }
    }

    for src in sources {
        let src = src.trim_start_matches("oci://");
        // 带 tag 就只收那一版;不带就问 registry 有哪些版本。
        let refs: Vec<String> = if src.rsplit('/').next().unwrap_or("").contains(':') {
            vec![src.to_string()]
        } else {
            let tags = ImageStore::list_tags(src)
                .await
                .with_context(|| format!("列 {src} 的版本"))?;
            tags.iter().map(|t| format!("{src}:{t}")).collect()
        };
        for r in refs {
            // 一个 tag 读不动(不是 crater 包、或权限不够)不该让整份索引失败 ——
            // 一个仓库里混着别的制品是常态。报出来,继续。
            match ImageStore::fetch_contract(&r).await {
                Ok((_m, bytes, plats)) => {
                    let Ok(cfg) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                        crate::oops!("  · {r} 的 config 不是 crater 契约,跳过");
                        continue;
                    };
                    let (name, e) = entry_of(&r, &cfg, plats, String::new());
                    if name.is_empty() {
                        crate::oops!("  · {r} 没有包名,跳过");
                        continue;
                    }
                    say!("  + {name} {}", e.version);
                    idx.upsert(&name, e);
                    n += 1;
                }
                Err(e) => crate::oops!("  · {r} 读不到契约({e}),跳过"),
            }
        }
    }

    if n == 0 {
        bail!("一个包都没收进来 —— 索引不写了(免得把已有的覆盖成空)");
    }
    let tars = attach_local_tars(&mut idx, out);
    if tars > 0 {
        say!("  · 同目录 {tars} 个包 tar 已记进 urls —— 这份索引可以整个目录搬走");
    }
    idx.generated = now_rfc3339();
    write_atomically(out, serde_yaml::to_string(&idx)?.as_bytes())?;
    say!();
    // 报**索引里**的版本数,不是本次收到的 n。merge 时两者差得很远,而"这一
    // 版并进去之后历史还在不在"恰恰只能从总数看出来 —— 之前只印 n,一次成功
    // 的增量发布和一次把历史写没的全量重写在终端上长得一模一样。
    let total: usize = idx.entries.values().map(|v| v.len()).sum();
    if merge {
        say!("索引 → {}({} 个包,{total} 个版本;本次 {n} 个)", out.display(), idx.entries.len());
    } else {
        say!("索引 → {}({} 个包,{total} 个版本)", out.display(), idx.entries.len());
    }
    say!("托管到任意静态 HTTP,对方 `crater repo add <名> <地址>` 就能搜。");
    Ok(())
}

// ───────────────────────── 离线镜像:urls ─────────────────────────

/// 一个 oci-archive 自报装着哪些引用。
///
/// 只流式读归档里的 `index.json`(几百字节)—— 一份包 tar 可以有几百兆,
/// 为了问一句"你是谁"把它整个摊开是不必要的。
fn refs_in_archive(tar: &std::path::Path) -> Vec<String> {
    let Ok(f) = std::fs::File::open(tar) else { return Vec::new() };
    let mut ar = tar::Archive::new(f);
    let Ok(entries) = ar.entries() else { return Vec::new() };
    for e in entries.flatten() {
        let Ok(p) = e.path() else { continue };
        if p.file_name().and_then(|n| n.to_str()) != Some("index.json") {
            continue;
        }
        // `blobs/sha256/…` 里不会有 index.json,顶层那一份才是归档目录。
        if p.components().count() != 2 && p.components().count() != 1 {
            continue;
        }
        let Ok(idx) = serde_json::from_reader::<_, serde_json::Value>(e) else { return Vec::new() };
        return idx["manifests"]
            .as_array()
            .map(|ms| {
                ms.iter()
                    .filter_map(|m| m["annotations"]["org.opencontainers.image.ref.name"].as_str())
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default();
    }
    Vec::new()
}

/// 把索引输出目录里的包 tar 记进对应条目的 `urls`,返回记上的 tar 个数。
///
/// **只记对得上的。** 匹配靠归档自报的引用,不靠文件名 —— `yq.pkg.tar` 里
/// 装的是哪一版是它自己说了算,按名字猜会在同名不同版时把人指到错的文件上,
/// 而那种错要到断网机上 load 完才看得出来。
///
/// 每次生成都先清空再重填:索引是派生数据,上一版记过的 tar 这一版可能已经
/// 不在了,留着一条指向空气的路径比没有更糟。
fn attach_local_tars(idx: &mut Index, out: &std::path::Path) -> usize {
    for versions in idx.entries.values_mut() {
        for e in versions.iter_mut() {
            e.urls.clear();
        }
    }
    let dir = out.parent().filter(|p| !p.as_os_str().is_empty()).map(|p| p.to_path_buf());
    let dir = dir.unwrap_or_else(|| std::path::PathBuf::from("."));
    let Ok(rd) = std::fs::read_dir(&dir) else { return 0 };
    let mut files: Vec<PathBuf> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            p.is_file() && (n.ends_with(".pkg.tar") || n.ends_with(".oci"))
        })
        .collect();
    files.sort();
    let mut used = 0usize;
    for f in &files {
        let name = f.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let carried = refs_in_archive(f);
        let mut hit = false;
        for versions in idx.entries.values_mut() {
            for e in versions.iter_mut() {
                if carried.iter().any(|r| r == &e.reference) && !e.urls.contains(&name) {
                    e.urls.push(name.clone());
                    hit = true;
                }
            }
        }
        if hit {
            used += 1;
        }
    }
    used
}

// ───────────────────────────── 仓库 ─────────────────────────────

#[derive(Debug, Default, Serialize, Deserialize)]
struct Repos {
    #[serde(default)]
    repos: BTreeMap<String, String>,
}

fn repos_file() -> PathBuf {
    ImageStore::home().join("repos.yaml")
}
fn cache_dir() -> PathBuf {
    ImageStore::home().join("repos")
}
fn cache_of(name: &str) -> PathBuf {
    cache_dir().join(format!("{name}.yaml"))
}

fn load_repos() -> Repos {
    std::fs::read(repos_file())
        .ok()
        .and_then(|b| serde_yaml::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_repos(r: &Repos) -> Result<()> {
    let p = repos_file();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(p, serde_yaml::to_string(r)?)?;
    Ok(())
}

/// 先写同目录的临时文件再 rename —— **索引不能有"写了一半"这个状态**。
///
/// `fs::write` 是先截断再写:进程在中间被打断(Ctrl-C、CI 超时、磁盘满),
/// 留下的就是一份截断的索引。而增量发布的下一次 `--merge` 读的正是这个文件,
/// 于是一次中断吃掉整份历史。同目录 + rename 才是原子的(跨文件系统的
/// rename 会失败,所以临时文件必须是兄弟而不是 /tmp 里的)。
fn write_atomically(out: &std::path::Path, bytes: &[u8]) -> Result<()> {
    let name = out
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "index.yaml".into());
    let tmp = out.with_file_name(format!(".{name}.tmp"));
    std::fs::write(&tmp, bytes).with_context(|| format!("写索引 {}", tmp.display()))?;
    std::fs::rename(&tmp, out).with_context(|| format!("落位索引 {}", out.display()))?;
    Ok(())
}

fn load_index_file(p: &std::path::Path) -> Result<Index> {
    let text = std::fs::read_to_string(p).with_context(|| format!("读索引 {}", p.display()))?;
    let idx: Index = serde_yaml::from_str(&text).with_context(|| format!("{} 不是 crater 索引", p.display()))?;
    if idx.api_version != API_VERSION {
        bail!("{} 的 apiVersion 是 {},本机认得的是 {API_VERSION}", p.display(), idx.api_version);
    }
    Ok(idx)
}

/// 取一份索引的字节。http(s) 走网络,其余当本地路径 —— `file://` 前缀可选,
/// 因为 U 盘场景下人多半直接写路径。
async fn fetch_index(url: &str) -> Result<Vec<u8>> {
    if url.starts_with("http://") || url.starts_with("https://") {
        return crater_core::source::fetch(url).await;
    }
    let p = url.strip_prefix("file://").unwrap_or(url);
    std::fs::read(p).with_context(|| format!("读索引 {p}"))
}

pub async fn add(name: &str, url: &str) -> Result<()> {
    let mut r = load_repos();
    if let Some(old) = r.repos.get(name) {
        if old != url {
            say!("{name} 原指向 {old},改为 {url}");
        }
    }
    r.repos.insert(name.to_string(), url.to_string());
    save_repos(&r)?;
    // 当场拉一次:地址写错要现在就知道,而不是下次 search 空手而归时。
    update(Some(name)).await
}

pub fn remove(name: &str) -> Result<()> {
    let mut r = load_repos();
    if r.repos.remove(name).is_none() {
        bail!("没有名叫 `{name}` 的仓库");
    }
    save_repos(&r)?;
    let _ = std::fs::remove_file(cache_of(name));
    say!("已移除 {name}");
    Ok(())
}

pub fn list() -> Result<()> {
    let r = load_repos();
    if r.repos.is_empty() {
        say!("还没有配仓库。`crater repo add <名> <索引地址>`");
        say!("索引由 `crater pkg index` 生成,托管在任意静态 HTTP 或 U 盘上。");
        return Ok(());
    }
    let w = r.repos.keys().map(|k| k.chars().count()).max().unwrap_or(0);
    for (n, u) in &r.repos {
        let c = cache_of(n);
        let stat = match load_index_file(&c) {
            Ok(i) => format!("{} 个包", i.entries.len()),
            Err(_) => "未同步".to_string(),
        };
        say!("  {:<w$}  {:<10}  {}", n, stat, u, w = w);
    }
    Ok(())
}

pub async fn update(only: Option<&str>) -> Result<()> {
    let r = load_repos();
    if r.repos.is_empty() {
        bail!("还没有配仓库。`crater repo add <名> <索引地址>`");
    }
    std::fs::create_dir_all(cache_dir())?;
    let mut bad = 0;
    for (n, u) in &r.repos {
        if only.is_some_and(|o| o != n) {
            continue;
        }
        match fetch_index(u).await {
            Ok(bytes) => {
                // 先落到临时文件再校验,坏索引不该把上一份好的冲掉 ——
                // `repo update` 在断网时最容易撞上,而那时旧索引正是救命的。
                let tmp = cache_of(n).with_extension("tmp");
                std::fs::write(&tmp, &bytes)?;
                match load_index_file(&tmp) {
                    Ok(i) => {
                        std::fs::rename(&tmp, cache_of(n))?;
                        say!("  ✓ {n}  {} 个包", i.entries.len());
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        crate::oops!("  ✗ {n}  {e}(保留上一份)");
                        bad += 1;
                    }
                }
            }
            Err(e) => {
                crate::oops!("  ✗ {n}  取不到({e})(保留上一份)");
                bad += 1;
            }
        }
    }
    if bad > 0 && only.is_some() {
        bail!("{} 同步失败", only.unwrap());
    }
    Ok(())
}

/// 全部已缓存的索引:(仓库名, 索引)。
fn cached() -> Vec<(String, Index)> {
    load_repos()
        .repos
        .keys()
        .filter_map(|n| load_index_file(&cache_of(n)).ok().map(|i| (n.clone(), i)))
        .collect()
}

// ───────────────────────────── 搜索 ─────────────────────────────

/// `crater search <关键词>` —— 只查本地缓存,不连网。
///
/// 不连网是刻意的:搜索要能在断网机房用,而且"每次搜都往外发一串请求"是
/// 把使用习惯和网络状况绑死。要新的就 `crater repo update`,那是一个明确动作。
pub fn search(query: &str) -> Result<()> {
    let repos = cached();
    if repos.is_empty() {
        say!("还没有可搜的索引。`crater repo add <名> <地址>` 再 `crater repo update`。");
        return Ok(());
    }
    let q = query.to_lowercase();
    let mut hits: Vec<(String, String, &Entry)> = Vec::new();
    for (rn, idx) in &repos {
        for (name, versions) in &idx.entries {
            let Some(latest) = versions.first() else { continue };
            if q.is_empty()
                || name.to_lowercase().contains(&q)
                || latest.description.to_lowercase().contains(&q)
            {
                hits.push((rn.clone(), name.clone(), latest));
            }
        }
    }
    if hits.is_empty() {
        say!("没有匹配 `{query}` 的包。`crater repo update` 拉一次新的?");
        return Ok(());
    }
    hits.sort_by(|a, b| a.1.cmp(&b.1));
    let wn = hits.iter().map(|h| h.1.chars().count()).max().unwrap_or(0);
    let wv = hits.iter().map(|h| h.2.version.chars().count()).max().unwrap_or(0);
    for (repo, name, e) in &hits {
        // 机群契约直接摆出来 —— "要几台机器"是决定装不装的第一个问题,
        // 让人先装再发现 inventory 不满足,是最费时间的顺序。
        let need = if e.fleet.is_empty() {
            String::new()
        } else {
            let s: Vec<String> = e.fleet.iter().map(|f| format!("{}×{}", f.name, f.min)).collect();
            format!("  [{}]", s.join(" "))
        };
        say!("  {:<wn$}  {:<wv$}  {}/{}{need}  {}", name, e.version, repo, name, e.description, wn = wn, wv = wv);
    }
    say!();
    say!("{} 个包。`crater pkg inspect <ref>` 看契约,`crater install <名>` 装。", hits.len());
    Ok(())
}

/// 包名(可带 `:版本`)→ OCI 引用。`install` 用它把 `crater install mysql`
/// 变成一条真引用。
///
/// 名字撞车时**报错而不是猜**:两个仓库都有 `mysql` 时选错一个,装上去的
/// 是别人的东西,而这件事要到出问题才会被发现。
pub fn resolve(name: &str, repo: Option<&str>) -> Result<String> {
    let (n, want_ver) = match name.split_once(':') {
        Some((a, b)) => (a, Some(b)),
        None => (name, None),
    };
    let mut found: Vec<(String, &Entry)> = Vec::new();
    let repos = cached();
    for (rn, idx) in &repos {
        if repo.is_some_and(|r| r != rn) {
            continue;
        }
        let Some(versions) = idx.entries.get(n) else { continue };
        let pick = match want_ver {
            Some(v) => versions.iter().find(|e| e.version == v),
            None => versions.first(),
        };
        if let Some(e) = pick {
            found.push((rn.clone(), e));
        }
    }
    match found.len() {
        0 if repos.is_empty() => bail!(
            "`{name}` 不像 OCI 引用,而本机一个仓库都没配。\n\
             `crater repo add <名> <索引地址>`,或直接给完整引用。"
        ),
        0 => bail!(
            "仓库里没有 `{name}`{}。`crater search {n}` 看看有什么。",
            want_ver.map(|v| format!(" 的 {v} 版")).unwrap_or_default()
        ),
        1 => {
            offline_hint(&found[0].0, found[0].1);
            Ok(found[0].1.reference.clone())
        }
        _ => {
            let rs: Vec<&str> = found.iter().map(|(r, _)| r.as_str()).collect();
            bail!(
                "`{n}` 在 {} 个仓库里都有:{} —— 用 `--repo <名>` 指明是哪个。",
                found.len(),
                rs.join(", ")
            )
        }
    }
}

/// 解析中了一条**离线镜像**的条目、而字节还没进本地 store 时,说清楚
/// 那份字节就在哪个文件里。
///
/// 这是 `urls` 唯一的读处,也是它存在的理由。没有它,断网机上漏掉一步
/// `pkg load` 的表现是 install 去连一个连不上的 registry —— 一次长超时,
/// 外加一条把人引向防火墙和证书的报错。而正确的动作只是"把手边这个 tar
/// 收进来"。
///
/// 只在**有 urls** 时开口:带 urls 的索引按定义是随包一起搬过来的离线镜像,
/// 那种索引上字节不在本地就是漏了一步。在线仓库的条目没有 urls,不会被这
/// 一条打扰。
fn offline_hint(repo: &str, e: &Entry) {
    if e.urls.is_empty() {
        return;
    }
    if ImageStore::open().map(|s| s.has_all_layers(&e.reference)).unwrap_or(false) {
        return; // 字节已经在了,没什么好说的
    }
    // urls 是相对索引文件的路径 —— 本地/U 盘索引才解得出绝对位置。
    let base = load_repos()
        .repos
        .get(repo)
        .filter(|u| !u.starts_with("http://") && !u.starts_with("https://"))
        .map(|u| PathBuf::from(u.strip_prefix("file://").unwrap_or(u)))
        .and_then(|p| p.parent().map(|d| d.to_path_buf()));
    crate::oops!("{} 的字节还不在本地。", e.reference);
    for u in &e.urls {
        match base.as_ref().map(|d| d.join(u)) {
            // tar 在 → 就差一步 `pkg load`。
            Some(p) if p.exists() => crate::oops!("  先收进来:crater pkg load {}", p.display()),
            // 索引说有、盘上却没有 → **只拷了一半**。这是 U 盘搬运最常见的
            // 那种错(拷了几百字节的索引,漏了几百兆的包),而它唯一的症状
            // 本来是"install 连不上 registry" —— 指向完全错的方向。
            Some(p) => crate::oops!(
                "  仓库 {repo} 的索引说它在 {} —— 但那个文件不在。U 盘只拷了索引、漏了包?",
                p.display()
            ),
            None => crate::oops!("  仓库 {repo} 说它在 {u}(相对索引所在目录)"),
        }
    }
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // 只为可读性,不参与任何判定 —— 不值得为它引一个日期库。
    let days = secs / 86400;
    let (y, m, d) = civil_from_days(days as i64);
    let (hh, mm, ss) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

/// Howard Hinnant 的 civil_from_days —— 天数 → 年月日,无依赖。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(v: &str) -> Entry {
        Entry {
            version: v.into(),
            blueprint_version: String::new(),
            reference: format!("reg/ns/x:{v}"),
            digest: String::new(),
            description: String::new(),
            fleet: vec![],
            params: 0,
            platforms: vec![],
            urls: vec![],
        }
    }

    #[test]
    fn upsert_is_idempotent_and_keeps_newest_first() {
        // 索引是派生数据,重跑 `pkg index` 必须幂等 —— 否则每跑一次条目翻倍。
        let mut i = Index::default();
        i.upsert("x", e("1.2.0"));
        i.upsert("x", e("1.10.0"));
        i.upsert("x", e("1.2.0"));
        let vs: Vec<&str> = i.entries["x"].iter().map(|x| x.version.as_str()).collect();
        assert_eq!(vs, vec!["1.10.0", "1.2.0"]);
    }

    #[test]
    fn version_comes_from_the_tag_not_the_blueprint() {
        // 蓝图的 version 与 tag 不一致是常态(library/yq 声明 "1",tag 是
        // 工具版本 4.44.3)。按蓝图 version 组织会让同一包的多个 tag 互相
        // **静默**覆盖 —— 这个测试就是那次事故的封条。
        let cfg = serde_json::json!({ "name": "x", "version": "1" });
        let (n, ent) = entry_of("reg/ns/x:4.44.3", &cfg, vec![], String::new());
        assert_eq!(n, "x");
        assert_eq!(ent.version, "4.44.3");
        assert_eq!(ent.blueprint_version, "1");
    }

    /// 增量发布的正向判据:并进新版本后**老版本还在**,且 semver 降序。
    ///
    /// 与 `upsert_is_idempotent_*` 的区别是这条走了一趟盘 —— merge 的输入
    /// 不是内存里的 Index,而是上一次写出去的那个文件。
    #[test]
    fn merge_from_disk_keeps_the_old_versions() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.yaml");

        let mut first = Index::default();
        first.upsert("yq", e("4.40.5"));
        std::fs::write(&p, serde_yaml::to_string(&first).unwrap()).unwrap();

        // 下一次发布只知道自己这一版,历史全从文件里来。
        let mut merged = load_index_file(&p).unwrap();
        merged.upsert("yq", e("4.44.3"));
        merged.upsert("rustfs", e("1.0.0"));

        let vs: Vec<&str> = merged.entries["yq"].iter().map(|x| x.version.as_str()).collect();
        assert_eq!(vs, vec!["4.44.3", "4.40.5"], "老版本必须留着,且新的排前面");
        assert!(merged.entries.contains_key("rustfs"), "另一个包也要在");
    }

    /// **反证**:`--merge` 读到一份坏索引必须报错,不能退回空索引。
    ///
    /// 退回空索引不会有任何症状 —— 命令成功、文件写出、只是历史没了,
    /// 要等到有人装旧版本时才发现。这四种坏法都是真会发生的:写了一半被
    /// 打断(空 / 截断)、放错文件(不是索引)、旧版 crater 写的(apiVersion)。
    #[test]
    fn a_broken_index_is_an_error_never_an_empty_start() {
        let d = tempfile::tempdir().unwrap();
        let good = serde_yaml::to_string(&{
            let mut i = Index::default();
            i.upsert("yq", e("4.40.5"));
            i
        })
        .unwrap();

        let cases: [(&str, String); 4] = [
            ("empty", String::new()),
            ("garbage", "这不是索引\n".to_string()),
            // 截在半个 token 上(写盘被打断的常见样子)。
            ("truncated", "apiVersion: crater.pkg/v1\ngenerated: x\nentries:\n  yq:\n  - vers".into()),
            ("wrong_api", good.replace(API_VERSION, "crater.pkg/v99")),
        ];
        for (name, body) in cases {
            let p = d.path().join(format!("{name}.yaml"));
            std::fs::write(&p, body).unwrap();
            assert!(load_index_file(&p).is_err(), "{name}:坏索引却读成功了 —— merge 会把历史写没");
        }

        // 对照组:好索引照常读得出来,免得上面四条是因为读函数整个坏了才过。
        let p = d.path().join("good.yaml");
        std::fs::write(&p, &good).unwrap();
        assert_eq!(load_index_file(&p).unwrap().entries.len(), 1);
    }

    /// 截断**恰好落在 `entries:` 之后**的索引是合法 YAML —— 这里钉住的就是
    /// "解析得动"这个事实本身,好让人知道防线为什么不在 load_index_file 里。
    ///
    /// `entries` 变成 null、serde 收成空 map,于是 merge 从零开始、退出 0、
    /// 把整份历史换成这一版。真机上复现过。挡它的闸在 `index()`(空索引不是
    /// 合法的 merge 底本),端到端用例在 tests/pkg_index_cli.rs。
    #[test]
    fn a_truncation_at_the_entries_line_still_parses() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cut.yaml");
        std::fs::write(&p, "apiVersion: crater.pkg/v1\ngenerated: 2026-09-02T00:00:00Z\nentries:\n").unwrap();

        let idx = load_index_file(&p).expect("截断在 entries: 之后仍是合法 YAML");
        assert!(idx.entries.is_empty(), "它看起来就是一份'没有任何包'的好索引 —— 危险正在这里");
    }

    /// 索引落盘不留"写了一半"的中间态,也不留临时文件。
    ///
    /// 临时文件必须是**同目录的兄弟** —— 放 /tmp 的话跨文件系统 rename 会
    /// 直接失败,而这条路径在 CI 容器里天天走。
    #[test]
    fn writing_an_index_is_atomic_and_leaves_no_litter() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("index.yaml");

        write_atomically(&p, b"first").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"first");

        // 覆盖一份已有的,同样不留残渣。
        write_atomically(&p, b"second").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second");

        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n != "index.yaml")
            .collect();
        assert!(leftovers.is_empty(), "留下了临时文件:{leftovers:?}");
    }

    /// 目标目录不存在时要说清是**哪个路径**写不了。
    ///
    /// 之前 `fs::write` 的错误裸奔成 "No such file or directory (os error 2)",
    /// 在 CI 日志里根本看不出是 `-o` 指错了地方。
    #[test]
    fn a_write_into_a_missing_directory_names_the_path() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("nope").join("index.yaml");
        let e = write_atomically(&p, b"x").unwrap_err();
        assert!(format!("{e:#}").contains("nope"), "错误没指出路径:{e:#}");
    }

    /// 造一个最小的 oci-archive tar:只有一份 index.json,自报装着 `refs`。
    fn fake_archive(path: &std::path::Path, refs: &[&str]) {
        let index = serde_json::json!({
            "schemaVersion": 2,
            "manifests": refs.iter().map(|r| serde_json::json!({
                "digest": "sha256:0000",
                "annotations": { "org.opencontainers.image.ref.name": r }
            })).collect::<Vec<_>>()
        });
        let body = serde_json::to_vec(&index).unwrap();
        let f = std::fs::File::create(path).unwrap();
        let mut b = tar::Builder::new(f);
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        b.append_data(&mut h, "index.json", &body[..]).unwrap();
        b.finish().unwrap();
    }

    /// `urls` 记的是**归档自报的引用**对得上的那个 tar,不是名字像的那个。
    ///
    /// 按文件名猜会在同名不同版时把人指到错的包上,而那种错要到断网机上
    /// load 完、装出一个别的版本来才看得见 —— 正是 D-128 那一类静默。
    #[test]
    fn urls_point_at_the_tar_that_really_carries_the_bytes() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("index.yaml");

        let mut idx = Index::default();
        let mut a = e("4.44.3");
        a.reference = "reg/t/yq:4.44.3".into();
        let mut b = e("4.40.5");
        b.reference = "reg/t/yq:4.40.5".into();
        idx.upsert("yq", a);
        idx.upsert("yq", b);

        // 名字最像 4.44.3 的那个 tar 里装的其实是 4.40.5。
        fake_archive(&d.path().join("yq.pkg.tar"), &["reg/t/yq:4.40.5"]);
        // 无关的归档不该被记进任何条目。
        fake_archive(&d.path().join("other.pkg.tar"), &["reg/t/rustfs:1.0"]);

        assert_eq!(attach_local_tars(&mut idx, &out), 1, "只有一个 tar 对得上");
        let by_ver = |v: &str| idx.entries["yq"].iter().find(|x| x.version == v).unwrap().urls.clone();
        assert_eq!(by_ver("4.40.5"), vec!["yq.pkg.tar".to_string()], "按自报引用配对");
        assert!(by_ver("4.44.3").is_empty(), "名字像不算数 —— 会把人指到错的版本上");
    }

    /// 重跑 `pkg index` 时旧的 `urls` 要**先清掉**。
    ///
    /// 索引是派生数据:上一版记过的 tar 这一版可能已经不在同一个目录了。
    /// 留一条指向空气的路径,比没有这个字段更糟 —— 它会让人以为包就在手边。
    #[test]
    fn stale_urls_are_cleared_not_accumulated() {
        let d = tempfile::tempdir().unwrap();
        let out = d.path().join("index.yaml");
        let mut idx = Index::default();
        let mut a = e("4.44.3");
        a.reference = "reg/t/yq:4.44.3".into();
        a.urls = vec!["上一版留下的.pkg.tar".into()];
        idx.upsert("yq", a);

        // 目录里一个 tar 都没有 → urls 应该被清空,而不是留着上一版那条。
        assert_eq!(attach_local_tars(&mut idx, &out), 0);
        assert!(idx.entries["yq"][0].urls.is_empty(), "旧 urls 没清掉");

        // tar 出现后再记上,且不重复累积。
        fake_archive(&d.path().join("yq.pkg.tar"), &["reg/t/yq:4.44.3"]);
        attach_local_tars(&mut idx, &out);
        attach_local_tars(&mut idx, &out);
        assert_eq!(idx.entries["yq"][0].urls, vec!["yq.pkg.tar".to_string()]);
    }

    #[test]
    fn dates_render_correctly() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(20698), (2026, 9, 2));
        assert_eq!(civil_from_days(19723), (2024, 1, 1)); // 闰年边界
    }
}

// ───────────────────── UI 用的数据接口(不打印,只返回) ─────────────────────

/// 已配的仓库及其缓存状态:(名, 地址, 缓存里有几个包)。
pub fn repo_status() -> Vec<(String, String, Option<usize>)> {
    load_repos()
        .repos
        .into_iter()
        .map(|(n, u)| {
            let c = load_index_file(&cache_of(&n)).ok().map(|i| i.entries.len());
            (n, u, c)
        })
        .collect()
}

/// 全部缓存索引里的**最新版**条目:(仓库名, 包名, 条目)。
///
/// 只给最新版:目录是"能装什么"的墙,不是版本历史。要装旧版的人知道
/// 自己在做什么,走 `crater install <名>:<版本>`。
pub fn latest_entries() -> Vec<(String, String, Entry)> {
    let mut out = Vec::new();
    for (rn, idx) in cached() {
        for (name, versions) in idx.entries {
            if let Some(e) = versions.into_iter().next() {
                out.push((rn.clone(), name, e));
            }
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}
