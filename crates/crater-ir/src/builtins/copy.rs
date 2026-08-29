//! `copy` —— 让目标路径的**内容**成为期望内容。
//!
//! 幂等靠内容寻址(sha256),不是靠时间戳:同样的内容重跑一律 `ok`。
//! 三种来源恰择其一:`content`(内联)/ `src`(控制端文件)/ `material`(离线闭包物料)。

use anyhow::Result;

use crate::builtins::file::run_ok;
use crate::eval::ResolvedArgs;
use crate::verbs::*;

/// 落 `owner:` / `group:` —— 两个类型都登记了这对字段,却都只应用了 `mode`。
///
/// 后果不是"权限差一点":redis-sentinel 要**重写自己的配置文件**,属主不对
/// 就直接拒绝启动。而 crater 这边报的是成功 —— 又一个"文档承诺过但没实现"。
pub(crate) fn apply_ownership(ctx: &dyn Ctx, path: &str, args: &ResolvedArgs) -> Result<()> {
    let owner = arg_str_opt(args, "owner");
    let group = arg_str_opt(args, "group");
    if owner.is_none() && group.is_none() {
        return Ok(());
    }
    let spec = format!("{}:{}", owner.unwrap_or(""), group.unwrap_or(""));
    run_ok(ctx, &format!("chown {} {}", sh(&spec), sh(path)))?;
    Ok(())
}

pub struct Copy;

impl ResourceType for Copy {
    fn name(&self) -> &'static str {
        "copy"
    }

    fn observe(&self, ctx: &dyn Ctx, args: &ResolvedArgs) -> Result<Observed> {
        let dest = arg_str(args, "dest")?;
        let cmd = format!(
            "test -f {d} && sha256sum {d} | cut -d' ' -f1 && stat -c '%a\n%U\n%G' {d}",
            d = sh(dest)
        );
        let (code, out) = ctx.probe(&cmd)?;
        if code != 0 {
            return Ok(Observed::absent());
        }
        let mut lines = out.lines();
        let mut obs = Observed::present([
            ("sha256", lines.next().unwrap_or_default().trim().to_string()),
            ("mode", lines.next().unwrap_or_default().trim().to_string()),
            ("owner", lines.next().unwrap_or_default().trim().to_string()),
            ("group", lines.next().unwrap_or_default().trim().to_string()),
        ]);
        // 内容来自物料时,把**期望摘要**一并取来 —— 它是控制端事实,
        // 没有它 diff 只能报"说不清",verify 就永远给不出绿灯。
        if let Some(name) = arg_str_opt(args, "material") {
            if let Some(want) = ctx.material_digest(name)? {
                obs.fields.insert("want_sha256".into(), want);
            }
        }
        Ok(obs)
    }

    fn diff(&self, input: &DiffInput) -> Change {
        let obs = input.observed;
        let desired_sha = arg_str_opt(input.args, "content").map(sha256_hex);

        if !obs.present {
            let mut fields = vec![FieldDiff::set("content", source_label(input.args))];
            if let Some(m) = arg_str_opt(input.args, "mode") {
                fields.push(FieldDiff::set("mode", m));
            }
            return Change::Create(fields);
        }

        let mut fields = Vec::new();
        match desired_sha {
            Some(want) => {
                if obs.get("sha256") != Some(want.as_str()) {
                    fields.push(FieldDiff::change(
                        "content",
                        short(obs.get("sha256").unwrap_or("?")),
                        short(&want),
                    ));
                }
            }
            // 来自物料:能拿到期望摘要就正经比对(内容寻址),拿不到才退回
            // "上游变没变"这个粗判据。
            None => match obs.get("want_sha256") {
                Some(want) => {
                    if obs.get("sha256") != Some(want) {
                        fields.push(FieldDiff::change(
                            "content",
                            short(obs.get("sha256").unwrap_or("?")),
                            short(want),
                        ));
                    }
                }
                None => {
                    if input.upstream_changed {
                        fields.push(FieldDiff::change(
                            "content",
                            "(上游已变)",
                            source_label(input.args),
                        ));
                    }
                }
            },
        }
        for key in ["owner", "group"] {
            if let (Some(want), Some(have)) = (arg_str_opt(input.args, key), obs.get(key)) {
                if want != have {
                    fields.push(FieldDiff::change(key, have, want));
                }
            }
        }
        if let (Some(want), Some(have)) = (arg_str_opt(input.args, "mode"), obs.get("mode")) {
            if want.trim_start_matches('0') != have.trim_start_matches('0') {
                fields.push(FieldDiff::change("mode", have, want));
            }
        }
        if fields.is_empty() {
            Change::Ok
        } else {
            Change::Update(fields)
        }
    }

    fn apply(&self, ctx: &dyn Ctx, args: &ResolvedArgs, _change: &Change) -> Result<Outcome> {
        let dest = arg_str(args, "dest")?;
        if let Some(content) = arg_str_opt(args, "content") {
            ctx.write_file(dest, content)?;
        } else if let Some(material) = arg_str_opt(args, "material") {
            ctx.place_material(material, dest)?;
        } else if let Some(src) = arg_str_opt(args, "src") {
            anyhow::bail!("`src: {src}` 需要控制端读文件,由执行层提供(本层不碰文件系统)");
        } else {
            anyhow::bail!("copy 需要 content / src / material 之一");
        }
        apply_ownership(ctx, dest, args)?;
        if let Some(mode) = arg_str_opt(args, "mode") {
            run_ok(ctx, &format!("chmod {} {}", sh(mode), sh(dest)))?;
        }
        Ok(Outcome::Changed)
    }

    fn destroy(&self, ctx: &dyn Ctx, args: &ResolvedArgs, obs: &Observed) -> Result<Outcome> {
        if !obs.present {
            return Ok(Outcome::Ok);
        }
        run_ok(ctx, &format!("rm -f {}", sh(arg_str(args, "dest")?)))?;
        Ok(Outcome::Changed)
    }
}

fn source_label(args: &ResolvedArgs) -> String {
    for key in ["material", "src"] {
        if let Some(v) = arg_str_opt(args, key) {
            return format!("{key}:{v}");
        }
    }
    arg_str_opt(args, "content")
        .map(|c| format!("{} 字节内联", c.len()))
        .unwrap_or_else(|| "(无来源)".into())
}

fn short(sha: &str) -> String {
    sha.chars().take(12).collect()
}

/// 纯 Rust sha256(不引依赖:本 crate 只做前端 + 契约,保持依赖面最小)。
pub fn sha256_hex(data: &str) -> String {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut msg = data.as_bytes().to_vec();
    let bit_len = (msg.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(v);
        }
    }
    h.iter().map(|w| format!("{w:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ctx::FakeCtx;
    use crate::eval::Yaml;

    fn args(pairs: &[(&str, &str)]) -> ResolvedArgs {
        pairs.iter().map(|(k, v)| (k.to_string(), Yaml::from(*v))).collect()
    }
    fn diff_of(a: &ResolvedArgs, o: &Observed, upstream: bool) -> Change {
        Copy.diff(&DiffInput { args: a, observed: o, upstream_changed: upstream })
    }

    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 跨块边界(> 55 字节)才会走第二个压缩块。
        assert_eq!(
            sha256_hex(&"a".repeat(64)),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn identical_content_is_idempotent() {
        let a = args(&[("dest", "/etc/x"), ("content", "hello")]);
        let obs = Observed::present([("sha256", sha256_hex("hello")), ("mode", "644".into())]);
        assert_eq!(diff_of(&a, &obs, false), Change::Ok);
    }

    #[test]
    fn changed_content_shows_a_short_digest_diff() {
        let a = args(&[("dest", "/etc/x"), ("content", "new")]);
        let obs = Observed::present([("sha256", sha256_hex("old"))]);
        let c = diff_of(&a, &obs, false);
        assert!(matches!(c, Change::Update(_)));
        let line = c.fields()[0].to_string();
        assert!(line.starts_with("content: "), "{line}");
        assert!(line.len() < 50, "摘要要短,不刷屏:{line}");
    }

    #[test]
    fn mode_alone_can_trigger_an_update() {
        let a = args(&[("dest", "/etc/x"), ("content", "hi"), ("mode", "0600")]);
        let obs = Observed::present([("sha256", sha256_hex("hi")), ("mode", "644".into())]);
        let c = diff_of(&a, &obs, false);
        assert_eq!(c.fields().len(), 1);
        assert_eq!(c.fields()[0].to_string(), "mode: 644 → 0600");
    }

    #[test]
    fn material_backed_copy_is_stable_until_upstream_moves() {
        // 物料内容在控制端,plan 期不重读;上游没变就不该谎报变更。
        let a = args(&[("dest", "/usr/local/bin/rustfs"), ("material", "rustfs-bin")]);
        let obs = Observed::present([("sha256", "deadbeef".into())]);
        assert_eq!(diff_of(&a, &obs, false), Change::Ok);
        assert!(matches!(diff_of(&a, &obs, true), Change::Update(_)));
    }

    #[test]
    fn observe_is_a_single_readonly_probe() {
        let ctx = FakeCtx::new().on("sha256sum", 0, "abc123\n644\n");
        let obs = Copy.observe(&ctx, &args(&[("dest", "/etc/x")])).unwrap();
        assert_eq!(obs.get("sha256"), Some("abc123"));
        assert_eq!(obs.get("mode"), Some("644"));
        assert!(ctx.writes().is_empty());
        assert_eq!(ctx.calls().len(), 1);
    }

    #[test]
    fn apply_writes_content_then_chmods() {
        let ctx = FakeCtx::new().on("chmod", 0, "");
        let a = args(&[("dest", "/etc/x"), ("content", "body"), ("mode", "0600")]);
        Copy.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        assert_eq!(ctx.written_file("/etc/x").as_deref(), Some("body"));
        assert!(ctx.calls().iter().any(|c| c.text().starts_with("chmod '0600'")));
    }

    #[test]
    fn apply_routes_material_to_the_closure_not_to_inline_write() {
        let ctx = FakeCtx::new();
        let a = args(&[("dest", "/usr/local/bin/x"), ("material", "bin")]);
        Copy.apply(&ctx, &a, &Change::Create(vec![])).unwrap();
        assert!(matches!(ctx.calls()[0], crate::ctx::Call::Place(_, _)));
    }

    #[test]
    fn destroying_something_already_gone_is_a_noop() {
        let ctx = FakeCtx::new();
        let a = args(&[("dest", "/etc/x")]);
        assert_eq!(Copy.destroy(&ctx, &a, &Observed::absent()).unwrap(), Outcome::Ok);
        assert!(ctx.calls().is_empty());
    }
}
