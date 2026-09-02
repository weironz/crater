//! `crater facts` —— `substrate.*` 到底有哪些,以及在你的机器上是什么值。
//!
//! 补的是一个真实缺口:资源类型的字段可以用 `crater types <类型>` 查,
//! 而 `substrate.*` 的白名单**只存在于源码里** —— 作者要知道能写什么,
//! 只能去读 facts.rs 或者猜。同一个项目里一半可发现、一半不可发现,说不通。
//!
//! 探测模式(`-i inventory`)解决的是另一个问题:`when: substrate.family ==
//! 'debian'` 不成立时,人想知道的是"那这台到底是什么" —— 而不是重读一遍
//! 自己写的条件。

use anyhow::Result;

use crate::say;
use crate::target::TargetOpts;

/// 不连机器:把白名单印出来。
fn list() {
    say!("substrate.* —— 目标机事实(封闭白名单,plan 之前一次探全)");
    say!();
    let w = crater_ir::facts::catalog()
        .iter()
        .map(|f| f.name.len())
        .max()
        .unwrap_or(0);
    for f in crater_ir::facts::catalog() {
        say!("  substrate.{:<w$}  {}", f.name, f.doc, w = w);
    }
    say!();
    say!("另有两项来自 inventory(不是探出来的):");
    say!("  substrate.name        inventory 里的机器名 —— 部署记录按它归档");
    say!("  substrate.roles       该机器所属的组(含嵌套传播)");
    say!();
    say!("蓝图还可以用 `facts:` 声明**派生事实** —— 声明处做计算,值位置保持名词:");
    say!("  facts:");
    say!("    vip_iface: \"iface_in(params.vip_cidr)\"   # 持有该网段地址的网卡");
    say!("  然后在资源里写 ${{facts.vip_iface}}。");
    say!();
    say!("可在 `when:` / `facts:` / `preflight:` 里调用的探针(封闭集合):");
    say!("  port_owner(端口)      谁在监听 —— 空串 = 没人");
    say!("  path_exists(路径)     路径在不在");
    say!("  cmd_ok(命令)          只读命令退出码为 0");
    say!("  service_state(名字)   systemd 单元状态");
    say!("  iface_in(网段)        持有该网段地址的网卡名 —— 匹配不到是空串");
    say!();
    say!("用法:`when: \"substrate.family == 'debian'\"`,或物料 URL 里 ${{substrate.arch}}。");
    say!("白名单之外不探 —— `substrate.` 不是一个能夹带任意命令的口子。");
    say!("构建期没有目标机可探,所以 `crater build` 要用 `--for arch=amd64` 补上。");
}

/// 连机器:逐台探全,横着摆成一张表。
///
/// 表的形状是 事实 × 主机,因为要回答的问题几乎总是"**哪台不一样**" ——
/// 竖着一台一台读,得自己在脑子里做 diff。
async fn probe(target: &TargetOpts) -> Result<()> {
    let hosts = target.exec_hosts()?;
    let mut cols: Vec<String> = Vec::new();
    let mut rows: Vec<(String, Vec<String>)> = crater_ir::facts::catalog()
        .iter()
        .map(|f| (f.name.to_string(), Vec::new()))
        .collect();

    for host in &hosts {
        cols.push(host.name.clone());
        let ctx = crate::blueprint::probe_ctx(host).await?;
        let got = crater_ir::facts::Facts::new(ctx.as_ref()).gather_all()?;
        for (name, vals) in rows.iter_mut() {
            // 探不到记空串(容器里没有 /etc/os-release 是常态)—— 表里显示成
            // `—`。与"探到了但值为空"在显示上不区分:对使用者是同一件事,
            // `when` 都不会成立。
            let v = got
                .get(name)
                .and_then(|y| y.as_str().map(str::to_string))
                .unwrap_or_default();
            vals.push(if v.is_empty() { "—".into() } else { v });
        }
    }

    let namew = rows
        .iter()
        .map(|(n, _)| n.chars().count())
        .max()
        .unwrap_or(0);
    let colw: Vec<usize> = cols
        .iter()
        .enumerate()
        .map(|(i, c)| {
            rows.iter()
                .map(|(_, v)| v[i].chars().count())
                .chain(std::iter::once(c.chars().count()))
                .max()
                .unwrap_or(0)
        })
        .collect();

    let head: String = cols
        .iter()
        .zip(&colw)
        .map(|(c, w)| format!("{c:<w$}  ", w = *w))
        .collect();
    say!("  {:<namew$}  {}", "", head.trim_end(), namew = namew);
    for (name, vals) in &rows {
        let line: String = vals
            .iter()
            .zip(&colw)
            .map(|(v, w)| format!("{v:<w$}  ", w = *w))
            .collect();
        say!("  {name:<namew$}  {}", line.trim_end(), namew = namew);
    }
    say!();
    // 值不一致的行才是这张表存在的理由 —— 直接点出来,不让人逐行比对。
    let differing: Vec<&str> = rows
        .iter()
        .filter(|(_, v)| v.iter().collect::<std::collections::BTreeSet<_>>().len() > 1)
        .map(|(n, _)| n.as_str())
        .collect();
    if cols.len() > 1 {
        if differing.is_empty() {
            say!("{} 台机器的事实完全一致。", cols.len());
        } else {
            say!("各机器不一致的事实:{}", differing.join(", "));
        }
    }
    Ok(())
}

pub async fn run(target: &TargetOpts) -> Result<()> {
    if target.has_explicit_targets() {
        probe(target).await
    } else {
        list();
        Ok(())
    }
}
