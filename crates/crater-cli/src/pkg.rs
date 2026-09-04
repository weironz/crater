//! `crater build/push/pull` —— 把一份蓝图打成 OCI 制品,推上去、拉下来、看契约(D-123)。
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
//!   什么"只需 manifest + config 几百字节,一层都不用下载 —— `inspect`
//!   与 UI 的远端目录都走这条。
//! - **凭据永远不进包。** inventory 是操作者侧的数据,而包是要推到 registry
//!   上给别人拉的。打包时全数排除并逐个报出来,余下的文件再扫一遍字面口令,
//!   撞上就**拒绝打包** —— 推上去之后再删是删不掉的。

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context as _, Result};
use crater_core::store::{
    ImageStore, ANN_MATERIAL_FETCH, ANN_MATERIAL_NAME, ANN_MATERIAL_SOURCE, ANN_SEED_ARCH,
    ANN_SEED_OS, ANN_SEED_VERSION, MT_MATERIAL, MT_PKG_CONFIG, MT_PKG_LAYER, MT_SITE_SEED,
};
use crater_ir::ir::Blueprint;
use serde_json::json;

use crate::say;

const MT_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
// 预留给包签名(D-137):`org.crater.signature.*` 注解前缀与
// `application/vnd.crater.signature.*` 两个 mediaType,现在起不作他用。
// 只是**不关门** —— 实现时零成本,而占用了再腾出来要动已发布的包。
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

/// 要进包的一个文件:(包内相对路径, 内容, unix mode)。
type PackedFile = (String, Vec<u8>, u32);
/// 被挡在包外的一项:(包内相对路径, 被排除的原因)。
type SkippedFile = (String, &'static str);

/// 走一遍目录,收出要进包的文件与被排除的清单。
fn collect(root: &Path) -> Result<(Vec<PackedFile>, Vec<SkippedFile>)> {
    use std::os::unix::fs::PermissionsExt;
    let mut files = Vec::new();
    let mut skipped = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .flatten()
            .map(|e| e.path())
            .collect();
        entries.sort(); // 可复现:目录序不该影响包的 digest
        for p in entries {
            let base = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
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
        let Ok(text) = std::str::from_utf8(data) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let t = line.trim_start();
            for key in ["password:", "passwd:", "secret_key:", "token:"] {
                let Some(v) = t.strip_prefix(key) else {
                    continue;
                };
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
/// `images` 看见的就是对方会拉到的。
///
/// `archs` 非空即带闭包:每个架构烤一遍物料,做成各自的层。**蓝图层与
/// config 跨架构是同一个 digest** —— registry 按内容寻址,只存一份,
/// 多一个架构只多它自己的物料字节。给了两个及以上架构就产出 image index。
async fn assemble(
    path: &Path,
    reference: &str,
    archs: &[String],
    fors: &[String],
    sets: &[String],
    seed_inventories: &[PathBuf],
    seed_files: &[PathBuf],
) -> Result<String> {
    let (bp_file0, root0) = locate(path)?;
    let (mut files, skipped) = collect(&root0)?;
    if files.is_empty() {
        bail!("{} 里没有可打包的文件", root0.display());
    }

    // `--set` 把覆盖值**烤进包里的那份蓝图文本**,然后后续一切都以它为准 ——
    // 包的字节、契约、以及烤物料时渲染的 URL 必须来自同一份文本。任何一处
    // 用回原文,包就会"写着一套装的是另一套"(D-159 那类)。
    //
    // 摊到临时目录再从那里 load,而不是就地改源文件:源文件是用户的,打包
    // 不该动它;而只改内存里的字节又不够 —— `closure::bake_bytes` 要按路径
    // 读蓝图,而蓝图可能被 `fmt --split` 拆成了同目录的多个文件。
    let _staging;
    let (bp_file, root) = if sets.is_empty() {
        (bp_file0, root0)
    } else {
        let bp0 = crate::blueprint::load(&bp_file0)?;
        let declared: Vec<String> = bp0.params.keys().cloned().collect();
        let kv: std::collections::BTreeMap<String, String> = sets
            .iter()
            .map(|s| {
                s.split_once('=')
                    .map(|(k, v)| (k.trim().to_string(), v.to_string()))
                    .ok_or_else(|| anyhow::anyhow!("`--set {s}` 应是 KEY=VALUE"))
            })
            .collect::<Result<_>>()?;

        let rel = bp_file0
            .strip_prefix(&root0)
            .unwrap_or(&bp_file0)
            .display()
            .to_string();
        let f = files
            .iter_mut()
            .find(|f| f.0 == rel)
            .ok_or_else(|| anyhow::anyhow!("{rel} 不在打包清单里"))?;
        let text =
            String::from_utf8(f.1.clone()).map_err(|_| anyhow::anyhow!("{rel} 不是 UTF-8 文本"))?;
        f.1 = crate::pkg_params::bake_defaults(&text, &kv, &declared)
            .map_err(|e| anyhow::anyhow!("{e}"))?
            .into_bytes();
        for (k, v) in &kv {
            say!("  · --set {k}={v} 已烤进包里的蓝图");
        }

        let dir = tempfile::tempdir()?;
        for (rel, data, mode) in &files {
            let p = dir.path().join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, data)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(*mode))?;
            }
        }
        let r = dir.path().to_path_buf();
        _staging = dir;
        (r.join(&rel), r)
    };

    let bp = crate::blueprint::load(&bp_file)?;
    refuse_literal_secrets(&files)?;
    let _ = &root;

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

    // Site Seeds are profile-addressed layers on the one public package tag.
    // `pull --offline -i inventory.yaml` reads their annotations and downloads
    // just its one closure blob; the remaining Seed layers stay remote.
    if !seed_inventories.is_empty() {
        if !archs.is_empty() || !fors.is_empty() {
            bail!("`--seed-inventory` 与 `--arch`/`--for` 不能混用；Seed 的画像只来自 inventory.platform");
        }
        let seed_dir = tempfile::tempdir()?;
        if !seed_files.is_empty() && seed_files.len() != seed_inventories.len() {
            bail!("`--seed-file` 给了 {} 个，但 `--seed-inventory` 给了 {} 个；两者必须按顺序一一对应", seed_files.len(), seed_inventories.len());
        }
        let mut profiles = BTreeSet::new();
        let mut layers = vec![bp_layer.clone()];
        let mut total = 0u64;
        for (i, inventory) in seed_inventories.iter().enumerate() {
            let output = seed_dir.path().join(format!("seed-{i}.tar"));
            let (plan, bytes) = if let Some(seed_file) = seed_files.get(i) {
                let plan = crate::prepare::plan_for_inventory(inventory)?;
                let bytes = std::fs::read(seed_file)
                    .with_context(|| format!("读取预先验证的 Site Seed {}", seed_file.display()))?;
                (plan, bytes)
            } else {
                let plan =
                    crate::prepare::bake_for_inventory(&bp_file, &output, inventory, sets).await?;
                let bytes = std::fs::read(&output)
                    .with_context(|| format!("读取刚生成的 Site Seed {}", output.display()))?;
                (plan, bytes)
            };
            let key = format!(
                "{}/{}/{}",
                plan.platform.arch, plan.platform.os, plan.platform.version
            );
            if !profiles.insert(key.clone()) {
                bail!("重复的 Site Seed 画像 `{key}`；一个公开包内每个画像只能有一份");
            }
            let (d, size) = store.put_blob(&bytes)?;
            total += size;
            say!("  ✓ Site Seed {key:<28} {:>9}", human(size));
            layers.push(json!({
                "mediaType": MT_SITE_SEED,
                "digest": format!("sha256:{d}"),
                "size": size,
                "annotations": {
                    ANN_SEED_ARCH: plan.platform.arch,
                    ANN_SEED_OS: plan.platform.os,
                    ANN_SEED_VERSION: plan.platform.version,
                    "org.opencontainers.image.title": format!("site-seed-{key}.tar"),
                }
            }));
        }
        let manifest = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST,
            "config": cfg_desc, "layers": layers, "annotations": ann
        });
        let digest = store.put_manifest(reference, &serde_json::to_vec(&manifest)?)?;
        say!(
            "离线 Seed {} —— {} 个画像，一个公开 tag",
            human(total),
            profiles.len()
        );
        return Ok(digest);
    }

    // 不带闭包:一份 manifest,与 Helm 的布局同形。
    if archs.is_empty() {
        let m = json!({
            "schemaVersion": 2, "mediaType": MT_MANIFEST,
            "config": cfg_desc, "layers": [bp_layer], "annotations": ann
        });
        return store.put_manifest(reference, &serde_json::to_vec(&m)?);
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
        // 传 None:包的物料层是"一份字节一层",镜像那棵树还没有对应的层形态
        // (issue #1 只解决闭包这一侧)。收不了就如实跳过并报出来。
        let (baked, _imgs, skip) =
            crate::closure::bake_bytes(&bp_file, &profile, &Default::default(), &mut seen, None)
                .await?;
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
        let d = store.put_manifest(reference, &serde_json::to_vec(&m)?)?;
        say!();
        say!("闭包 {} —— 一份 manifest", human(total_mat));
        return Ok(d);
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
    let d = store.put_manifest(reference, &serde_json::to_vec(&index)?)?;
    say!();
    say!(
        "闭包 {} —— {} 个架构,index 一个 tag 装下",
        human(total_mat),
        per_arch.len()
    );
    Ok(d)
}

// ───────────────────────────── 命令 ─────────────────────────────

/// `crater build <路径> -t <ref>` —— 只组装,不推。
pub async fn build(
    path: &Path,
    reference: &str,
    archs: &[String],
    fors: &[String],
    sets: &[String],
    seed_inventories: &[PathBuf],
    seed_files: &[PathBuf],
) -> Result<()> {
    let digest = assemble(
        path,
        reference,
        archs,
        fors,
        sets,
        seed_inventories,
        seed_files,
    )
    .await?;
    say!("已入本地 store(sha256:{digest})—— `crater push {reference}` 推上去");
    Ok(())
}

/// `crater push <路径> <ref>` —— 组装并推。
pub async fn push(
    path: &Path,
    reference: &str,
    archs: &[String],
    fors: &[String],
    sets: &[String],
    seed_inventories: &[PathBuf],
    seed_files: &[PathBuf],
) -> Result<()> {
    let digest = assemble(
        path,
        reference,
        archs,
        fors,
        sets,
        seed_inventories,
        seed_files,
    )
    .await?;
    let store = ImageStore::open()?;
    store.push(reference).await?;
    say!("推送完成 → {reference}");
    // 报出 manifest digest(D-137):tag 可变、digest 不可变,"按 digest 钉住"
    // 与将来的验签都以它为坐标。不印出来的话,人得再查一次 registry 才拿得到。
    say!("  digest  sha256:{digest}");
    Ok(())
}

// 「一个已拉全的包里的物料字节 → 部署侧的 `BlobMap`」搬去了
// `blob_source::oci::OciSource` —— 它是 D-119 说的第二个 blob 后端,现在与 tar
// 闭包同在一个 `BlobSource` 后面,而四个方法仍然是四个方法。

/// `crater pull <ref> [--into DIR] [--full]` —— 拉下来并摊回文件。
///
/// 默认**瘦拉**:manifest + config + 蓝图层。物料层(第二阶段)留在 registry,
/// 部署时目标机自己按 URL 取 —— 在线部署根本用不到那几百兆。
pub async fn pull(
    reference: &str,
    into: Option<&Path>,
    full: bool,
    offline_platform: Option<&crater_core::spec::Platform>,
    offline: bool,
) -> Result<()> {
    let store = ImageStore::open()?;
    if let Some(platform) = offline_platform {
        match store.pull_site_seed(reference, platform).await? {
            Some(_) => say!(
                "已预取 {}/{}/{} 的完整 Site Seed",
                platform.arch,
                platform.os,
                platform.version
            ),
            None => say!("该包未声明 Site Seed，已按兼容模式拉取完整包"),
        }
    } else if offline {
        // Keep `crater pull yq:… --offline` ergonomic for ordinary packages.
        // A Seed matrix, however, cannot be selected honestly without the
        // static inventory key — do not silently fetch every distro/profile.
        store.pull_thin(reference).await?;
        if store.has_site_seeds(reference)? {
            bail!(
                "{reference} 有多份 Site Seed；`--offline` 需要 `-i inventory.yaml` 选择 os/version/arch，\n\
                 例如:`crater pull {reference} --offline -i inventory.yaml`"
            );
        }
        store.pull(reference).await?;
        say!("该包未声明 Site Seed，已按兼容模式拉取完整包");
    } else if full {
        store.pull(reference).await?;
    } else {
        store.pull_thin(reference).await?;
    }
    let m = store.resolve_manifest(reference)?;
    let cfg = read_config(&store, &m)?;
    warn_if_newer(&cfg);
    let name = cfg["name"].as_str().unwrap_or("pkg").to_string();
    let dir = into
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| PathBuf::from(&name));
    if dir.exists() && std::fs::read_dir(&dir)?.next().is_some() {
        bail!("{} 已存在且非空 —— 换个 --into,或先移开", dir.display());
    }
    let layer = m["layers"]
        .as_array()
        .and_then(|ls| {
            ls.iter()
                .find(|l| l["mediaType"].as_str() == Some(MT_PKG_LAYER))
        })
        .ok_or_else(|| anyhow::anyhow!("{reference} 不是 crater 蓝图包(没有蓝图层)"))?;
    let d = layer["digest"].as_str().unwrap_or_default();
    let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:")))
        .with_context(|| format!("{reference} 的蓝图层不在本地"))?;
    crater_core::bundle::untar_gz_into(&dir, &bytes, 0)?;
    stamp(&dir, reference)?;
    let n = std::fs::read_dir(&dir)
        .map(|r| r.flatten().count())
        .unwrap_or(0);
    say!("{reference} → {}({n} 项)", dir.display());
    let mats = m["layers"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .filter(|l| l["mediaType"].as_str() == Some(MT_MATERIAL))
                .count()
        })
        .unwrap_or(0);
    if full && mats > 0 {
        say!("闭包 {mats} 份物料随包带下 —— 断网部署:");
        say!(
            "  crater apply -f {}/... -i <机群> --closure oci://{reference}",
            dir.display()
        );
    }
    print_contract(&cfg);
    Ok(())
}

/// `crater inspect <ref>` —— 只拉 manifest + config,一层都不下载。
pub async fn inspect(reference: &str) -> Result<()> {
    // 本地有就读本地,省一次往返;没有才问 registry。
    let store = ImageStore::open()?;
    let cfg = match store
        .resolve_manifest(reference)
        .ok()
        .and_then(|m| read_config(&store, &m).ok())
    {
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

/// `crater tags <ref>` —— 远端有哪些版本。
pub async fn tags(reference: &str) -> Result<()> {
    let mut tags = ImageStore::list_tags(reference).await?;
    if tags.is_empty() {
        say!("{reference}:一个 tag 都没有");
        return Ok(());
    }
    // semver 降序:人问"有哪些版本"时想先看见最新的那个。
    tags.sort_by_key(|t| std::cmp::Reverse(semver_key(t)));
    for t in &tags {
        // OCI tag 里不能有 `+`,推的时候换成了 `_`,读回来换回去。
        say!("  {}", t.replace('_', "+"));
    }
    say!();
    say!(
        "{} 个版本。`crater inspect <ref>:<版本>` 看契约。",
        tags.len()
    );
    Ok(())
}

/// `crater images` —— 本地 store 里的蓝图包。
/// 这个引用在 `crater images` 里该附一句什么说明。
///
/// 是 crater 包就给"描述(物料几份)";普通制品(闭包用的容器镜像等)返回
/// 空串。此前这是一条独立命令 `pkg ls`,而"同一个 store 要看两遍"本身就是
/// 分裂的信号 —— 现在 `images` 列全部,能多说的就多说一句。
pub fn describe(store: &ImageStore, reference: &str) -> String {
    let Ok(top) = store.resolve_manifest(reference) else {
        return String::new();
    };
    // 多架构包的契约在子清单里 —— 不折这一下就看不见它们。
    let m = platform_manifest(store, &top).unwrap_or(top);
    if m["config"]["mediaType"].as_str() != Some(MT_PKG_CONFIG) {
        return String::new();
    }
    let cfg = read_config(store, &m).unwrap_or(json!({}));
    let desc = cfg["description"].as_str().unwrap_or("").to_string();
    let mats = m["layers"]
        .as_array()
        .map(|ls| {
            ls.iter()
                .filter(|l| l["mediaType"].as_str() == Some(MT_MATERIAL))
                .count()
        })
        .unwrap_or(0);
    match (desc.is_empty(), mats) {
        (true, 0) => "(crater 包)".into(),
        (true, n) => format!("(crater 包,闭包 {n} 份)"),
        (false, 0) => desc,
        (false, n) => format!("{desc}(闭包 {n} 份)"),
    }
}

// ─────────────────────────── 离线搬运(U 盘) ───────────────────────────

/// `crater save <ref> -o <包>.pkg.tar`
///
/// 与裸 `crater save` 的区别全在**说清楚搬走的是什么**:几个架构、几份物料、
/// 多大。断网机房里包不对的代价是白跑一趟,而 tar 里缺没缺东西在甲机上是
/// 看得出来的 —— 只要有人报。
pub fn save(reference: &str, out: &Path) -> Result<()> {
    let store = ImageStore::open()?;
    let top = store
        .resolve_manifest(reference)
        .with_context(|| format!("{reference} 不在本地 store(`crater images` 看有哪些)"))?;

    // **瘦包不该被当成离线包搬走。** 蓝图层齐了就能在线部署,于是 save 会
    // 成功、tar 会生成、`load` 会成功 —— 一直到断网机上 apply 去取物料时才
    // 报错,而那时 U 盘已经在另一栋楼里了。
    //
    // 判据是"字节在不在盘上",不是层上的 `fetch=dependency` 标注 —— 后者说的
    // 是这份物料**从哪来**(一个远端 URL),不是它有没有被烤进包。两者只在
    // 瘦拉时才分开,拿它当判据会把每一个正常的离线包都拦下来。
    let per_arch = arch_layers(&store, &top);
    if !store.has_all_layers(reference) {
        bail!(
            "{reference} 的闭包不完整 —— 物料层还在 registry,tar 里不会有字节。\n\
             搬到断网机上装不了。先补齐:`crater pull {reference} --full`,再 save。"
        );
    }

    store.export_oci_archive(reference, out)?;
    let bytes = std::fs::metadata(out).map(|m| m.len()).unwrap_or(0);
    let plats = crater_core::store::platforms_of(&top);
    say!("{reference} → {}", out.display());
    say!(
        "  {}  {}",
        human(bytes),
        if plats.is_empty() {
            "单架构".to_string()
        } else {
            format!("{} 个架构:{}", plats.len(), plats.join(" "))
        }
    );
    for (a, mats, here) in &per_arch {
        say!("  {a:<10} 物料 {here}/{mats} 份在本地");
    }
    say!();
    say!("索引也放同一个目录,对面就能搜:");
    say!(
        "  crater index --store -o {}/index.yaml",
        out.parent()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".into())
    );
    Ok(())
}

/// `crater load <包>.pkg.tar`
///
/// 收进来之后**当场复核闭包**。`import` 只保证"归档里有的都进来了";
/// 这里回答的是断网机上唯一要紧的那个问题 —— 装得了装不了。
pub fn load(file: &Path, as_ref: Option<&str>) -> Result<()> {
    let store = ImageStore::open()?;
    let (reference, root) = store.import_oci_archive_rooted(file, as_ref)?;
    // 报的是**归档的根**,不是 tag 落到的那份子清单 —— 多架构包收进来之后
    // 引用指向本机架构那一份(与在线 pull 一致),而"tar 里有几个架构"是
    // 另一个问题,也是搬错了才看得出来的那个。
    let top: serde_json::Value =
        serde_json::from_slice(&std::fs::read(store.blob_path(&root))?).unwrap_or(json!({}));
    let plats = crater_core::store::platforms_of(&top);
    say!("{} → {reference}", file.display());
    if !plats.is_empty() {
        say!("  {} 个架构:{}", plats.len(), plats.join(" "));
    }
    for (a, mats, here) in arch_layers(&store, &top) {
        say!("  {a:<10} 物料 {here}/{mats} 份在本地");
    }
    if !store.has_all_layers(&reference) {
        bail!(
            "{reference} 收进来了,但闭包不完整 —— 装不了。\n\
             回联网机上 `crater pull {reference} --full` 再 `crater save`。"
        );
    }
    say!();
    // **`--full` 不能省。** 没有它 install 走瘦拉,物料仍然按 URL 去下载 ——
    // 而这条提示恰恰是给断网现场看的:在那里它必然失败,报"下载失败 exit 127"。
    // 提示里漏一个开关,等于把人送进一个与提示内容相反的结论。实测踩过。
    say!("闭包完整,不用连网:");
    say!("  crater install {reference} --full -i <机群>");
    say!("  (`--full` 是关键 —— 少了它会去下载物料,断网现场必失败)");
    Ok(())
}

/// 每个架构的 (架构名, 声明的物料层数, 字节确实在本地的层数)。
///
/// 两个数分开报,是因为它们不等价正是离线搬运唯一会翻车的地方:清单上写着
/// 一份物料,盘上却没有那个 blob —— 瘦拉的包就长这样。
///
/// 单架构包返回一条,名字用 `-`。
fn arch_layers(store: &ImageStore, top: &serde_json::Value) -> Vec<(String, usize, usize)> {
    let count = |m: &serde_json::Value| -> (usize, usize) {
        let ls = m["layers"].as_array().map(|v| v.as_slice()).unwrap_or(&[]);
        let mats: Vec<_> = ls
            .iter()
            .filter(|l| l["mediaType"].as_str() == Some(MT_MATERIAL))
            .collect();
        let here = mats
            .iter()
            .filter(|l| {
                l["digest"]
                    .as_str()
                    .map(|d| store.blob_path(d.trim_start_matches("sha256:")).exists())
                    .unwrap_or(false)
            })
            .count();
        (mats.len(), here)
    };
    match top["manifests"].as_array() {
        Some(subs) => subs
            .iter()
            .filter_map(|s| {
                let a = s["platform"]["architecture"].as_str()?.to_string();
                let d = s["digest"].as_str()?;
                let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:"))).ok()?;
                let m: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                let (b, dp) = count(&m);
                Some((a, b, dp))
            })
            .collect(),
        None => {
            let (b, d) = count(top);
            vec![("-".to_string(), b, d)]
        }
    }
}

// ───────────────────────────── 小工具 ─────────────────────────────

/// 一条本地清单折到**带 config 的那一份**上。
///
/// 单架构包原样返回;多架构包的顶层是 image index —— 它没有 config,契约在
/// 子清单里。不折这一下,`images` 与 `pkg index --store` 会把每一个多架构包
/// 当成"不是 crater 包"跳过:`images` 说"本地还没有蓝图包",`pkg index
/// --store` 把它**静默**漏在索引外(store 里只有它时才会撞上"一个包都没收
/// 进来"那道闸,否则连个响都没有)。issue #3。
pub(crate) fn platform_manifest(
    store: &ImageStore,
    m: &serde_json::Value,
) -> Option<serde_json::Value> {
    let subs = m["manifests"].as_array()?;
    // 架构优先级与 store 拉取一致(D-127):本机 → amd64 → 任意一条。
    let want = crater_core::arch::detect_local();
    let want = want.as_str();
    let pick = |a: &str| {
        subs.iter()
            .find(|e| e["platform"]["architecture"].as_str() == Some(a))
    };
    let sub = pick(want)
        .or_else(|| pick("amd64"))
        .or_else(|| subs.first())?;
    let d = sub["digest"].as_str()?;
    let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:"))).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// 读 config;不是 crater 包(或读不动)就是 None —— 调用方多半在遍历
/// 一整个 store,一条读不动不该让整趟失败。
pub(crate) fn config_of(store: &ImageStore, m: &serde_json::Value) -> Option<serde_json::Value> {
    let m = match platform_manifest(store, m) {
        Some(sub) => sub,
        None => m.clone(),
    };
    if m["config"]["mediaType"].as_str() != Some(MT_PKG_CONFIG) {
        return None;
    }
    read_config(store, &m).ok()
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
    std::fs::read_to_string(dir.join(STAMP))
        .ok()
        .map(|s| s.trim().to_string())
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
/// `1.2.0-rc1 < 1.2.0` —— 少了这一条,`tags` 会把 rc 当成最新版推荐。
pub(crate) fn semver_key(v: &str) -> Vec<i64> {
    let v = v.trim_start_matches('v');
    let (core, pre) = match v.find(['-', '+', '_']) {
        Some(i) => (&v[..i], &v[i + 1..]),
        None => (v, ""),
    };
    let mut key: Vec<i64> = core
        .split('.')
        .map(|p| p.parse::<i64>().unwrap_or(-1))
        .collect();
    key.resize(4, 0);
    if pre.is_empty() {
        key.push(i64::MAX); // 正式版胜过它的任何预发布
    } else {
        key.push(0);
        key.extend(
            pre.split(['.', '-', '_', '+'])
                .map(|p| p.parse::<i64>().unwrap_or(-1)),
        );
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
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{f:.1} {}", U[i])
    }
}

/// 把契约印成人能读的样子 —— 与 `crater inspect` 同一个问题的同一个答案。
fn print_contract(cfg: &serde_json::Value) {
    let ver = cfg["version"]
        .as_str()
        .map(|v| format!("  v{v}"))
        .unwrap_or_default();
    say!();
    say!("蓝图 {}{ver}", cfg["name"].as_str().unwrap_or("?"));
    if let Some(d) = cfg["description"].as_str().filter(|s| !s.is_empty()) {
        say!("{d}");
    }
    if let Some(ps) = cfg["params"].as_array().filter(|a| !a.is_empty()) {
        say!();
        say!("参数:");
        let w = ps
            .iter()
            .map(|p| p["name"].as_str().unwrap_or("").len())
            .max()
            .unwrap_or(0);
        // 必填排前面 —— 读的人最先要知道"我非填不可的是什么"。
        let mut sorted: Vec<&serde_json::Value> = ps.iter().collect();
        sorted.sort_by_key(|p| {
            (
                !p["default"].is_null(),
                p["name"].as_str().unwrap_or("").to_string(),
            )
        });
        for p in sorted {
            let need = if p["default"].is_null() {
                "必填".to_string()
            } else {
                format!("默认 {}", compact(&p["default"]))
            };
            let stage = if p["stage"].as_str() == Some("build") {
                " [构建期]"
            } else {
                ""
            };
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
            let n = if min == 0 {
                "可为空".to_string()
            } else {
                format!("至少 {min} 台")
            };
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
    /// 切 tag 只能看**最后一段**里的冒号。
    ///
    /// `localhost:5000/ns/yq:4.*` 里第一个冒号是端口 —— 按第一个冒号切会得到
    /// 仓库 `localhost`、tag `5000/ns/yq:4.*`,一个根本不存在的东西,而报错会是
    /// "localhost 上没有匹配的版本",把人引向 registry 而不是引向这行代码。
    #[test]
    fn splitting_a_tag_must_not_trip_on_the_port_colon() {
        assert_eq!(
            super::split_tag("localhost:5000/ns/yq:4.*"),
            Some(("localhost:5000/ns/yq", "4.*"))
        );
        assert_eq!(
            super::split_tag("ghcr.io/acme/yq:0.0.*"),
            Some(("ghcr.io/acme/yq", "0.0.*"))
        );
        // 带端口但**没写 tag** —— 不能把端口当成 tag
        assert_eq!(super::split_tag("localhost:5000/ns/yq"), None);
        assert_eq!(super::split_tag("ghcr.io/acme/yq"), None);
        // 没有仓库路径的裸名字
        assert_eq!(super::split_tag("yq:4.44.3"), Some(("yq", "4.44.3")));
    }

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
            (
                "t/a.j2".into(),
                b"password: \"${env:PW}\"\n".to_vec(),
                0o644,
            ),
            (
                "t/b.j2".into(),
                b"password: {{ params.pw }}\n".to_vec(),
                0o644,
            ),
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
        v.sort_by_key(|e| std::cmp::Reverse(semver_key(e)));
        assert_eq!(v, vec!["1.10.0", "1.2.0", "1.2", "1.2.0-rc1", "0.9.9"]);
    }

    // ─── 升级路径(D-141) ───

    #[test]
    fn a_registry_port_is_not_a_version() {
        // `zot:5031/lib/yq` 的 `5031` 是端口。认错了的话,两个版本会摊进
        // 同一个 `yq-5031/`,D-128 那次事故原样复活。
        assert_eq!(tag_of("yq:4.40.5"), "4.40.5");
        assert_eq!(tag_of("zot:5031/lib/yq:4.40.5"), "4.40.5");
        assert_eq!(tag_of("zot:5031/lib/yq"), "latest");
        assert_eq!(tag_of("yq"), "latest");
        assert_eq!(
            tag_of("zot:5031/lib/yq@sha256:0123456789abcdef0123"),
            "sha256-0123456789ab"
        );
    }

    #[test]
    fn two_versions_never_share_a_directory() {
        // 这条就是本 issue 的全部理由:目录名带上版本,"装错版本"从
        // "被拦下来"变成"发生不了"。
        assert_eq!(pkg_dir_name("yq", "zot:5031/lib/yq:4.44.3"), "yq-4.44.3");
        assert_eq!(pkg_dir_name("yq", "zot:5031/lib/yq:4.40.5"), "yq-4.40.5");
        assert_ne!(
            pkg_dir_name("yq", "yq:4.44.3"),
            pkg_dir_name("yq", "yq:4.40.5")
        );
        // 手敲出来的怪引用不能把包摊到别的目录去:结果永远是**一段**路径。
        for r in ["yq:../../etc", "yq:4.0 rc", "yq:/etc/passwd", "yq:."] {
            let d = pkg_dir_name("yq", r);
            assert_eq!(
                Path::new(&d).components().count(),
                1,
                "{r} 摊成了 {d} —— 不是单段路径"
            );
            assert!(!d.contains('/'), "{r} 摊成了 {d}");
        }
        assert_eq!(pkg_dir_name("yq", "yq:4.0 rc"), "yq-4.0-rc");
    }

    #[test]
    fn repointing_an_app_file_touches_exactly_one_line() {
        // 反证:params / 注释 / 键序被顺手重排,等于升级悄悄改了人的文件。
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("yq.app.yaml");
        let before = "# yq —— 把这份蓝图钉在这群机器上。\n\
                      app:\n  \
                      name: yq\n  \
                      blueprint: yq-4.44.3/yq.blueprint.yaml\n  \
                      inventory: inv.yaml\n  \
                      params:\n    \
                      version: 4.44.3\n";
        std::fs::write(&f, before).unwrap();
        assert!(repoint_app(&f, Path::new("yq-4.40.5/yq.blueprint.yaml")).unwrap());
        let after = std::fs::read_to_string(&f).unwrap();
        assert!(after.contains("  blueprint: yq-4.40.5/yq.blueprint.yaml\n"));
        assert!(!after.contains("4.44.3/yq.blueprint.yaml"));
        // 除了那一行,逐行相同。
        let a: Vec<&str> = before.lines().collect();
        let b: Vec<&str> = after.lines().collect();
        assert_eq!(a.len(), b.len());
        assert_eq!(
            a.iter().zip(&b).filter(|(x, y)| x != y).count(),
            1,
            "只该动 blueprint 那一行"
        );
    }

    #[test]
    fn an_app_file_without_a_blueprint_line_is_left_alone() {
        // 没有那一行就报出来让人自己指 —— 不要凭猜往里塞一行。
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("x.app.yaml");
        std::fs::write(&f, "app:\n  name: x\n").unwrap();
        assert!(!repoint_app(&f, Path::new("x-2/x.blueprint.yaml")).unwrap());
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "app:\n  name: x\n");
    }

    #[test]
    fn a_package_dir_with_no_package_in_the_store_admits_it_cannot_tell() {
        // 与 D-135 同一条纪律:比不了要说"判不出",不能报"没改动" ——
        // 后者会让升级闸门在最该拦的时候放行。
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.blueprint.yaml"), "name: a\n").unwrap();
        match compare_to_package(dir.path(), "no-such-registry.invalid/nope:1") {
            Drift::Unknown(_) => {}
            other => panic!("该是判不出,却得到 {other:?}"),
        }
    }
}

// ───────────────────────────── install ─────────────────────────────

// ─── 升级路径:目录布局与本地改动闸门(D-141) ───

/// 引用里的版本段 —— 摊包目录名的后半截。
///
/// 取的是**引用的 tag**,不是契约里的 `version:`。后者是蓝图自己的版本,
/// 与被装的东西的版本不是一回事:`library/yq` 的 `version: "1"` 在
/// `yq:4.44.3` 与 `yq:4.40.5` 两个包里是同一个值 —— 拿它做目录名等于
/// 把 D-128 那次事故原样复制一遍。tag 才是使用者敲进去、也真正区分这
/// 两次安装的那一段。
fn tag_of(reference: &str) -> String {
    // digest 引用没有 tag,拿短 digest 顶上 —— 它同样是"不同就是不同"。
    if let Some((_, d)) = reference.split_once('@') {
        let hex = d.rsplit(':').next().unwrap_or(d);
        return format!("sha256-{}", &hex[..hex.len().min(12)]);
    }
    // `zot:5031/lib/yq` 里的 `:5031` 是端口不是 tag —— 只认最后一段路径里的冒号。
    let last = reference.rfind('/').map(|i| i + 1).unwrap_or(0);
    match reference[last..].rsplit_once(':') {
        Some((_, tag)) if !tag.is_empty() => tag.to_string(),
        _ => "latest".to_string(),
    }
}

/// 摊包目录名:`<包名>-<版本>`。
///
/// OCI tag 的字符集(`[A-Za-z0-9_.-]`)本就是文件名安全的;这里仍然过一道,
/// 因为引用可以是人手敲的,而一个带 `/` 的"tag"会把包摊到别处去。
fn pkg_dir_name(name: &str, reference: &str) -> String {
    let tag: String = tag_of(reference)
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '+') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{name}-{tag}")
}

/// 一次目录对账的结论。**三态**,与 D-135 同一套诚实:判不出要说判不出,
/// 不能拿"没比出差别"冒充"没有差别"。
#[derive(Debug, PartialEq)]
enum Drift {
    Same,
    Changed(Vec<String>),
    Unknown(String),
}

/// `reference` 的蓝图层字节(本地 store 里那份)。
fn pkg_layer_bytes(store: &ImageStore, reference: &str) -> Option<Vec<u8>> {
    let m = store.resolve_manifest(reference).ok()?;
    let layer = m["layers"]
        .as_array()?
        .iter()
        .find(|l| l["mediaType"].as_str() == Some(MT_PKG_LAYER))?;
    let d = layer["digest"].as_str()?;
    std::fs::read(store.blob_path(d.trim_start_matches("sha256:"))).ok()
}

/// 摊开的包目录 vs 它当初那个包 —— 有没有人动过。
///
/// 比的口径就是 [`collect`] 那一套(打包时用的同一份):凭据、`*.app.yaml`、
/// 闭包、点开头的文件本来就不在包里,把它们当"改动"报出来只会天天误报,
/// 报多了人就不看了。
///
/// 只比字节不比 mode:两边都是同一条 `untar_gz_into` 摊出来的,mode 差异
/// 在实践中来自 umask 而不是人的意图,而一条会误报的警告等于没有警告。
fn compare_to_package(dir: &Path, reference: &str) -> Drift {
    let store = match ImageStore::open() {
        Ok(s) => s,
        Err(e) => return Drift::Unknown(format!("打不开本地 store:{e}")),
    };
    let Some(bytes) = pkg_layer_bytes(&store, reference) else {
        return Drift::Unknown(format!("`{reference}` 的蓝图层不在本地 store"));
    };
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => return Drift::Unknown(format!("建不出临时目录:{e}")),
    };
    if let Err(e) = crater_core::bundle::untar_gz_into(tmp.path(), &bytes, 0) {
        return Drift::Unknown(format!("`{reference}` 的蓝图层摊不开:{e}"));
    }
    let (want, _) = match collect(tmp.path()) {
        Ok(x) => x,
        Err(e) => return Drift::Unknown(format!("读原样字节:{e}")),
    };
    let (have, _) = match collect(dir) {
        Ok(x) => x,
        Err(e) => return Drift::Unknown(format!("读 {}:{e}", dir.display())),
    };
    let want: std::collections::BTreeMap<&str, &[u8]> = want
        .iter()
        .map(|(p, d, _)| (p.as_str(), d.as_slice()))
        .collect();
    let have: std::collections::BTreeMap<&str, &[u8]> = have
        .iter()
        .map(|(p, d, _)| (p.as_str(), d.as_slice()))
        .collect();
    let mut out = Vec::new();
    for (p, d) in &want {
        match have.get(p) {
            None => out.push(format!("删 {p}")),
            Some(h) if h != d => out.push(format!("改 {p}")),
            Some(_) => {}
        }
    }
    for p in have.keys() {
        if !want.contains_key(p) {
            out.push(format!("增 {p}"));
        }
    }
    out.sort();
    if out.is_empty() {
        Drift::Same
    } else {
        Drift::Changed(out)
    }
}

/// `<名>.app.yaml` 现在指着哪一份蓝图(文件, 它所在的目录)。
///
/// app 文件是"这次安装的正身",所以"上一版装在哪"以它为准,而不是去
/// 目录树里猜 —— UI 读的也正是这一行(`ui_app::parse_app`)。
fn app_points_at(app_name: &str) -> Option<(PathBuf, PathBuf)> {
    let f = PathBuf::from(format!("{app_name}.app.yaml"));
    let text = std::fs::read_to_string(&f).ok()?;
    let def = crate::ui_app::parse_app(&f, &text).ok()?;
    let bp = PathBuf::from(&def.blueprint);
    let dir = bp
        .parent()
        .filter(|p| !p.as_os_str().is_empty())?
        .to_path_buf();
    Some((f, dir))
}

/// 换版本之前先问一句:上一版的目录里有没有人动过的东西。
///
/// 为什么这道闸门不能省:`<包名>-<版本>` 布局下新版摊进**新目录**,旧目录
/// 一个字节都不会被覆盖 —— 听起来很安全,但那正是问题:有人在旧目录里改过
/// 的模板、加过的文件,升级之后**一声不响地不在了**。目标机上跑的是新版本,
/// 而他以为自己的改动还在。这是本仓库最忌讳的那类静默失效。
///
/// `--yes` **不能**跨过这道闸门:`--yes` 的意思是"计划我看过了,执行吧",
/// 不是"我的改动随便丢"。要丢得单独说一次。
fn upgrade_gate(prev_dir: &Path, prev_ref: &str, source: &str, force: bool) -> Result<()> {
    say!();
    say!("换版本:{prev_ref} → {source}");
    match compare_to_package(prev_dir, prev_ref) {
        Drift::Same => {
            say!(
                "  {} 与包一致 —— 没有会被落下的本地改动",
                prev_dir.display()
            );
            Ok(())
        }
        Drift::Changed(items) => {
            crate::oops!(
                "{} 里有 {} 处本地改动,它们不会跟到新版本:",
                prev_dir.display(),
                items.len()
            );
            for i in &items {
                say!("    {i}");
            }
            if force {
                say!("  --force:照旧升级。旧目录原样留着,改动没丢,只是没跟过去。");
                return Ok(());
            }
            bail!(
                "先决定这些改动怎么办 —— 一台机器都没碰:\n  \
                 要带过去:装完后把它们套到新目录再 `crater apply`\n  \
                 不带过去:加 `--force` 重跑(旧目录不删,随时能回去看)\n  \
                 要看差别:diff {} 与 `crater pull {prev_ref} --into <临时目录>`",
                prev_dir.display()
            )
        }
        Drift::Unknown(why) => {
            crate::oops!("? 判不出 {} 有没有本地改动:{why}", prev_dir.display());
            if force {
                say!("  --force:照旧升级。");
                return Ok(());
            }
            bail!(
                "判不出就不猜 —— 一台机器都没碰。二选一:\n  \
                 把原样字节取回来再判:`crater pull {prev_ref} --into <临时目录>`\n  \
                 不在乎旧目录里有什么:加 `--force` 重跑"
            )
        }
    }
}

/// 换版本时,这次计划判不判得出目标机上那份是旧的?
///
/// D-135 之后,物料没声明 `sha256:` 又没有闭包时 crater 报 `?` 并**不动** ——
/// 于是"升级"会变成一次什么都没改的 apply,而日志从头到尾都是对的。
/// 这一句就是把那个结局**提前**说出来:计划印出来之前先讲清楚它会是 `?`,
/// 而不是让人对着一份"无变更"的计划自己去悟。
///
/// 只提醒不拦:拦住也变不出摘要,而 D-135 的 `?` 本身已经保证了不会误改。
fn warn_if_undecidable(bp: &Blueprint, source: &str, have_bytes: bool) {
    if have_bytes {
        return;
    }
    let blind: Vec<&str> = bp
        .materials
        .iter()
        .filter(|m| m.kind == crater_ir::ir::MaterialKind::File && m.sha256.is_none())
        .map(|m| m.name.as_str())
        .collect();
    if blind.is_empty() {
        return;
    }
    crate::oops!(
        "{} 个物料没声明 `sha256:`,这次又没有闭包 —— 目标机上那份是不是旧版本,crater 判不出(D-135):{}",
        blind.len(),
        blind.join(", ")
    );
    say!("  计划里它们会是 `?`,不是 `~ 将修改`,apply 也不会动它们。");
    say!("  要真换上去,二选一:");
    say!("    crater install {source} --full …                  # 字节拉到本地,能算摘要");
    say!("    crater install {source} --closure <closure.tar> … # 离线现场同理");
    say!("  或让包作者在蓝图物料上写 `sha256:`。");
}

/// `crater install <引用|目录> -i <机群> [--set k=v]… [--yes]`
///
/// 把散在五条命令里的动作串成一条:拉包 → 读契约 → 对账机群 → 落 app 文件
/// → plan。**闸门一步不省** —— `--yes` 才继续 apply,否则停在计划上。
/// "一键"省掉的是找包、抄参数、比对组名那几步,不是"先看 diff 再动手"。
///
/// 顺序是刻意的:**契约与机群都在本地对完账,才连第一台机器**。参数少给一个、
/// 组名写错一个,在 SSH 之前就该说清楚 —— 那时候纠正的代价是改一行命令,
/// 连上之后再发现就已经在改机器了。
/// `install` 的三个开关。
///
/// 抽出来不是为了让 clippy 闭嘴,是因为它们**确实构成一个整体**:三个都在
/// 回答"这次安装允许越过哪道闸",而且三道闸互不替代 ——
/// `yes` 是"计划我看过了",`force` 是"上一版目录里的本地改动我不要了",
/// `full` 是"连物料一起拉",`offline` 是"绝不联网、只吃本地完整包"。四个
/// `bool` 挨着传,调用点上是裸 `true`/
/// `false`,谁是谁全靠位置记 —— 而记错的后果分别是"没看计划就执行"和
/// "手工改动被丢掉"。
#[derive(Debug, Clone, Copy, Default)]
pub struct InstallOpts {
    /// 过闸执行,而不只是看计划。
    pub yes: bool,
    /// 连物料层一起拉(离线现场)。
    pub full: bool,
    /// 禁止访问 registry 或物料源；缺一个字节就立刻失败。
    pub offline: bool,
    /// 上一版目录里有本地改动也照旧升级。**`yes` 跨不过它。**
    pub force: bool,
}

/// 引用的 tag 位是范围时,去 registry 问版本、挑最高的合格者。
///
/// **精确 tag 直接返回,不问。** 少一次网络往返只是顺带的;真正的理由是
/// 有些仓库只给 pull 权限、列不了 tag —— 精确引用在那种仓库上必须照样能用,
/// 而"为了拉一个我已经指名道姓的 tag 先去列一遍目录"会让它平白失败。
async fn resolve_ref_range(reference: &str) -> Result<String> {
    let Some((repo_path, tag)) = split_tag(reference) else {
        return Ok(reference.to_string()); // 没写 tag,交给下游按 latest 处理
    };
    let req = crate::version_req::VersionReq::parse(tag).map_err(|e| anyhow::anyhow!("{e}"))?;
    if let Some(exact) = req.as_exact() {
        let _ = exact;
        return Ok(reference.to_string());
    }
    say!("{reference} —— 问 registry 有哪些版本");
    let tags = ImageStore::list_tags(repo_path).await?;
    let refs: Vec<&str> = tags.iter().map(String::as_str).collect();
    match req.best(refs) {
        Some(v) => {
            say!("  {tag} → {v}");
            Ok(format!("{repo_path}:{v}"))
        }
        None => bail!(
            "{repo_path} 上没有匹配 `{tag}` 的版本。\n现有:{}",
            if tags.is_empty() {
                "(一个都没有)".to_string()
            } else {
                tags.join(", ")
            }
        ),
    }
}

/// 从引用里切出 `(仓库路径, tag)`。
///
/// 只认**最后一段**里的冒号 —— `localhost:5000/ns/yq` 的第一个冒号是端口,
/// 按第一个冒号切会把主机名切断,得到一个根本不存在的仓库。
fn split_tag(reference: &str) -> Option<(&str, &str)> {
    let last = reference.rsplit('/').next()?;
    let i = last.find(':')?;
    let at = reference.len() - last.len() + i;
    Some((&reference[..at], &reference[at + 1..]))
}

pub async fn install(
    source: &str,
    target: &crate::target::TargetOpts,
    sets: &[String],
    name: Option<&str>,
    repo: Option<&str>,
    opts: InstallOpts,
) -> Result<()> {
    let InstallOpts {
        yes,
        full,
        offline,
        force,
    } = opts;
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
    } else if !local.exists() && !offline {
        // 完整引用,但 tag 位可能是个**范围**(`reg/ns/yq:4.*`)。
        // 这正是 helm#11000 里那个原例(`helm pull oci://… --version '0.0.*'`)
        // —— 靠 `tags/list` 问出远端有哪些版本,再挑最高的合格者。
        // 不需要索引:发现版本是 OCI 自带的能力,发现**有哪些包**才不是。
        resolved = resolve_ref_range(source).await?;
        resolved.as_str()
    } else {
        source
    };
    // 这次是不是换版本 —— 后面 D-135 那句提醒要用。
    let mut upgrading = false;
    let mut selected_seed = None;
    let bp_file = if local.exists() {
        locate(local)?.0
    } else {
        let store = ImageStore::open()?;
        if offline {
            if !store.has(source) {
                bail!(
                    "{source} 不在本地 store —— `--offline` 不会访问 registry。\n\
                     在有网处先执行:`crater pull {source} --offline`，\n\
                     或用 `crater load <包>.pkg.tar` 把完整包搬进来。"
                );
            }
            if store.has_site_seeds(source)? {
                let platform = target.offline_platform()?;
                selected_seed = store.local_site_seed(source, &platform)?;
                if selected_seed.is_none() {
                    bail!(
                        "{source} 缺少 {}/{}/{} 的本地 Site Seed。\n\
                         在有网处先执行:`crater pull {source} --offline -i <inventory>`，\n\
                         再用 `crater save/load` 搬到断网环境。",
                        platform.arch,
                        platform.os,
                        platform.version
                    );
                }
            } else if !store.has_all_layers(source) {
                bail!(
                    "{source} 在本地但不是完整离线包。\n\
                     在有网处先执行:`crater pull {source} --offline`，\n\
                     再进入断网环境执行 apply。"
                );
            }
        } else if full {
            store.pull(source).await?
        } else {
            store.pull_thin(source).await?
        }
        let m = store.resolve_manifest(source)?;
        let cfg = read_config(&store, &m)?;
        warn_if_newer(&cfg);
        let pkg_name = cfg["name"].as_str().unwrap_or("pkg").to_string();

        // ── 目录布局:`<包名>-<版本>`(D-141)。
        //
        // D-128 的封条挡住了"静默装错版本",但代价是换版本只能人工搬目录。
        // 换成版本化目录,那次事故就**发生不了**而不是被拦下来:两个版本
        // 天生不共用一个目录。另外两条同样重要:
        //
        // - app 文件的 `blueprint:` 会跟着变,于是一次升级在 `git diff` 里
        //   是看得见的一行;`<包名>/` 布局下升级在版本库里毫无痕迹。
        // - 旧版本原样留着 —— 回退不用连网,离线现场也能回。
        //
        // 老布局摊出来的 `<包名>/` 若正是这一版,就原地用,不强行搬家。
        let legacy = PathBuf::from(&pkg_name);
        let dir = if legacy.is_dir() && stamped(&legacy).as_deref() == Some(source) {
            legacy.clone()
        } else {
            PathBuf::from(pkg_dir_name(&pkg_name, source))
        };

        // ── 升级闸门:在摊开任何字节**之前**,先问上一版有没有被人动过。
        let app_name = name.unwrap_or(&pkg_name).to_string();
        let prev_dir = app_points_at(&app_name)
            .map(|(_, d)| d)
            .filter(|d| d.is_dir() && d != &dir)
            .or_else(|| (legacy.is_dir() && legacy != dir).then(|| legacy.clone()));
        if let Some(prev) = prev_dir {
            if let Some(prev_ref) = stamped(&prev).filter(|p| p != source) {
                upgrading = true;
                upgrade_gate(&prev, &prev_ref, source, force)?;
            }
        }

        if !dir.exists() {
            let layer = m["layers"]
                .as_array()
                .and_then(|ls| {
                    ls.iter()
                        .find(|l| l["mediaType"].as_str() == Some(MT_PKG_LAYER))
                })
                .ok_or_else(|| anyhow::anyhow!("{source} 不是 crater 蓝图包"))?;
            let d = layer["digest"].as_str().unwrap_or_default();
            let bytes = std::fs::read(store.blob_path(d.trim_start_matches("sha256:")))?;
            crater_core::bundle::untar_gz_into(&dir, &bytes, 0)?;
            stamp(&dir, source)?;
            say!("{source} → {}/", dir.display());
        } else {
            // 目录已在:默认用本地那份(它可能有本地改动,重装不该冲掉)。
            //
            // 印记检查留着当兜底 —— 版本化目录之后它只剩一种触发方式:
            // 两个 registry 上同名同 tag 的包。那仍然是"装的不是你以为的
            // 那个",仍然该拦。
            match stamped(&dir) {
                Some(prev) if prev != source => bail!(
                    "{} 里是 `{prev}`,这次要装的是 `{source}` —— 同名同版本、来源不同。\n\
                     换个位置:`crater pull {source} --into <目录>`,\n\
                     或先把 {} 移开。",
                    dir.display(),
                    dir.display()
                ),
                _ => {
                    say!("{} 已存在 —— 用本地这份(包已在 store)", dir.display());
                    // 重装同一版不覆盖任何字节,所以这里只报不拦:人要知道
                    // 自己跑的是"包 + 我改过的那几处",不是原样的包。
                    if let Drift::Changed(items) = compare_to_package(&dir, source) {
                        crate::oops!(
                            "{} 有 {} 处本地改动,这次装的是改过的那份:",
                            dir.display(),
                            items.len()
                        );
                        for i in &items {
                            say!("    {i}");
                        }
                    }
                }
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
                 改 {} 里的组,或用 `crater inspect` 再看一遍契约。",
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
        target.closure =
            Some(selected_seed.unwrap_or_else(|| PathBuf::from(format!("oci://{source}"))));
    }
    let target = &target;
    // 换版本时,先把"这次判不判得出"讲清楚,再印计划(D-135)。
    if upgrading {
        warn_if_undecidable(&bp, source, full || target.closure.is_some());
    }
    say!();
    crate::blueprint::plan_blueprint(&bp_file, target, sets).await?;
    if !yes {
        say!();
        say!("以上是计划,**什么都没改**。确认后执行:");
        say!(
            "  crater apply -f {} -i {} {}",
            bp_file.display(),
            target
                .inventory
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<机群>".into()),
            sets.iter()
                .map(|s| format!("--set {s}"))
                .collect::<Vec<_>>()
                .join(" ")
        );
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
    [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "api_key",
        "credential",
    ]
    .iter()
    .any(|k| n.contains(k))
}

/// 把 app 文件里的 `blueprint:` 指到新版本;文件里没有这一行返回 `false`。
///
/// 逐行改而不是 `serde_yaml` 读回再写出:后者会把注释、空行、键序全抹平,
/// 而这个文件的开头两行注释("改它 = 改任务")正是它自我说明的那部分。
/// 升级不该顺手把人的文件重排一遍。
fn repoint_app(app_file: &Path, new_bp: &Path) -> Result<bool> {
    let text = std::fs::read_to_string(app_file)?;
    let mut out = String::with_capacity(text.len() + 64);
    let mut done = false;
    for line in text.lines() {
        let t = line.trim_start();
        if !done && t.starts_with("blueprint:") {
            out.push_str(&line[..line.len() - t.len()]);
            out.push_str(&format!("blueprint: {}\n", new_bp.display()));
            done = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if done {
        std::fs::write(app_file, out)?;
    }
    Ok(done)
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
        // 升级时**只改 `blueprint:` 这一行**,别的一个字不动。
        //
        // 不动的理由:params / inventory / verify 是操作者自己填的,整份重写
        // 等于把他的改动抹掉,而这个文件正是"这次安装的正身"。
        // 要动这一行的理由:UI 与后续 apply 都按它找蓝图(`ui_app::parse_app`),
        // 不改它就是"目标机装了新版、app 文件还指着旧版" —— 下一次从 UI 点
        // 一下 apply 就悄悄退回旧版本了。
        let prev = std::fs::read_to_string(&out)
            .ok()
            .and_then(|t| crate::ui_app::parse_app(&out, &t).ok())
            .map(|d| d.blueprint);
        let now = bp_file.display().to_string();
        if prev.as_deref() == Some(now.as_str()) {
            say!("{} 已存在 —— 保留不动(改它就是改这次安装)", out.display());
        } else if repoint_app(&out, bp_file)? {
            say!(
                "任务 {} 换版本:blueprint {} → {now}",
                out.display(),
                prev.as_deref().unwrap_or("?")
            );
            say!("  (params / inventory / verify 原样不动 —— 那是你填的)");
        } else {
            crate::oops!(
                "{} 里没有 `blueprint:` 一行 —— 没动它。请手工指到 {now},否则它还跑旧版本。",
                out.display()
            );
        }
        return Ok(());
    }
    let is_secret =
        |k: &str| bp.params.get(k).map(|p| p.secret).unwrap_or(false) || looks_secret(k);
    let secret: Vec<&str> = given
        .keys()
        .filter(|k| is_secret(k))
        .map(|s| s.as_str())
        .collect();
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
        let items: Vec<String> = l.split(',').map(|s| s.trim().to_string()).collect();
        y.push_str(&format!("  limit: [{}]\n", items.join(", ")));
    }
    let plain: Vec<(&String, &serde_yaml::Value)> =
        given.iter().filter(|(k, _)| !is_secret(k)).collect();
    if !plain.is_empty() {
        y.push_str("  params:\n");
        for (k, v) in plain {
            let s = serde_yaml::to_string(v)
                .unwrap_or_default()
                .trim()
                .to_string();
            y.push_str(&format!("    {k}: {s}\n"));
        }
    }
    if !secret.is_empty() {
        y.push_str("  # 敏感参数不写进这个文件(它是要进 git 的):\n");
        for k in &secret {
            y.push_str(&format!(
                "  #   {k} —— 每次用 `--set {k}=…`,或放进机群 vars\n"
            ));
        }
    }
    std::fs::write(&out, y)?;
    say!("任务 → {}", out.display());
    if !secret.is_empty() {
        crate::oops!(
            "敏感参数 {} 未写入 app 文件 —— 每次执行都要重新给。",
            secret.join(", ")
        );
    }
    Ok(())
}
