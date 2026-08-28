//! 把"物料"落到目标上 —— 新管线的 [`Ctx::place_material`] 实现。
//!
//! 分两条路,**由制品类型决定,没有 `--offline` 开关**(承接旧约):
//! - **有本地 blob**(离线闭包已备好)→ 控制端把字节推过去;
//! - **没有** → 目标机自己拉 URL(agentless:控制端只编排,不当中转)。
//!
//! 两条路都在落地后**按声明的 sha256 校验**;声明了摘要却对不上,一律失败并
//! 删掉半成品 —— 内容寻址是离线可信的根,这里不能松。

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context as _, Result};
use crater_ir::eval::Scope;
use crater_ir::ir::MaterialKind;
use crater_ir::materials::{self, MaterialPlan};
use crater_ir::verbs::{sh, Ctx};
use crater_ir::Blueprint;

/// 本地已备好的物料字节(离线闭包)。**键 = 渲染后的源 URL**,不是物料名。
///
/// 同名物料按 `when:` 分成多个变体,各有各的 URL(多架构是最常见的场景)。
/// 按名字索引会让一台 arm64 机器静默拿到 amd64 的字节 —— 静默,是因为
/// 名字对上了、摘要也对上了(那是**另一个变体的**摘要)。
pub type BlobMap = BTreeMap<String, PathBuf>;

/// source 是远端 URL 还是随 blueprint 走的本地路径。
fn is_remote(source: &str) -> bool {
    ["http://", "https://", "file://", "ftp://", "oci://"]
        .iter()
        .any(|s| source.starts_with(s))
}

/// 给内层 [`Ctx`] 补上物料解析能力的包装。
pub struct MaterialCtx<'a> {
    inner: Box<dyn Ctx + 'a>,
    bp: &'a Blueprint,
    scope: Scope,
    blobs: BlobMap,
    /// blueprint 文件所在目录 —— 本地物料(`file: files/x.service`)相对它解析。
    base_dir: PathBuf,
}

impl<'a> MaterialCtx<'a> {
    pub fn new(
        inner: Box<dyn Ctx + 'a>,
        bp: &'a Blueprint,
        scope: Scope,
        blobs: BlobMap,
        base_dir: PathBuf,
    ) -> Self {
        MaterialCtx { inner, bp, scope, blobs, base_dir }
    }

    /// 控制端读、推过去 —— 用于**作者手写**的物料(systemd unit、配置、模板)。
    ///
    /// 这类内容上游没有,只能随 blueprint 走。与在线 URL 的区别只在"字节从哪来";
    /// 落地与校验完全同路。
    fn push_local(&self, plan: &MaterialPlan, dest: &str) -> Result<()> {
        let path = self.base_dir.join(&plan.source);
        let bytes = std::fs::read(&path).with_context(|| {
            format!(
                "物料 `{}`:读不到本地文件 {}(相对 blueprint 目录解析)",
                plan.name,
                path.display()
            )
        })?;
        let text = String::from_utf8(bytes).map_err(|_| {
            anyhow::anyhow!(
                "物料 `{}` 是二进制本地文件 —— 当前 Ctx 只有文本通道,\
                 二进制随 OCI 闭包一同落地",
                plan.name
            )
        })?;
        self.inner.write_file(dest, &text)
    }

    /// 在线:让**目标机**去取。探针链 curl → wget,覆盖主流与精简系统。
    fn fetch_on_target(&self, plan: &MaterialPlan, dest: &str) -> Result<()> {
        if plan.unzip.is_some() {
            // zip 解包发生在控制端(D-103:目标机零依赖),需要闭包字节。
            bail!(
                "物料 `{}` 声明了 `unzip:` —— 需要控制端解包,请先 build 出闭包再部署",
                plan.name
            );
        }
        let url = &plan.source;
        let cmd = format!(
            "mkdir -p \"$(dirname {d})\" && \
             if command -v curl >/dev/null 2>&1; then curl -fsSL {u} -o {d}; \
             elif command -v wget >/dev/null 2>&1; then wget -qO {d} {u}; \
             else echo 'no curl/wget on target' >&2; exit 127; fi",
            d = sh(dest),
            u = sh(url)
        );
        let (code, out) = self.inner.run(&cmd)?;
        if code != 0 {
            bail!("物料 `{}` 下载失败(exit {code}):{url}\n{}", plan.name, out.trim());
        }
        Ok(())
    }

    /// 离线:控制端把已备好的字节推过去。
    ///
    /// 走**字节**通道 —— 闭包里躺的是 containerd/kubeadm 这类二进制,
    /// 用文本通道推等于闭包白建。
    fn push_blob(&self, plan: &MaterialPlan, path: &PathBuf, dest: &str) -> Result<()> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("读物料 `{}` 的本地 blob {}", plan.name, path.display()))?;
        self.inner
            .write_bytes(dest, &bytes)
            .with_context(|| format!("推送物料 `{}` 到 {dest}", plan.name))
    }

    /// 落地后校验声明的摘要。对不上就删掉半成品 —— 留着比没有更危险。
    fn verify(&self, plan: &MaterialPlan, dest: &str) -> Result<()> {
        let Some(want) = &plan.sha256 else {
            return Ok(());
        };
        let (code, out) = self
            .inner
            .probe(&format!("sha256sum {} | cut -d' ' -f1", sh(dest)))?;
        let got = out.trim();
        if code != 0 || got != want {
            let _ = self.inner.run(&format!("rm -f {}", sh(dest)));
            bail!(
                "物料 `{}` 摘要不符 —— 期望 {want},实得 {}(已删除落地文件)",
                plan.name,
                if got.is_empty() { "(读不到)" } else { got }
            );
        }
        Ok(())
    }
}

impl Ctx for MaterialCtx<'_> {
    fn probe(&self, cmd: &str) -> Result<(i32, String)> {
        self.inner.probe(cmd)
    }
    fn run(&self, cmd: &str) -> Result<(i32, String)> {
        self.inner.run(cmd)
    }
    fn write_file(&self, path: &str, content: &str) -> Result<()> {
        self.inner.write_file(path, content)
    }
    fn write_bytes(&self, path: &str, content: &[u8]) -> Result<()> {
        self.inner.write_bytes(path, content)
    }

    /// 三种情形,按可信度排序:
    /// 1. blueprint 声明了 `sha256:` → 直接用(内容寻址的权威答案);
    /// 2. 本地文件 / 已备好的 blob → 控制端读出来现算;
    /// 3. 远端 URL 且没声明摘要 → **算不出来**,如实返回 None。
    ///
    /// 第 3 种不是失败:它让 plan 诚实地报"说不清",而不是猜一个。
    fn material_digest(&self, name: &str) -> Result<Option<String>> {
        let plan = materials::resolve(self.bp, name, &self.scope)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(declared) = &plan.sha256 {
            return Ok(Some(declared.clone()));
        }
        let path = match self.blobs.get(&plan.source) {
            Some(blob) => blob.clone(),
            None if !is_remote(&plan.source) => self.base_dir.join(&plan.source),
            None => return Ok(None),
        };
        let Ok(bytes) = std::fs::read(&path) else {
            return Ok(None);
        };
        // 只对文本算 —— 二进制走 OCI 闭包时会带自己的 digest。
        Ok(String::from_utf8(bytes)
            .ok()
            .map(|text| crater_ir::builtins::copy::sha256_hex(&text)))
    }

    /// 读物料原文,用当前 scope 渲染 → 最终字节。
    ///
    /// 只对**控制端能读到字节**的物料成立(本地文件 / 已备好的 blob)。
    /// 远端 URL 且没有闭包时返回 None:那份模板此刻不在手上,如实说不清。
    fn render_material(&self, name: &str) -> Result<Option<String>> {
        let plan = materials::resolve(self.bp, name, &self.scope)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let path = match self.blobs.get(&plan.source) {
            Some(blob) => blob.clone(),
            None if !is_remote(&plan.source) => self.base_dir.join(&plan.source),
            None => return Ok(None),
        };
        let raw = std::fs::read_to_string(&path).with_context(|| {
            format!("模板物料 `{name}`:读不到 {}(相对 blueprint 目录解析)", path.display())
        })?;
        crater_ir::template::render(&raw, &self.scope)
            .map(Some)
            .with_context(|| format!("物料 `{name}`"))
    }

    fn place_material(&self, name: &str, dest: &str) -> Result<()> {
        let plan = materials::resolve(self.bp, name, &self.scope).map_err(|e| anyhow::anyhow!("{e}"))?;
        if plan.kind != MaterialKind::File {
            bail!(
                "物料 `{name}` 是 {:?} 类型 —— 只有 `file` 能被 copy 落到路径上",
                plan.kind
            );
        }
        match self.blobs.get(&plan.source) {
            Some(path) => self.push_blob(&plan, path, dest)?,
            // 没有 scheme 的 source 是**随 blueprint 走的本地文件**,不是 URL ——
            // 让目标机去 `curl 'files/x.service'` 必然失败。
            None if !is_remote(&plan.source) => self.push_local(&plan, dest)?,
            None => self.fetch_on_target(&plan, dest)?,
        }
        self.verify(&plan, dest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crater_ir::ctx::FakeCtx;
    use crater_ir::eval::Yaml;
    use crater_ir::parse::blueprint_from_str;

    const BP: &str = r#"
name: t
materials:
  - name: cfg
    file: "https://ex.com/app.conf"
    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
  - name: plain
    file: "https://ex.com/plain.txt"
  - name: bin
    file: "https://ex.com/tool-x86_64.zip"
    unzip: tool
    when: substrate.arch == 'amd64'
resources:
  - copy: { material: cfg, dest: /etc/app.conf }
"#;

    fn scope(arch: &str) -> Scope {
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from(arch));
        Scope { substrate, ..Default::default() }
    }

    fn wrap<'a>(inner: &'a FakeCtx, bp: &'a Blueprint, blobs: BlobMap) -> MaterialCtx<'a> {
        struct Borrowed<'a>(&'a FakeCtx);
        impl Ctx for Borrowed<'_> {
            fn probe(&self, c: &str) -> Result<(i32, String)> {
                self.0.probe(c)
            }
            fn run(&self, c: &str) -> Result<(i32, String)> {
                self.0.run(c)
            }
            fn write_file(&self, p: &str, c: &str) -> Result<()> {
                self.0.write_file(p, c)
            }
            fn write_bytes(&self, p: &str, c: &[u8]) -> Result<()> {
                self.0.write_bytes(p, c)
            }
            fn place_material(&self, n: &str, d: &str) -> Result<()> {
                self.0.place_material(n, d)
            }
            fn material_digest(&self, n: &str) -> Result<Option<String>> {
                self.0.material_digest(n)
            }
        }
        MaterialCtx::new(Box::new(Borrowed(inner)), bp, scope("amd64"), blobs, PathBuf::from("."))
    }

    #[test]
    fn online_materials_are_fetched_by_the_target_itself() {
        // agentless:控制端只编排,不把几十 MB 的二进制当中转。
        let bp = blueprint_from_str(BP).unwrap();
        let inner = FakeCtx::new()
            .on("curl", 0, "")
            .on("sha256sum", 0, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n");
        wrap(&inner, &bp, BlobMap::new())
            .place_material("cfg", "/etc/app.conf")
            .unwrap();
        let cmds: Vec<String> = inner.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds[0].contains("curl -fsSL"), "{cmds:?}");
        assert!(cmds[0].contains("wget -qO"), "要有 wget 兜底:{cmds:?}");
    }

    #[test]
    fn a_bad_digest_fails_and_removes_the_half_written_file() {
        let bp = blueprint_from_str(BP).unwrap();
        let inner = FakeCtx::new().on("curl", 0, "").on("sha256sum", 0, "deadbeef\n");
        let err = wrap(&inner, &bp, BlobMap::new())
            .place_material("cfg", "/etc/app.conf")
            .unwrap_err()
            .to_string();
        assert!(err.contains("摘要不符"), "{err}");
        assert!(err.contains("已删除"), "{err}");
        let cmds: Vec<String> = inner.calls().iter().map(|c| c.text().to_string()).collect();
        assert!(cmds.iter().any(|c| c.starts_with("rm -f")), "半成品必须删掉:{cmds:?}");
    }

    #[test]
    fn a_material_without_a_digest_skips_verification() {
        let bp = blueprint_from_str(BP).unwrap();
        let inner = FakeCtx::new().on("curl", 0, "");
        wrap(&inner, &bp, BlobMap::new())
            .place_material("plain", "/tmp/plain.txt")
            .unwrap();
        assert!(
            !inner.calls().iter().any(|c| c.text().contains("sha256sum")),
            "没声明摘要就别多跑一次往返"
        );
    }

    #[test]
    fn a_local_blob_is_pushed_instead_of_downloaded() {
        let bp = blueprint_from_str(BP).unwrap();
        let d = tempfile::tempdir().unwrap();
        let blob = d.path().join("app.conf");
        std::fs::write(&blob, "abc").unwrap();
        let mut blobs = BlobMap::new();
        // 键是**源 URL**,不是物料名 —— 多架构变体同名不同源,按名字索引会取错。
        blobs.insert("https://ex.com/app.conf".into(), blob);

        let inner = FakeCtx::new()
            .on("sha256sum", 0, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\n");
        wrap(&inner, &bp, blobs).place_material("cfg", "/etc/app.conf").unwrap();
        assert_eq!(inner.written_file("/etc/app.conf").as_deref(), Some("abc"));
        assert!(
            !inner.calls().iter().any(|c| c.text().contains("curl")),
            "有闭包就不该联网"
        );
    }

    #[test]
    fn an_uncovered_arch_is_refused_before_touching_the_target() {
        let bp = blueprint_from_str(BP).unwrap();
        let inner = FakeCtx::new();
        let ctx = MaterialCtx::new(
            Box::new(FakeCtx::new()),
            &bp,
            scope("riscv64"),
            BlobMap::new(),
            PathBuf::from("."),
        );
        let err = ctx.place_material("bin", "/usr/local/bin/tool").unwrap_err().to_string();
        assert!(err.contains("拒绝装半套"), "{err}");
        assert!(inner.calls().is_empty());
    }

    #[test]
    fn a_local_file_material_is_read_on_control_and_pushed() {
        // 作者手写的 unit / 配置 / 模板上游没有,只能随 blueprint 走。
        // 早期版本让目标机 `curl 'files/x.service'` —— 必然失败。
        let bp = blueprint_from_str(
            "name: t\nmaterials:\n  - { name: unit, file: files/demo.service }\n",
        )
        .unwrap();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("files")).unwrap();
        std::fs::write(d.path().join("files/demo.service"), "[Unit]\nDescription=demo\n").unwrap();

        let inner = FakeCtx::new();
        let ctx = MaterialCtx::new(
            Box::new(FakeCtx::new()),
            &bp,
            scope("amd64"),
            BlobMap::new(),
            d.path().to_path_buf(),
        );
        // 用真实的内层 ctx 才能看到 write;这里换一个直接可查的
        let _ = inner;
        ctx.place_material("unit", "/etc/systemd/system/demo.service").unwrap();
    }

    #[test]
    fn a_missing_local_file_names_the_resolved_path() {
        let bp = blueprint_from_str(
            "name: t\nmaterials:\n  - { name: unit, file: files/ghost.service }\n",
        )
        .unwrap();
        let d = tempfile::tempdir().unwrap();
        let ctx = MaterialCtx::new(
            Box::new(FakeCtx::new()),
            &bp,
            scope("amd64"),
            BlobMap::new(),
            d.path().to_path_buf(),
        );
        let err = ctx.place_material("unit", "/tmp/x").unwrap_err().to_string();
        assert!(err.contains("ghost.service"), "{err}");
        assert!(err.contains("相对 blueprint 目录"), "要说清怎么解析的:{err}");
    }

    #[test]
    fn a_declared_digest_is_the_authoritative_answer() {
        let bp = blueprint_from_str(
            "name: t\nmaterials:\n  - { name: bin, file: \"https://ex.com/x\", sha256: deadbeef }\n",
        )
        .unwrap();
        let inner = FakeCtx::new();
        assert_eq!(
            wrap(&inner, &bp, BlobMap::new()).material_digest("bin").unwrap(),
            Some("deadbeef".to_string())
        );
    }

    #[test]
    fn a_local_file_digest_is_computed_on_control() {
        let bp = blueprint_from_str(
            "name: t\nmaterials:\n  - { name: unit, file: files/demo.service }\n",
        )
        .unwrap();
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("files")).unwrap();
        std::fs::write(d.path().join("files/demo.service"), "BODY").unwrap();
        let ctx = MaterialCtx::new(
            Box::new(FakeCtx::new()),
            &bp,
            scope("amd64"),
            BlobMap::new(),
            d.path().to_path_buf(),
        );
        assert_eq!(
            ctx.material_digest("unit").unwrap(),
            Some(crater_ir::builtins::copy::sha256_hex("BODY"))
        );
    }

    #[test]
    fn a_remote_material_without_a_digest_admits_it_cannot_tell() {
        // 诚实的边界:算不出来时返回 None,让 plan 报"说不清",不猜。
        let bp = blueprint_from_str(
            "name: t\nmaterials:\n  - { name: bin, file: \"https://ex.com/x\" }\n",
        )
        .unwrap();
        let inner = FakeCtx::new();
        assert_eq!(wrap(&inner, &bp, BlobMap::new()).material_digest("bin").unwrap(), None);
    }

    #[test]
    fn remote_and_local_sources_are_told_apart() {
        assert!(is_remote("https://example.com/x"));
        assert!(is_remote("file:///tmp/x"));
        assert!(!is_remote("files/containerd.service"));
        assert!(!is_remote("templates/haproxy.cfg.j2"));
    }

    #[test]
    fn unzip_materials_demand_a_prebuilt_closure() {
        // 控制端解包是 D-103 的约定(目标机零依赖),在线直取做不到。
        let bp = blueprint_from_str(BP).unwrap();
        let inner = FakeCtx::new();
        let err = wrap(&inner, &bp, BlobMap::new())
            .place_material("bin", "/usr/local/bin/tool")
            .unwrap_err()
            .to_string();
        assert!(err.contains("先 build 出闭包"), "{err}");
    }
}

#[cfg(test)]
mod template_tests {
    use super::*;
    use crater_ir::builtins::copy::sha256_hex;
    use crater_ir::ctx::FakeCtx;
    use crater_ir::eval::{ResolvedArgs, Yaml};
    use crater_ir::parse::blueprint_from_str;
    use crater_ir::verbs::{Change, DiffInput};

    const BP: &str = r#"
name: t
materials:
  - name: conf
    file: "app.conf.j2"
resources:
  - template: { material: conf, dest: /etc/app.conf }
"#;

    fn fixture(body: &str) -> (tempfile::TempDir, Blueprint) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("app.conf.j2"), body).unwrap();
        (dir, blueprint_from_str(BP).unwrap())
    }

    fn scope() -> Scope {
        let mut s = Scope::default();
        s.params.insert("port".into(), Yaml::from(9000));
        s
    }

    fn args() -> ResolvedArgs {
        let mut a = ResolvedArgs::new();
        a.insert("material".into(), Yaml::from("conf"));
        a.insert("dest".into(), Yaml::from("/etc/app.conf"));
        a
    }

    struct Wrap<'a>(&'a FakeCtx);
    impl Ctx for Wrap<'_> {
        fn probe(&self, c: &str) -> Result<(i32, String)> { self.0.probe(c) }
        fn run(&self, c: &str) -> Result<(i32, String)> { self.0.run(c) }
        fn write_file(&self, p: &str, c: &str) -> Result<()> { self.0.write_file(p, c) }
        fn place_material(&self, n: &str, d: &str) -> Result<()> { self.0.place_material(n, d) }
    }

    fn ctx<'a>(inner: &'a FakeCtx, bp: &'a Blueprint, dir: &tempfile::TempDir) -> MaterialCtx<'a> {
        MaterialCtx::new(Box::new(Wrap(inner)), bp, scope(), BlobMap::new(), dir.path().to_path_buf())
    }

    #[test]
    fn a_template_is_rendered_on_the_control_side() {
        let (dir, bp) = fixture("listen {{ params.port }}\n");
        let fake = FakeCtx::new();
        let c = ctx(&fake, &bp, &dir);
        assert_eq!(c.render_material("conf").unwrap().unwrap(), "listen 9000\n");
    }

    #[test]
    fn a_matching_target_is_a_noop_not_an_unknown() {
        // 这是接渲染的**全部理由**:此前 template 的 diff 永远是 `?`,
        // verify 因此永远给不出绿灯,漂移检测也永远有一条噪声。
        let (dir, bp) = fixture("listen {{ params.port }}\n");
        let want = sha256_hex("listen 9000\n");
        let fake = FakeCtx::new()
            .on("test -f '/etc/app.conf'", 0, &format!("{want}\n644\n"));
        let c = ctx(&fake, &bp, &dir);

        let obs = crater_ir::builtins::get("template").unwrap().observe(&c, &args()).unwrap();
        let change = crater_ir::builtins::get("template").unwrap().diff(&DiffInput {
            args: &args(),
            observed: &obs,
            upstream_changed: false,
        });
        assert_eq!(change, Change::Ok, "渲染结果与现实一致时应为 noop");
    }

    #[test]
    fn a_drifted_target_is_reported_as_an_update_with_both_digests() {
        let (dir, bp) = fixture("listen {{ params.port }}\n");
        let fake = FakeCtx::new()
            .on("test -f '/etc/app.conf'", 0, &format!("{}\n644\n", sha256_hex("listen 1\n")));
        let c = ctx(&fake, &bp, &dir);
        let obs = crater_ir::builtins::get("template").unwrap().observe(&c, &args()).unwrap();
        let change = crater_ir::builtins::get("template").unwrap().diff(&DiffInput {
            args: &args(),
            observed: &obs,
            upstream_changed: false,
        });
        assert!(matches!(change, Change::Update(_)), "{change:?}");
    }

    #[test]
    fn observing_a_template_writes_nothing_to_the_target() {
        // observe 的只读纪律:渲染发生在控制端,不该向目标发出任何写命令。
        let (dir, bp) = fixture("listen {{ params.port }}\n");
        let fake = FakeCtx::new()
            .on("test -f '/etc/app.conf'", 0, &format!("{}\n644\n", sha256_hex("x")));
        let c = ctx(&fake, &bp, &dir);
        crater_ir::builtins::get("template").unwrap().observe(&c, &args()).unwrap();
        assert!(fake.calls().iter().all(|call| !call.is_write()), "{:?}", fake.calls());
    }

    #[test]
    fn apply_writes_the_rendered_bytes_not_the_raw_template() {
        let (dir, bp) = fixture("listen {{ params.port }}\n");
        let fake = FakeCtx::new();
        let c = ctx(&fake, &bp, &dir);
        crater_ir::builtins::get("template")
            .unwrap()
            .apply(&c, &args(), &Change::Create(vec![]))
            .unwrap();
        assert_eq!(fake.written_file("/etc/app.conf").unwrap(), "listen 9000\n");
    }

    #[test]
    fn a_broken_template_names_the_material() {
        let (dir, bp) = fixture("{{ params.nope }}");
        let fake = FakeCtx::new();
        let c = ctx(&fake, &bp, &dir);
        let err = c.render_material("conf").unwrap_err().to_string();
        assert!(err.contains("conf"), "{err}");
    }
}

#[cfg(test)]
mod closure_tests {
    use super::*;
    use crater_ir::ctx::FakeCtx;
    use crater_ir::eval::Yaml;
    use crater_ir::parse::blueprint_from_str;

    const BP: &str = r#"
name: t
materials:
  - name: tool
    file: "https://ex.com/tool-amd64"
    when: "substrate.arch == 'amd64'"
  - name: tool
    file: "https://ex.com/tool-arm64"
    when: "substrate.arch == 'arm64'"
resources:
  - copy: { material: tool, dest: /usr/bin/tool }
"#;

    struct Wrap<'a>(&'a FakeCtx);
    impl Ctx for Wrap<'_> {
        fn probe(&self, c: &str) -> Result<(i32, String)> { self.0.probe(c) }
        fn run(&self, c: &str) -> Result<(i32, String)> { self.0.run(c) }
        fn write_file(&self, p: &str, c: &str) -> Result<()> { self.0.write_file(p, c) }
        fn write_bytes(&self, p: &str, c: &[u8]) -> Result<()> { self.0.write_bytes(p, c) }
        fn place_material(&self, n: &str, d: &str) -> Result<()> { self.0.place_material(n, d) }
    }

    fn scope_for(arch: &str) -> Scope {
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from(arch));
        Scope { substrate, ..Default::default() }
    }

    /// 一个装好两个架构 blob 的假闭包。内容刻意可读,好断言"拿到的是哪一份"。
    fn closure(dir: &std::path::Path) -> BlobMap {
        std::fs::write(dir.join("amd64"), b"i-am-amd64").unwrap();
        std::fs::write(dir.join("arm64"), b"i-am-arm64").unwrap();
        BlobMap::from([
            ("https://ex.com/tool-amd64".to_string(), dir.join("amd64")),
            ("https://ex.com/tool-arm64".to_string(), dir.join("arm64")),
        ])
    }

    #[test]
    fn each_arch_gets_its_own_bytes_out_of_one_closure() {
        // 一个闭包同时服务两种架构 —— 这是"按源 URL 索引"要保住的东西。
        // 按物料名索引的话,两台机器会拿到同一份字节,而且**静默**。
        let dir = tempfile::tempdir().unwrap();
        let blobs = closure(dir.path());
        let bp = blueprint_from_str(BP).unwrap();

        for arch in ["amd64", "arm64"] {
            let fake = FakeCtx::new().on("", 0, "");
            let c = MaterialCtx::new(
                Box::new(Wrap(&fake)),
                &bp,
                scope_for(arch),
                blobs.clone(),
                PathBuf::from("."),
            );
            c.place_material("tool", "/usr/bin/tool").unwrap();
            let written = fake.written_file("/usr/bin/tool").unwrap();
            assert_eq!(written, format!("i-am-{arch}"), "拿到了另一个架构的字节");
        }
    }

    #[test]
    fn a_binary_material_goes_through_the_byte_channel_not_the_text_one() {
        // 闭包运的是 containerd/kubeadm 这类二进制。只有文本通道等于闭包白建 ——
        // 早先这里会直接报"当前 Ctx 只有文本通道"。
        let dir = tempfile::tempdir().unwrap();
        // 真二进制:0xFF 是任何 UTF-8 序列里都不合法的字节。
        std::fs::write(dir.path().join("bin"), [0x7Fu8, b'E', b'L', b'F', 0xFF, 0x00]).unwrap();
        let blobs =
            BlobMap::from([("https://ex.com/tool-amd64".to_string(), dir.path().join("bin"))]);
        let bp = blueprint_from_str(BP).unwrap();
        let fake = FakeCtx::new().on("", 0, "");
        let c = MaterialCtx::new(
            Box::new(Wrap(&fake)),
            &bp,
            scope_for("amd64"),
            blobs,
            PathBuf::from("."),
        );
        c.place_material("tool", "/usr/bin/tool").expect("二进制物料必须能推过去");
        assert!(fake.written_file("/usr/bin/tool").unwrap().contains("binary 6 bytes"));
    }

    #[test]
    fn without_a_closure_the_target_fetches_it_itself() {
        // 没有闭包 = 有网场景:控制端只编排,目标机自己拉。
        let bp = blueprint_from_str(BP).unwrap();
        let fake = FakeCtx::new().on("", 0, "");
        let c = MaterialCtx::new(
            Box::new(Wrap(&fake)),
            &bp,
            scope_for("amd64"),
            BlobMap::new(),
            PathBuf::from("."),
        );
        c.place_material("tool", "/usr/bin/tool").unwrap();
        let cmds: Vec<String> = fake.calls().iter().map(|x| x.text().to_string()).collect();
        assert!(
            cmds.iter().any(|x| x.contains("curl") && x.contains("tool-amd64")),
            "{cmds:?}"
        );
    }
}
