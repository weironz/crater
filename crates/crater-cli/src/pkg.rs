//! `crater pkg` —— 把一份蓝图打成 OCI 制品,推上去、拉下来、看契约(D-123)。
//!
//! 这不是新造一套制品语法,是把旧 task 管线**已经真机验证过**的那套接给
//! 蓝图管线:自定义层类型过线保真(D-033)、按 digest 增量拉(D-078)、
//! 瘦拉只取需要的层(D-087)。新蓝图管线此前只有一个出口 —— 本地
//! `closure.tar`,一个只能靠 U 盘走的文件。
//!
//! 三条刻意的设计:
//!
//! - **制品身份放 `config.mediaType`,不设 `artifactType`。** 后者是 OCI 1.1
//!   写法,要配空的 config 描述符,而阿里云 ACR 会拒收 —— 与本仓库 buildx
//!   provenance 撞的是同一堵墙。1.0 写法五家 registry 全通。
//! - **config blob 就是契约本身**(参数/机群/物料清单)。于是"这东西要我给
//!   什么"只需 manifest + config 几百字节,一层都不用下载 —— `pkg inspect`
//!   与 UI 的远端目录都走这条。
//! - **凭据永远不进包。** inventory 是操作者侧的数据,而包是要推到 registry
//!   上给别人拉的。打包时全数排除并逐个报出来,余下的文件再扫一遍字面口令,
//!   撞上就**拒绝打包** —— 推上去之后再删是删不掉的。

use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use crater_core::store::{
    ImageStore, ANN_MATERIAL_FETCH, ANN_MATERIAL_NAME, ANN_MATERIAL_SOURCE, MT_MATERIAL,
    MT_PKG_CONFIG, MT_PKG_LAYER,
};
use crater_ir::ir::Blueprint;
use serde_json::json;

use crate::say;

const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const MT_INDEX: &str = "application/vnd.oci.image.index.v1+json";

// ───────────────────────── 契约(config blob 的内容) ─────────────────────────

/// 一份蓝图的**输入契约** —— 参数、机群、物料、规模。
///
/// 它同时是三处的同一份数据:`pkg` 的 config blob、UI 目录的卡片、
/// `crater inspect` 想印的东西。写成一处是因为它们本就该一致 ——
/// 表单按它渲染、机群按它对账、远端目录按它排列,三边各算一遍必然会漂。
pub fn contract(bp: &Blueprint) -> serde_json::Value {
    let params: Vec<serde_json::Value> = bp
        .params
        .values()
        .map(|p| {
            json!({
                "name": p.name,
                "type": crate::ui_catalog::param_type_json(&p.ty),
                "default": p.default.as_ref().map(crate::ui_catalog::yaml_to_json),
                "required": p.required,
                "secret": p.secret,
                "stage": if p.stage == crater_ir::schema::Stage::Build { "build" } else { "deploy" },
                "desc": p.desc,
            })
        })
        .collect();
    let fleet: Vec<serde_json::Value> = bp
        .fleet
        .groups
        .iter()
        .map(|(name, c)| json!({ "name": name, "min": c.min }))
        .collect();
    // 物料只登记"是什么、从哪来",不登记字节 —— 字节是第二阶段的物料层。
    let materials: Vec<serde_json::Value> = bp
        .materials
        .iter()
        .map(|m| json!({ "name": m.name, "kind": format!("{:?}", m.kind).to_lowercase() }))
        .collect();
    json!({
        "name": bp.name,
        "version": bp.version,
        "description": bp.description,
        "params": params,
        "fleet": fleet,
        "materials": materials,
        "counts": {
            "resources": bp.resources.len(),
            "procedures": bp.procedures.len(),
            "health": bp.health.len(),
            "custom_types": bp.types.len(),
        },
        // 不写 `min_version`:哪一版 crater 起能跑这份蓝图,谁也算不出来。
        // 记下"谁烤的"是能诚实回答的那部分 —— 拉包时版本更新就提醒一句。
        "crater": { "built_by": env!("CARGO_PKG_VERSION") },
    })
}

// ───────────────────────────── 打包 ─────────────────────────────

/// 这个文件该不该进包。
///
/// 排除的是**操作者侧**与**派生物**:凭据、生成的 app 文件、闭包、备份。
/// 包是作者交出去的东西,里面只该有作者写的那些。
fn packable(rel: &Path) -> Option<&'static str> {
    let name = rel.file_name()?.to_string_lossy().to_ascii_lowercase();
    if name.starts_with("inventory") && (name.ends_with(".yaml") || name.ends_with(".yml")) {
        return Some("凭据");
    }
    if name.ends_with(".app.yaml") || name.ends_with(".app.yml") {
        return Some("部署实例");
    }
    if name.ends_with(".closure.tar") || name.ends_with(".oci") || name.ends_with(".pkg.tar") {
        return Some("制品");
    }
    if name.ends_with(".bak") || name.ends_with('~') {
        return Some("备份");
    }
    None
}

/// 走一遍目录,收出要进包的文件(相对路径, 字节, mode)与被排除的清单。
fn collect(root: &Path) -> Result<(Vec<(String, Vec<u8>, u32)>, Vec<(String, &'static str)>)> {
    use std::os::unix::fs::PermissionsExt;
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<PathBuf> =
            std::fs::read_dir(&dir)?.flatten().map(|e| e.path()).collect();
        entries.sort(); // 可复现:目录序不该影响包的 digest
        for p in entries {
            let base = p.file_name().unwrap_or_default().to_string_lossy().into_owned();
            if base.starts_with('.') || matches!(base.as_str(), "target" | "node_modules") {
                continue;
            }
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(root).unwrap_or(&p).to_path_buf();
            let rels = rel.display().to_string();
            if let Some(why) = packable(&rel) {
                skipped.push((rels, why));
                continue;
            }
            let data = std::fs::read(&p).with_context(|| format!("读 {}", p.display()))?;
            let mode = std::fs::metadata(&p)?.permissions().mode() & 0o7777;
            files.push((rels, data, mode));
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok((files, skipped))
}

/// 扫一遍要进包的文本,撞上字面口令就拒绝。
///
/// 排除 inventory 已经挡住了绝大多数,这一道是兜底:模板里也能写死口令,
/// 而模板是一定要进包的。**硬错误而不是告警** —— 推上 registry 之后
/// "先换口令再谈清理"是唯一的补救,代价与一次告警不在一个量级。
fn refuse_literal_secrets(files: &[(String, Vec<u8>, u32)]) -> Result<()> {
    for (rel, data, _) in files {
        let Ok(text) = std::str::from_utf8(data) else { continue };
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            for key in ["password:", "passwd:", "secret_key:", "token:"] {
                let Some(v) = t.strip_prefix(key) else { continue };
                let v = v.trim().trim_matches(['"', '\'']);
                // 不是字面口令的四种写法,都放行:
                // - 空值(块状写法,真值在下一行,由那一行自己受检)
                // - `${env:…}` / `{{ … }}` 插值 —— 正是我们希望看到的写法
                // - 以 `{` 开头的映射:这是**声明**不是值。蓝图的
                //   `secret_key: { default: "changeme", desc: … }` 是在登记
                //   一个参数,把它当成泄漏会让每份带敏感参数的蓝图都打不了包。
                if v.is_empty() || v.starts_with("${") || v.starts_with('{') {
                    continue;
                }
                bail!(
                    "{rel}:{} 写着字面口令 `{key} {v}` —— 拒绝打包。\n\
                     包是要推到 registry 上的,推上去就删不掉了。\n\
                     改成 `${{env:VAR}}`、`password_file:` 或模板变量再来。",
                    i + 1
                );
            }
        }
    }
    Ok(())
}

/// 给一个路径,定位蓝图文件与包根目录。
///
/// 给目录:找里面唯一的 `*.blueprint.yaml`;给文件:根目录取它的父目录 ——
/// 包的边界是**目录**,因为模板与静态文件都住在蓝图旁边。
fn locate(path: &Path) -> Result<(PathBuf, PathBuf)> {
    if path.is_dir() {
        let mut found: Vec<PathBuf> = std::fs::read_dir(path)?
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_file() && crate::blueprint::is_blueprint_file(p))
            .collect();
        found.sort();
        match found.len() {
            0 => bail!("{} 里没有蓝图文件(*.blueprint.yaml)", path.display()),
            1 => Ok((found.remove(0), path.to_path_buf())),
            _ => bail!(
                "{} 里有 {} 份蓝图 —— 一个包只装一份,请直接指定文件",
                path.display(),
                found.len()
            ),
        }
    } else {
        if !crate::blueprint::is_blueprint_file(path) {
            bail!("{} 不是蓝图文件(*.blueprint.yaml)", path.display());
        }
        let root = path.parent().unwrap_or(Path::new(".")).to_path_buf();
        Ok((path.to_path_buf(), root))
    }
}

/// 组装制品并落进本地 store。
///
/// 落本地再推,而不是边算边推:于是"推上去的"和"本地留着的"是同一份字节,
/// `pkg ls` 看见的就是对方会拉到的。
///
/// `archs` 非空即带闭包:每个架构烤一遍物料,做成各自的层。**蓝图层与
/// config 跨架构是同一个 digest** —— registry 按内容寻址,只存一份,
/// 多一个架构只多它自己的物料字节。给了两个及以上架构就产出 image index。
async fn assemble(
    path: &Path,
    reference: &str,
    archs: &[String],
    fors: &[String],
) -> Result<()> {
    let (bp_file, root) = locate(path)?;
    let bp = crate::blueprint::load(&bp_file)?;
    let (files, skipped) = collect(&root)?;
    if files.is_empty() {
        bail!("{} 里没有可打包的文件", root.display());
    }
    refuse_literal_secrets(&files)?;

    let store = ImageStore::open()?;
    let cfg = serde_json::to_vec_pretty(&contract(&bp))?;
    let layer = crater_core::bundle::tar_gz_files(&files)?;
    let (cfg_d, cfg_s) = store.put_blob(&cfg)?;
    let (lay_d, lay_s) = store.put_blob(&layer)?;

    say!("包 {reference} —— {} 个文件,{}", files.len(), human(lay_s));
    // tag 与蓝图 version 不一致只是**提醒**,不是错误:两种约定都合理
    // (Helm 也分 chart version 与 appVersion),而索引按 tag 组织,
    // 不会因此错乱。提醒的价值在于——不一致往往是手滑打错了 tag。
    if let Some(bv) = bp.version.as_deref() {
        let tag = reference.rsplit(':').next().unwrap_or("");
        if !tag.is_empty() && tag != bv {
            say!("  · tag `{tag}` 与蓝图 version `{bv}` 不同 —— 索引与 install 按 tag 走");
        }
    }
    for (rel, why) in &skipped {
        say!("  · 排除 {rel}({why})");
    }

    let created = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let ann = json!({
        "org.opencontainers.image.title": bp.name,
        "org.opencontainers.image.version": bp.version.clone().unwrap_or_default(),
        "org.opencontainers.image.description": bp.description.clone().unwrap_or_default(),
        "org.opencontainers.image.created": created.to_string(),
    });
    let cfg_desc = json!({
        "mediaType": MT_PKG_CONFIG, "digest": format!("sha256:{cfg_d}"), "size": cfg_s
    });
    let bp_layer = json!({
        "mediaType": MT_PKG_LAYER,
        "digest": format!("sha256:{lay_d}"),
        "size": lay_s,
        // ORAS 惯例:带上文件名,`oras pull` 也能当逃生通道把包取出来。
        "annotations": { "org.opencontainers.image.title": format!("{}.tar.gz", bp.name) }
    });

    // 不带闭包:一份 manifest,与 Helm 的布局同形。
    if archs.is_empty() {
        let m = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST,
            "config": cfg_desc, "layers": [bp_layer], "annotations": ann
        });
        store.put_manifest(reference, &serde_json::to_vec(&m)?)?;
        return Ok(());
    }

    // 带闭包:逐架构烤。`seen` 跨架构复用 —— 两个架构共用的物料(证书、
    // 配置模板)只下载一次,两份 manifest 引用同一个 digest。
    let mut seen = std::collections::BTreeMap::new();
    let mut per_arch: Vec<(String, serde_json::Value)> = Vec::new();
    let mut total_mat = 0u64;
    for a in archs {
        let mut profile: Vec<String> = fors.to_vec();
        profile.push(format!("arch={a}"));
        say!();
        say!("── {a} ──");
        let (baked, skip) =
            crate::closure::bake_bytes(&bp_file, &profile, &Default::default(), &mut seen).await?;
        for s in &skip {
            say!("  · 跳过 {s}");
        }
        let mut layers = vec![bp_layer.clone()];
        for b in &baked {
            let sha = crater_core::bundle::sha256_hex(&b.bytes);
            let (d, sz) = store.put_blob(&b.bytes)?;
            total_mat += sz;
            say!("  ✓ {:<28} {:>9}  {}", b.name, human(sz), &sha[..12]);
            layers.push(json!({
                "mediaType": MT_MATERIAL,
                "digest": format!("sha256:{d}"),
                "size": sz,
                "annotations": {
                    // 物料是**外部字节**,不随瘦拉走:在线部署时目标机自己按
                    // URL 取,几百兆的层留在 registry 里(D-087 的老约定)。
                    ANN_MATERIAL_FETCH: "dependency",
                    ANN_MATERIAL_NAME: b.name,
                    // 部署侧的 BlobMap 按**渲染后的 URL** 索引,不按物料名 ——
                    // 同名物料按 `when:` 分成多个变体,各有各的 URL。
                    ANN_MATERIAL_SOURCE: b.source,
                    "org.opencontainers.image.title": b.name,
                }
            }));
        }
        // 这个架构一个物料都没烤出来(全被上一个架构的 seen 挡掉了),说明
        // 该蓝图的物料与架构无关 —— 那就没有必要为它单开一份 manifest。
        let m = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST,
            "config": cfg_desc, "layers": layers, "annotations": ann
        });
        per_arch.push((a.clone(), m));
    }

    if per_arch.len() == 1 {
        let (_, m) = per_arch.remove(0);
        store.put_manifest(reference, &serde_json::to_vec(&m)?)?;
        say!();
        say!("闭包 {} —— 一份 manifest", human(total_mat));
        return Ok(());
    }

    // 多架构 → image index。`platform` 是 OCI 定义的变体选择字段,
    // 所有 registry 与运行时都懂它;用注解自造一套只有 crater 认得。
    let mut entries = Vec::new();
    for (a, m) in &per_arch {
        let bytes = serde_json::to_vec(m)?;
        let (d, sz) = store.put_blob(&bytes)?;
        entries.push(json!({
            "mediaType": MT_MANIFEST,
            "digest": format!("sha256:{d}"),
            "size": sz,
            "platform": { "architecture": a, "os": "linux" }
        }));
    }
    let index = json!({
        "schemaVersion": 2, "mediaType": MT_INDEX,
        "manifests": entries, "annotations": ann
    });
    store.put_manifest(reference, &serde_json::to_vec(&index)?)?;
    say!();
    say!("闭包 {} —— {} 个架构,index 一个 tag 装下", human(total_mat), per_arch.len());
    Ok(())
}

// ───────────────────────────── 命令 ─────────────────────────────

/// `crater pkg build <路径> -t <ref>` —— 只组装,不推。
pub async fn build(path: &Path, reference: &str, archs: &[String], fors: &[String]) -> Result<()> {
    assemble(path, reference, archs, fors).await?;
    say!("已入本地 store —— `crater pkg push {reference}` 推上去");
    Ok(())
}

/// `crater pkg push <路径> <ref>` —— 组装并推。
pub async fn push(path: &Path, reference: &str, archs: &[String], fors: &[String]) -> Result<()> {
    assemble(path, reference, archs, fors).await?;
    let store = ImageStore::open()?;
    store.push(reference).await?;
    say!("推送完成 → {reference}");
    Ok(())
}

/// 一个已拉全的包里的物料字节 → 部署侧的 `BlobMap`(源 URL → 本地路径)。
///
/// 不解包、不复制:store 的 blob 本来就是内容寻址的文件,直接把路径交出去。
/// 这是 D-119 说的"第二个 blob 后端",而它没让 `BlobSource` 多一个方法。
pub fn blobs_of(reference: &str) -> Result<crate::material_ctx::BlobMap> {
    let store = ImageStore::open()?;
    let m = store.resolve_manifest(reference)?;
    let mut map = crate::material_ctx::BlobMap::new();
    let mut missing = Vec::new();
    for l in m["layers"].as_array().into_iter().flatten() {
        if l["mediaType"].as_str() != Some(MT_MATERIAL) {
            continue;
        }
        let Some(src) = l["annotations"][ANN_MATERIAL_SOURCE].as_str() else { continue };
        let d = l["digest"].as_str().unwrap_or_default().trim_start_matches("sha256:");
        let p = store.blob_path(d);
        // 瘦拉过的包物料层不在本地。报出来而不是静默少一条 —— 少一条的表现
        // 是"部署时目标机自己去联网下载",在断网现场就是装不上。
        if p.exists() {
            map.insert(src.to_string(), p);
        } else {
            missing.push(src.to_string());
        }
    }
    if !missing.is_empty() {
        bail!(
            "{reference} 的 {} 份物料不在本地(瘦拉的包只有蓝图层)。\n\
             先 `crater pkg pull {reference} --full`。缺:{}",
            missing.len(),
            missing.join(", ")
        );
    }
    Ok(map)
}

/// `crater pkg pull <ref> [--into DIR] [--full]` —— 拉下来并摊回文件。
///
/// 默认**瘦拉**:manifest + config + 蓝图层。物料层(第二阶段)留在 registry,
/// 部署时目标机自己按 URL 取 —— 在线部署根本用不到那几百兆。
pub async fn pull(reference: &str, into: Option<&Path>, full: bool) -> Result<()> {
    let store = ImageStore::open()?;
    if full {
        store.pull(reference).await?;
    } else {
        store.pull_thin(reference).await?;
    }
    let m = store.resolve_manifest(reference)?;
    let cfg = read_config(&store, &m)?;
    warn_if_newer(&cfg);
    let name = cfg["name"].as_str().unwrap_or("pkg").to_string();
    let dir = into.map(|d| d.to_path_buf()).unwrap_or_else(|| PathBuf::from(&name));
    if dir.exists() && std::fs::read_dir(&dir)?.next().is_some() {
        bail!("{} 已存在且非空 —— 换个 --into,或先移开", dir.display());
    }
    let layer = m["layers"]
        .as_array()
        .and_then(|ls| ls.iter().find(|l| l["mediaType"].as_str() == Some(MT_PKG_LAYER)))
        .ok_or_else(|| anyhow::anyhow!("{reference} 不是 crater 蓝图包(没有蓝图层)"))?;
    let d = layer["digest"].as_str().unwrap_or_default();
    let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:")))
        .with_context(|| format!("{reference} 的蓝图层不在本地"))?;
    crater_core::bundle::untar_gz_into(&dir, &bytes, 0)?;
    stamp(&dir, reference)?;
    let n = std::fs::read_dir(&dir).map(|r| r.flatten().count()).unwrap_or(0);
    say!("{reference} → {}({n} 项)", dir.display());
    let mats = m["layers"]
        .as_array()
        .map(|ls| ls.iter().filter(|l| l["mediaType"].as_str() == Some(MT_MATERIAL)).count())
        .unwrap_or(0);
    if full && mats > 0 {
        say!("闭包 {mats} 份物料随包带下 —— 断网部署:");
        say!("  crater apply -f {}/... -i <机群> --closure oci://{reference}", dir.display());
    }
    print_contract(&cfg);
    Ok(())
}

/// `crater pkg inspect <ref>` —— 只拉 manifest + config,一层都不下载。
pub async fn inspect(reference: &str) -> Result<()> {
    // 本地有就读本地,省一次往返;没有才问 registry。
    let store = ImageStore::open()?;
    let cfg = match store.resolve_manifest(reference).ok().and_then(|m| read_config(&store, &m).ok()) {
        Some(c) => {
            say!("(本地 store)");
            c
        }
        None => {
            let (_m, bytes, _plats) = ImageStore::fetch_contract(reference).await?;
            serde_json::from_slice(&bytes)
                .with_context(|| format!("{reference} 的 config 不是 crater 契约"))?
        }
    };
    print_contract(&cfg);
    Ok(())
}

/// `crater pkg tags <ref>` —— 远端有哪些版本。
pub async fn tags(reference: &str) -> Result<()> {
    let mut tags = ImageStore::list_tags(reference).await?;
    if tags.is_empty() {
        say!("{reference}:一个 tag 都没有");
        return Ok(());
    }
    // semver 降序:人问"有哪些版本"时想先看见最新的那个。
    tags.sort_by(|a, b| semver_key(b).cmp(&semver_key(a)));
    for t in &tags {
        // OCI tag 里不能有 `+`,推的时候换成了 `_`,读回来换回去。
        say!("  {}", t.replace('_', "+"));
    }
    say!();
    say!("{} 个版本。`crater pkg inspect <ref>:<版本>` 看契约。", tags.len());
    Ok(())
}

/// `crater pkg ls` —— 本地 store 里的蓝图包。
pub fn ls() -> Result<()> {
    let store = ImageStore::open()?;
    let all = store.list()?;
    let mut rows: Vec<(String, String, String, u64, usize)> = Vec::new();
    for img in &all {
        let Ok(m) = store.resolve_manifest(&img.reference) else { continue };
        if m["config"]["mediaType"].as_str() != Some(MT_PKG_CONFIG) {
            continue; // 不是蓝图包(旧 task 制品、普通镜像)—— `crater images` 管那些
        }
        let cfg = read_config(&store, &m).unwrap_or(json!({}));
        let mats = m["layers"]
            .as_array()
            .map(|ls| ls.iter().filter(|l| l["mediaType"].as_str() == Some(MT_MATERIAL)).count())
            .unwrap_or(0);
        rows.push((
            img.reference.clone(),
            cfg["name"].as_str().unwrap_or("").to_string(),
            cfg["description"].as_str().unwrap_or("").to_string(),
            img.content_size,
            if store.has_all_layers(&img.reference) { mats } else { usize::MAX },
        ));
    }
    if rows.is_empty() {
        say!("本地还没有蓝图包。`crater pkg push <蓝图目录> <ref>` 做一个,\
              或 `crater pkg pull <ref>` 拉一个。");
        return Ok(());
    }
    rows.sort();
    let w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0);
    for (r, _n, desc, size, mats) in &rows {
        // 瘦拉的包照样能在线部署(蓝图层全在),标注只是说"物料层还在
        // registry" —— 断网现场要的是另一种。
        let mark = match *mats {
            usize::MAX => "  (瘦)".to_string(),
            0 => String::new(),
            n => format!("  (闭包 {n} 份)"),
        };
        say!("  {:<w$}  {:>9}{}  {}", r, human(*size), mark, desc, w = w);
    }
    Ok(())
}

// ───────────────────────────── 小工具 ─────────────────────────────

/// 读 config;不是 crater 包(或读不动)就是 None —— 调用方多半在遍历
/// 一整个 store,一条读不动不该让整趟失败。
pub(crate) fn config_of(store: &ImageStore, m: &serde_json::Value) -> Option<serde_json::Value> {
    if m["config"]["mediaType"].as_str() != Some(MT_PKG_CONFIG) {
        return None;
    }
    read_config(store, m).ok()
}

/// 摊出来的包目录里留一行"我是从哪来的"。
///
/// 点开头,所以 [`packable`] 会把它排除在外 —— 再打包时不会把上一次的
/// 来源带进新包。
const STAMP: &str = ".crater-pkg";

fn stamp(dir: &Path, reference: &str) -> Result<()> {
    std::fs::write(dir.join(STAMP), format!("{reference}\n"))?;
    Ok(())
}

/// 这个目录是哪条引用摊出来的。没有印记(手写的蓝图目录)返回 None,
/// 那种情况下沿用旧行为 —— 人自己的目录,不该由我们判定"版本不对"。
fn stamped(dir: &Path) -> Option<String> {
    std::fs::read_to_string(dir.join(STAMP)).ok().map(|s| s.trim().to_string())
}

fn read_config(store: &ImageStore, m: &serde_json::Value) -> Result<serde_json::Value> {
    let d = m["config"]["digest"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("manifest 没有 config"))?;
    let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:")))?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// 包是更新的 crater 烤的 —— 提醒一句就够,不拦。
///
/// 拦是拦不住的:蓝图能不能跑取决于用到了哪些特性,不取决于版本号大小。
/// 但装到一半报语法错时,人第一个想知道的就是"是不是我的 crater 太老"。
fn warn_if_newer(cfg: &serde_json::Value) {
    let by = cfg["crater"]["built_by"].as_str().unwrap_or_default();
    let mine = env!("CARGO_PKG_VERSION");
    if !by.is_empty() && semver_key(by) > semver_key(mine) {
        crate::oops!("这个包由 crater {by} 打出,本机是 {mine} —— 用到新特性时会报解析错误。");
    }
}

/// semver 排序键。
///
/// 两处不显然的地方:主版本段**补齐到四位**,否则 `1.2` 会排在 `1.2.0`
/// 之前(短的 Vec 更小);预发布段在正式版**之后**读到一个哨兵,于是
/// `1.2.0-rc1 < 1.2.0` —— 少了这一条,`pkg tags` 会把 rc 当成最新版推荐。
pub(crate) fn semver_key(v: &str) -> Vec<i64> {
    let v = v.trim_start_matches('v');
    let (core, pre) = match v.find(['-', '+', '_']) {
        Some(i) => (&v[..i], &v[i + 1..]),
        None => (v, ""),
    };
    let mut key: Vec<i64> = core.split('.').map(|p| p.parse::<i64>().unwrap_or(-1)).collect();
    key.resize(4, 0);
    if pre.is_empty() {
        key.push(i64::MAX); // 正式版胜过它的任何预发布
    } else {
        key.push(0);
        key.extend(pre.split(['.', '-', '_', '+']).map(|p| p.parse::<i64>().unwrap_or(-1)));
    }
    key
}

fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "K", "M", "G"];
    let mut f = n as f64;
    let mut i = 0;
    while f >= 1024.0 && i < 3 {
        f /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{f:.1} {}", U[i]) }
}

/// 把契约印成人能读的样子 —— 与 `crater inspect` 同一个问题的同一个答案。
fn print_contract(cfg: &serde_json::Value) {
    let ver = cfg["version"].as_str().map(|v| format!("  v{v}")).unwrap_or_default();
    say!();
    say!("蓝图 {}{ver}", cfg["name"].as_str().unwrap_or("?"));
    if let Some(d) = cfg["description"].as_str().filter(|s| !s.is_empty()) {
        say!("{d}");
    }
    if let Some(ps) = cfg["params"].as_array().filter(|a| !a.is_empty()) {
        say!();
        say!("参数:");
        let w = ps.iter().map(|p| p["name"].as_str().unwrap_or("").len()).max().unwrap_or(0);
        // 必填排前面 —— 读的人最先要知道"我非填不可的是什么"。
        let mut sorted: Vec<&serde_json::Value> = ps.iter().collect();
        sorted.sort_by_key(|p| (!p["default"].is_null(), p["name"].as_str().unwrap_or("").to_string()));
        for p in sorted {
            let need = if p["default"].is_null() {
                "必填".to_string()
            } else {
                format!("默认 {}", compact(&p["default"]))
            };
            let stage = if p["stage"].as_str() == Some("build") { " [构建期]" } else { "" };
            say!(
                "  {:<w$}  {:<18}  {}{stage}",
                p["name"].as_str().unwrap_or(""),
                need,
                p["desc"].as_str().unwrap_or(""),
                w = w
            );
        }
    }
    if let Some(gs) = cfg["fleet"].as_array().filter(|a| !a.is_empty()) {
        say!();
        say!("需要的机群:");
        for g in gs {
            let min = g["min"].as_u64().unwrap_or(0);
            let n = if min == 0 { "可为空".to_string() } else { format!("至少 {min} 台") };
            say!("  {:<16}  {n}", g["name"].as_str().unwrap_or(""));
        }
    }
    let c = &cfg["counts"];
    say!();
    say!(
        "资源 {} 项 · 物料 {} 份 · 自定义类型 {} 个 · 健康探针 {} 条",
        c["resources"].as_u64().unwrap_or(0),
        cfg["materials"].as_array().map(|a| a.len()).unwrap_or(0),
        c["custom_types"].as_u64().unwrap_or(0),
        c["health"].as_u64().unwrap_or(0)
    );
}

fn compact(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_and_derivatives_never_enter_a_package() {
        // 包要推到 registry 上,凭据进去就删不掉了 —— 这条排除是安全边界,
        // 不是整洁癖,所以值一个测试。
        assert!(packable(Path::new("inventory.yaml")).is_some());
        assert!(packable(Path::new("inventory.example.yaml")).is_some());
        assert!(packable(Path::new("prod.app.yaml")).is_some());
        assert!(packable(Path::new("k8s.closure.tar")).is_some());
        // 作者写的东西照常进包。
        assert!(packable(Path::new("yq.blueprint.yaml")).is_none());
        assert!(packable(Path::new("templates/rustfs.env.j2")).is_none());
        assert!(packable(Path::new("README.md")).is_none());
    }

    #[test]
    fn literal_secret_refuses_but_interpolation_passes() {
        let bad = vec![("t/a.j2".into(), b"password: hunter2\n".to_vec(), 0o644)];
        assert!(refuse_literal_secrets(&bad).is_err());
        let ok = vec![
            ("t/a.j2".into(), b"password: \"${env:PW}\"\n".to_vec(), 0o644),
            ("t/b.j2".into(), b"password: {{ params.pw }}\n".to_vec(), 0o644),
            // 参数**声明**不是泄漏 —— 少了这一条,每份带敏感参数的蓝图都打不了包。
            (
                "x.blueprint.yaml".into(),
                b"    secret_key: { default: \"changeme\", secret: true }\n".to_vec(),
                0o644,
            ),
            ("t/c.j2".into(), b"password:\n".to_vec(), 0o644),
        ];
        assert!(refuse_literal_secrets(&ok).is_ok());
    }

    #[test]
    fn credential_shaped_names_stay_out_of_the_app_file() {
        // 蓝图漏标 `secret:` 是真会发生的(本仓库的 rustfs 就漏过),而 app
        // 文件是要进 git 的 —— 这条兜底比"相信声明"重要。
        assert!(looks_secret("secret_key"));
        assert!(looks_secret("root_password"));
        assert!(looks_secret("API_TOKEN"));
        // 不该误伤的:端口、路径、公钥路径不是凭据。
        assert!(!looks_secret("port"));
        assert!(!looks_secret("data_dir"));
        assert!(!looks_secret("version"));
    }

    #[test]
    fn semver_sorts_newest_first_and_prerelease_below_release() {
        let mut v = vec!["1.2.0", "1.10.0", "1.2.0-rc1", "0.9.9", "1.2"];
        v.sort_by(|a, b| semver_key(b).cmp(&semver_key(a)));
        assert_eq!(v, vec!["1.10.0", "1.2.0", "1.2", "1.2.0-rc1", "0.9.9"]);
    }
}

// ───────────────────────────── install ─────────────────────────────

/// `crater install <引用|目录> -i <机群> [--set k=v]… [--yes]`
///
/// 把散在五条命令里的动作串成一条:拉包 → 读契约 → 对账机群 → 落 app 文件
/// → plan。**闸门一步不省** —— `--yes` 才继续 apply,否则停在计划上。
/// "一键"省掉的是找包、抄参数、比对组名那几步,不是"先看 diff 再动手"。
///
/// 顺序是刻意的:**契约与机群都在本地对完账,才连第一台机器**。参数少给一个、
/// 组名写错一个,在 SSH 之前就该说清楚 —— 那时候纠正的代价是改一行命令,
/// 连上之后再发现就已经在改机器了。
#[allow(clippy::too_many_arguments)]
pub async fn install(
    source: &str,
    target: &crate::target::TargetOpts,
    sets: &[String],
    name: Option<&str>,
    repo: Option<&str>,
    yes: bool,
    full: bool,
) -> Result<()> {
    // ① 取到蓝图 —— 本地路径就地用,否则当成 registry 引用拉下来。
    let local = Path::new(source);
    // `crater install mysql` 里的 `mysql` 不是引用,是包名 —— 去仓库索引里
    // 查它对应哪条引用。判据是"有没有 `/`":OCI 引用一定带仓库路径,
    // 而包名一定不带。
    let resolved;
    let source = if !local.exists() && !source.contains('/') {
        resolved = crate::repo::resolve(source, repo)?;
        say!("{source} → {resolved}");
        resolved.as_str()
    } else {
        source
    };
    let bp_file = if local.exists() {
        locate(local)?.0
    } else {
        let store = ImageStore::open()?;
        if full { store.pull(source).await? } else { store.pull_thin(source).await? }
        let m = store.resolve_manifest(source)?;
        let cfg = read_config(&store, &m)?;
        warn_if_newer(&cfg);
        let pkg_name = cfg["name"].as_str().unwrap_or("pkg").to_string();
        let dir = PathBuf::from(&pkg_name);
        if !dir.exists() {
            let layer = m["layers"]
                .as_array()
                .and_then(|ls| ls.iter().find(|l| l["mediaType"].as_str() == Some(MT_PKG_LAYER)))
                .ok_or_else(|| anyhow::anyhow!("{source} 不是 crater 蓝图包"))?;
            let d = layer["digest"].as_str().unwrap_or_default();
            let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:")))?;
            crater_core::bundle::untar_gz_into(&dir, &bytes, 0)?;
            stamp(&dir, source)?;
            say!("{source} → {}/", dir.display());
        } else {
            // 目录已在:默认用本地那份(它可能有本地改动,重装不该冲掉)。
            //
            // 但**必须先确认它就是这次要装的那个版本**:包目录按包名命名,
            // `install yq:4.40.5` 会撞上上次 `install yq` 摊下的 4.44.3,
            // 然后**静默**装成 4.44.3 —— 引用解析对了、日志也对,装上去的
            // 却是另一版。这一条是那次事故的封条。
            match stamped(&dir) {
                Some(prev) if prev != source => bail!(
                    "{} 里是 `{prev}`,这次要装的是 `{source}` —— 版本对不上。\n\
                     换个位置:`crater pkg pull {source} --into <目录>`,\n\
                     或先把 {} 移开。",
                    dir.display(),
                    dir.display()
                ),
                _ => say!("{} 已存在 —— 用本地这份(包已在 store)", dir.display()),
            }
        }
        locate(&dir)?.0
    };

    let bp = crate::blueprint::load(&bp_file)?;
    let app_name = name.unwrap_or(&bp.name).to_string();
    let given: std::collections::BTreeMap<String, serde_yaml::Value> =
        crate::blueprint::parse_sets(sets)?.into_iter().collect();

    // ② 契约对账:必填参数一个都不能缺。
    //
    // 刻意**不交互追问** —— 部署工具在管道里挂住等输入是最难查的一类故障。
    // 缺什么就连同现成的 `--set` 一起报出来,人贴一行就能重来。
    let missing: Vec<&str> = bp
        .params
        .values()
        .filter(|p| p.required && p.default.is_none() && !given.contains_key(&p.name))
        .map(|p| p.name.as_str())
        .collect();
    if !missing.is_empty() {
        let flags: Vec<String> = missing.iter().map(|n| format!("--set {n}=…")).collect();
        bail!(
            "{} 还缺 {} 个必填参数:{}\n补上再来:crater install {source} … {}",
            bp.name,
            missing.len(),
            missing.join(", "),
            flags.join(" ")
        );
    }

    // ③ 机群对账 —— 在连机器**之前**。组名写错时,这里说"没有 storage 组",
    // 连上之后才发现则要从一堆 SSH 报错里往回猜。
    if !bp.fleet.groups.is_empty() {
        let inv_path = target
            .inventory
            .clone()
            .ok_or_else(|| anyhow::anyhow!("{} 声明了机群契约,需要 `-i <inventory>`", bp.name))?;
        let text = std::fs::read_to_string(&inv_path)
            .with_context(|| format!("读机群 {}", inv_path.display()))?;
        let spec: crater_core::spec::CraterSpec = serde_yaml::from_str(&text)?;
        let inv = spec.inventory;
        let mut bad = Vec::new();
        say!("机群对账({}):", inv_path.display());
        for (g, c) in &bp.fleet.groups {
            let declared = inv.groups.contains_key(g);
            let have = inv.groups.get(g).map(|x| x.hosts.len()).unwrap_or(0);
            // 三种状态要分得开:没这个组 / 有但台数不够 / 满足。
            // "没有 worker 组"与"worker 组是空的"是两回事 —— 后者在
            // `min: 0` 的单节点拓扑里完全合法。
            let (mark, why) = if !declared {
                ("✗", format!("机群里没有 `{g}` 组"))
            } else if have < c.min {
                ("✗", format!("`{g}` 只有 {have} 台,要 {} 台", c.min))
            } else {
                ("✓", String::new())
            };
            say!("  {mark} {:<16} 需要 {:<3} 现有 {}", g, c.min, have);
            if !why.is_empty() {
                bad.push(why);
            }
        }
        if !bad.is_empty() {
            bail!(
                "机群不满足契约,一台机器都没碰:\n  {}\n\
                 改 {} 里的组,或用 `crater pkg inspect` 再看一遍契约。",
                bad.join("\n  "),
                inv_path.display()
            );
        }
    }

    // ④ 落 app 文件 —— 这次安装的正身,可 git 可 diff。
    write_app(&app_name, &bp_file, target, &bp, &given)?;

    // ⑤ 闸门。
    //
    // `--full` 拉来的包自带物料层,那就是这次安装的闭包 —— 断网现场
    // 不必再单独准备一个 closure.tar。用户显式给了 `--closure` 则尊重他的。
    let mut target = target.clone();
    if full && target.closure.is_none() && !local.exists() {
        target.closure = Some(PathBuf::from(format!("oci://{source}")));
    }
    let target = &target;
    say!();
    crate::blueprint::plan_blueprint(&bp_file, target, sets).await?;
    if !yes {
        say!();
        say!("以上是计划,**什么都没改**。确认后执行:");
        say!("  crater apply -f {} -i {} {}",
            bp_file.display(),
            target.inventory.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|| "<机群>".into()),
            sets.iter().map(|s| format!("--set {s}")).collect::<Vec<_>>().join(" "));
        say!("  (或重跑本条命令加 --yes)");
        return Ok(());
    }
    say!();
    crate::blueprint::apply_blueprint(&bp_file, target, sets).await
}

/// 名字看起来像凭据 —— 蓝图**没标** `secret:` 时的兜底。
///
/// 为什么要兜底:`install` 拉的是别人做的包,而那份包的作者有没有认真标
/// `secret:` 不由我们决定 —— 本仓库自己的 rustfs 蓝图就漏标过。写进 app
/// 文件的东西是要进 git 的,而 git 历史删不掉:**宁可多扣一个,不可漏放
/// 一个**。扣下来的会连同"下次怎么给"一起报出来,不是静默丢弃。
fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["password", "passwd", "secret", "token", "apikey", "api_key", "credential"]
        .iter()
        .any(|k| n.contains(k))
}

/// 写 `<名>.app.yaml`:这次安装绑定了哪份蓝图、哪个机群、改了哪些参数。
///
/// **敏感参数不落盘。** 它们的值来自 `--set`,写进一个可 git 的文件就等于
/// 把口令提交进版本库 —— 与 D-121 把凭据挪出 inventory 是同一条纪律。
fn write_app(
    name: &str,
    bp_file: &Path,
    target: &crate::target::TargetOpts,
    bp: &Blueprint,
    given: &std::collections::BTreeMap<String, serde_yaml::Value>,
) -> Result<()> {
    let out = PathBuf::from(format!("{name}.app.yaml"));
    if out.exists() {
        say!("{} 已存在 —— 保留不动(改它就是改这次安装)", out.display());
        return Ok(());
    }
    let is_secret = |k: &str| bp.params.get(k).map(|p| p.secret).unwrap_or(false) || looks_secret(k);
    let secret: Vec<&str> = given.keys().filter(|k| is_secret(k)).map(|s| s.as_str()).collect();
    let mut y = String::new();
    y.push_str(&format!("# {name} —— 把这份蓝图钉在这群机器上。\n"));
    y.push_str("# 这个文件就是\"任务\"本身:可 git、可 diff、可进闭包;改它 = 改任务。\n");
    y.push_str("app:\n");
    y.push_str(&format!("  name: {name}\n"));
    y.push_str(&format!("  blueprint: {}\n", bp_file.display()));
    if let Some(i) = &target.inventory {
        y.push_str(&format!("  inventory: {}\n", i.display()));
    }
    if let Some(l) = &target.limit {
        let items: Vec<String> = l.split(',').map(|s| format!("{}", s.trim())).collect();
        y.push_str(&format!("  limit: [{}]\n", items.join(", ")));
    }
    let plain: Vec<(&String, &serde_yaml::Value)> =
        given.iter().filter(|(k, _)| !is_secret(k)).collect();
    if !plain.is_empty() {
        y.push_str("  params:\n");
        for (k, v) in plain {
            let s = serde_yaml::to_string(v).unwrap_or_default().trim().to_string();
            y.push_str(&format!("    {k}: {s}\n"));
        }
    }
    if !secret.is_empty() {
        y.push_str("  # 敏感参数不写进这个文件(它是要进 git 的):\n");
        for k in &secret {
            y.push_str(&format!("  #   {k} —— 每次用 `--set {k}=…`,或放进机群 vars\n"));
        }
    }
    std::fs::write(&out, y)?;
    say!("任务 → {}", out.display());
    if !secret.is_empty() {
        crate::oops!("敏感参数 {} 未写入 app 文件 —— 每次执行都要重新给。", secret.join(", "));
    }
    Ok(())
}
