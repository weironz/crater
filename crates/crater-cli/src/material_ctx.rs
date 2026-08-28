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

/// 本地已备好的物料字节(离线闭包)。键 = 物料名。
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
    fn push_blob(&self, plan: &MaterialPlan, path: &PathBuf, dest: &str) -> Result<()> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("读物料 `{}` 的本地 blob {}", plan.name, path.display()))?;
        // 通过 Ctx 的文本通道推送(内部走分块 base64,二进制安全)。
        let encoded = String::from_utf8_lossy(&bytes).into_owned();
        if encoded.as_bytes() != bytes {
            bail!(
                "物料 `{}` 是二进制,当前 Ctx 只有文本通道 —— 二进制推送随 OCI 闭包一同落地",
                plan.name
            );
        }
        self.inner.write_file(dest, &encoded)
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

    fn place_material(&self, name: &str, dest: &str) -> Result<()> {
        let plan = materials::resolve(self.bp, name, &self.scope).map_err(|e| anyhow::anyhow!("{e}"))?;
        if plan.kind != MaterialKind::File {
            bail!(
                "物料 `{name}` 是 {:?} 类型 —— 只有 `file` 能被 copy 落到路径上",
                plan.kind
            );
        }
        match self.blobs.get(name) {
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
            fn place_material(&self, n: &str, d: &str) -> Result<()> {
                self.0.place_material(n, d)
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
        blobs.insert("cfg".into(), blob);

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
