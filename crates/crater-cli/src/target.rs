//! Target selection & connection: the TargetOpts six-tuple (CLI 重构 1/3),
//! inventory/--host/local resolution (D-084), executor construction, and the
//! `crater create inventory` starter template.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::info;

use crater_core::executor::{Executor, LocalExecutor, SshExecutor};
use crater_core::spec::CraterSpec;

/// The connection / target-selection six-tuple shared by every fleet command
/// (`apply`/`delete`/`task list`/`task show`). `#[command(flatten)]` keeps each
/// subcommand's surface identical while defining the flags + resolver once.
#[derive(clap::Args, Clone, Debug)]
pub(crate) struct TargetOpts {
    /// Inventory file (its `inventory:` hosts) — large-fleet form, per-host
    /// creds. A spec source carries its own inventory.
    #[arg(short = 'i', long)]
    pub(crate) inventory: Option<PathBuf>,
    /// Target host(s), comma-separated for a small fleet sharing one credential:
    /// `--host 10.0.0.5,10.0.0.6`. Omit (and no `-i`) → local install.
    #[arg(long)]
    pub(crate) host: Option<String>,
    #[arg(long, default_value = "root")]
    pub(crate) user: String,
    #[arg(long)]
    pub(crate) password: Option<String>,
    /// SSH private-key file (alternative to --password), shared by all --host.
    #[arg(long)]
    pub(crate) key: Option<PathBuf>,
    #[arg(long, default_value_t = 22)]
    pub(crate) port: u16,
    /// Offline closure (`crater build -f <blueprint> -o closure.tar`): materials
    /// are pushed from these pre-fetched bytes instead of being downloaded by
    /// the target. Required for air-gapped fleets. Blueprint pipeline only.
    #[arg(long, value_name = "FILE")]
    pub(crate) closure: Option<PathBuf>,
    /// Fleet-wide concurrency: at most N hosts move at once within one step.
    /// Default 1 (serial). A step's own `throttle` can only cap *below* this.
    #[arg(long, default_value_t = 1, value_name = "N")]
    pub(crate) parallel: usize,
    /// 只对 inventory 里的一部分执行:主机名 / 组名,逗号分隔
    /// (`--limit n3` / `--limit lb,db`)。机群契约仍按整份 inventory 成立。
    #[arg(long, value_name = "NAMES")]
    pub(crate) limit: Option<String>,
    /// 执行顺序。
    ///
    /// `host`(默认):一台机器跑完全部资源,再下一台。排障时最顺手 ——
    /// 一台机器的来龙去脉连在一起。
    ///
    /// `linear`:一个资源在全机群跑完,再下一个(ansible 的默认策略)。
    /// 滚动升级时要的就是它:每一步做完立刻知道**所有机器**成没成,
    /// 而不是等最后一台跑完才发现第三步在第二台上就炸了。
    #[arg(long, value_enum, default_value_t = Strategy::Host)]
    pub(crate) strategy: Strategy,
    /// 分批滚动:每批多少台(`--serial 1` / `--serial 25%`)。
    ///
    /// 一批**整批做完**再下一批;**任一批失败就停**,后面的机器根本不碰 ——
    /// 这才是滚动升级的意义:出事时还剩大半个机群是好的。
    /// 不给则一次过全部(现有行为)。
    #[arg(long, value_name = "N|N%")]
    pub(crate) serial: Option<String>,
}

/// 执行顺序策略。
#[derive(Clone, Copy, PartialEq, Eq, Debug, clap::ValueEnum)]
pub(crate) enum Strategy {
    /// 逐台:一台跑完全部资源再下一台。
    Host,
    /// 逐资源:一个资源在全机群跑完再下一个(ansible 的 linear)。
    Linear,
}

/// 把执行目标切成批次。`spec` 是 `N` 或 `N%`;空/无效 → 一批到底。
///
/// 百分比向上取整且至少 1 台 —— `--serial 10%` 在 5 台上得到 1,
/// 而不是 0(那会让整条命令什么都不做,还看不出是为什么)。
pub(crate) fn batches(
    hosts: &[crater_core::spec::Host],
    spec: Option<&str>,
) -> Result<Vec<Vec<crater_core::spec::Host>>> {
    let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(vec![hosts.to_vec()]);
    };
    let size = if let Some(pct) = spec.strip_suffix('%') {
        let pct: usize = pct
            .trim()
            .parse()
            .map_err(|_| anyhow::anyhow!("--serial 百分比要写成整数,如 25%:{spec}"))?;
        if pct == 0 || pct > 100 {
            anyhow::bail!("--serial 百分比要在 1..=100:{spec}");
        }
        ((hosts.len() * pct) as f64 / 100.0).ceil() as usize
    } else {
        spec.parse::<usize>()
            .map_err(|_| anyhow::anyhow!("--serial 要写成台数或百分比:{spec}"))?
    };
    let size = size.max(1);
    Ok(hosts.chunks(size).map(|c| c.to_vec()).collect())
}

impl TargetOpts {
    /// True iff the user explicitly named targets (`-i` / `--host`) — for
    /// commands like `gc` where the localhost fallback would be wrong.
    pub(crate) fn has_explicit_targets(&self) -> bool {
        self.inventory.is_some() || self.host.is_some()
    }

    /// 这次真正要动的目标(`--limit` 过滤后)。求计划、连接、记账都走它。
    pub(crate) fn exec_hosts(&self) -> Result<Vec<crater_core::spec::Host>> {
        apply_limit(&self.hosts()?, self.limit.as_deref())
    }

    /// Resolve to a concrete host list: inventory > `--host` > localhost.
    /// inventory **显式声明**的组名(含空组)。
    ///
    /// 空组是合法拓扑(单节点的 `worker: { hosts: [] }`),必须与"拼错的组名"
    /// 分得开 —— 后者要报错,前者只是选不中任何人。
    pub(crate) fn declared_groups(&self) -> Vec<String> {
        let Some(path) = self.inventory.as_deref() else {
            return Vec::new();
        };
        crater_core::spec::CraterSpec::from_yaml_file(path)
            .map(|spec| spec.inventory.groups.keys().cloned().collect())
            .unwrap_or_default()
    }

    pub(crate) fn hosts(&self) -> Result<Vec<crater_core::spec::Host>> {
        target_hosts(
            self.inventory.as_deref(),
            self.host.clone(),
            &self.user,
            self.password.clone(),
            self.key.clone(),
            self.port,
        )
    }

    /// Like [`hosts`], but a task with no CLI target falls back to its embedded
    /// inventory before localhost (D-084).
    pub(crate) fn task_hosts(&self, task_path: &Path) -> Result<Vec<crater_core::spec::Host>> {
        task_hosts(
            task_path,
            self.inventory.as_deref(),
            self.host.clone(),
            &self.user,
            self.password.clone(),
            self.key.clone(),
            self.port,
        )
    }
}

/// Starter inventory written by `crater create inventory`. Comments document
/// every field; password/key are mutually exclusive (key wins, `~` expands).
pub(crate) const INVENTORY_TEMPLATE: &str = r#"# crater inventory —— 部署目标主机清单。
# 用法:crater apply <动作> -i <此文件>(大量机器、每台各自凭据)。
#
# 每台主机至少 name + address;认证用 password 或 key(二选一,key 优先)。
# user 默认 root,port 默认 22。
#
# 角色/成员由 groups 决定(仿 kubekey/Ansible):每个组列 hosts:(主机名)
# 和/或 groups:(嵌套子组),可嵌套。host 的角色 = 所属组(含嵌套向上传播),
# 不在 host 上重复写。task 的 when_role/hosts 按组名匹配。
#
# 三级 vars(全局 inventory.vars < 组 groups.<g>.vars < 主机 hosts[].vars),
# 覆盖 task 的 params 默认 —— 环境配置(vip/网段等)放这里,让 OCI 与环境无关。
inventory:
  # vars:                 # 全局(对所有主机生效)
  #   vip: "192.168.1.100"
  hosts:
    # ① 密码认证
    - name: web1
      address: 192.168.1.11
      user: root
      port: 22
      password: "changeme"

    # ② SSH 私钥认证(适合禁用密码登录的机群;~ 会自动展开为 $HOME)
    - name: web2
      address: 192.168.1.12
      user: ubuntu
      key: ~/.ssh/id_rsa

    # ③ 再一台
    - name: db1
      address: 192.168.1.20
      password: "changeme"

  groups:
    # 组成员 = 主机名;run_once 步骤取组内首台(如 k8s init 节点)。
    web:
      hosts: [web1, web2]
      # vars:               # 组级 vars(覆盖全局,被主机 vars 覆盖)
      #   listen_port: "8080"
    db:
      hosts: [db1]
    # 嵌套:组也能包含其他组,角色向上传播(web1 同时拥有 app 角色)。
    app:
      groups: [web, db]
"#;

/// `crater create inventory [path]`: write a sample inventory for the user to
/// edit. Refuses to clobber an existing file unless `--force`.
pub(crate) fn create_inventory(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        anyhow::bail!("{} 已存在(加 --force 覆盖)", path.display());
    }
    std::fs::write(path, INVENTORY_TEMPLATE)?;
    info!(
        "已生成 {} —— 编辑主机后用:crater apply <动作> -i {}",
        path.display(),
        path.display()
    );
    Ok(())
}

/// Expand a leading `~` to `$HOME` (std::fs / russh don't do shell expansion),
/// so an inventory `key: ~/.ssh/id_rsa` or `--key ~/...` works.
pub(crate) fn expand_tilde(p: &Path) -> PathBuf {
    if let Ok(rest) = p.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    p.to_path_buf()
}

/// Build an executor for a host: local (dry-run or `@local`), SSH key, or SSH
/// password. Shared by component (`run_host`) and task (`run_task_on_host`).
pub(crate) async fn connect_executor(
    host: &crater_core::spec::Host,
    do_apply: bool,
) -> Result<Box<dyn Executor>> {
    if !do_apply || host.is_local() {
        return Ok(Box::new(LocalExecutor));
    }
    if let Some(keypath) = &host.key {
        return Ok(Box::new(
            SshExecutor::connect_auth(
                &host.address,
                host.port,
                &host.user,
                &crater_core::executor::SshAuth::Key {
                    path: expand_tilde(keypath),
                    passphrase: None,
                },
            )
            .await?,
        ));
    }
    if let Some(pw) = host.password.as_deref().filter(|s| !s.is_empty()) {
        return Ok(Box::new(
            SshExecutor::connect(&host.address, host.port, &host.user, pw).await?,
        ));
    }
    anyhow::bail!("host {} needs --password or --key", host.name)
}

/// Resolve targets for a TASK file (D-084): an explicit `-i`/`--host` wins; else
/// the task's own embedded `inventory:` (self-contained single file); else local.
pub(crate) fn task_hosts(
    task_path: &Path,
    inv: Option<&Path>,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    key: Option<PathBuf>,
    port: u16,
) -> Result<Vec<crater_core::spec::Host>> {
    if inv.is_some() || host.is_some() {
        return target_hosts(inv, host, user, password, key, port);
    }
    // No CLI target → use the task's embedded inventory if it has hosts, else local.
    if let Some(mut emb) = crater_core::task::TaskFile::from_yaml_file(task_path)?.inventory {
        if !emb.hosts.is_empty() {
            emb.resolve(); // derive roles + merge the three var levels (D-077/082)
            info!("  目标取自任务内嵌 inventory({} 台)", emb.hosts.len());
            return Ok(emb.hosts);
        }
    }
    target_hosts(None, None, user, password, key, port) // → localhost
}

/// `--limit`:在 inventory 里挑一部分**执行目标**(主机名或组名)。
///
/// 语义与 ansible 的 `--limit` 一致,而且必须一致:机群契约、cast/exports
/// 仍按**整份 inventory** 成立 —— 限定的是"这次动谁",不是"机群变小了"。
/// 否则想对一台机器重跑一次 apply,就会被"控制面至少 3 台"的契约拦下来。
pub(crate) fn apply_limit(
    all: &[crater_core::spec::Host],
    limit: Option<&str>,
) -> Result<Vec<crater_core::spec::Host>> {
    let Some(spec) = limit.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(all.to_vec());
    };
    let wanted: Vec<&str> = spec
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    // 组名靠 host.roles 匹配 —— roles 由组成员关系派生(D-077),已经是传递闭包。
    let picked: Vec<crater_core::spec::Host> = all
        .iter()
        .filter(|h| {
            wanted
                .iter()
                .any(|w| h.name == *w || h.roles.iter().any(|r| r == w))
        })
        .cloned()
        .collect();
    if picked.is_empty() {
        // 报错要**把可选项列出来**:打错一个组名与"这组本来就空"是两回事,
        // 而只说"没选中"会让人反复猜。
        let names: Vec<&str> = all.iter().map(|h| h.name.as_str()).collect();
        let mut groups: Vec<&str> = all
            .iter()
            .flat_map(|h| h.roles.iter().map(|r| r.as_str()))
            .collect();
        groups.sort_unstable();
        groups.dedup();
        anyhow::bail!(
            "--limit `{spec}` 没选中任何主机\n  可选主机:{}\n  可选组:{}",
            names.join(", "),
            if groups.is_empty() {
                "(inventory 没有分组)".into()
            } else {
                groups.join(", ")
            }
        );
    }
    Ok(picked)
}

/// 把 `${env:VAR}` 与 `password_file:` 解成真值。
///
/// 只认 `${env:NAME}` 这一种形式,不做通用插值 —— 凭据字段上支持任意表达式
/// 只会让"这个口令到底从哪来"变成一道谜题,而那正是出事时最不想面对的谜题。
fn resolve_secrets(hosts: &mut [crater_core::spec::Host]) -> Result<()> {
    for h in hosts.iter_mut() {
        if let Some(pw) = h.password.take() {
            h.password = Some(expand_env(&pw).with_context(|| format!("主机 {}", h.name))?);
        }
        if let Some(pf) = h.password_file.take() {
            let path = expand_tilde(&pf);
            let raw = std::fs::read_to_string(&path)
                .with_context(|| format!("主机 {}:读口令文件 {}", h.name, path.display()))?;
            if h.password.is_some() {
                anyhow::bail!(
                    "主机 {}:password 与 password_file 同时给了 —— 哪个生效不该靠猜",
                    h.name
                );
            }
            // 末尾换行是 `echo secret > f` 的必然产物,不该被当成口令的一部分。
            h.password = Some(raw.trim_end_matches(['\n', '\r']).to_string());
        }
        if let Some(k) = h.key.take() {
            let expanded =
                expand_env(&k.to_string_lossy()).with_context(|| format!("主机 {}", h.name))?;
            h.key = Some(PathBuf::from(expanded));
        }
    }
    Ok(())
}

/// `${env:NAME}` → 环境变量。变量缺失是**硬错误**:悄悄替成空串,
/// 表现出来是"认证失败",而真因(忘了 export)要查很久。
fn expand_env(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("${env:") {
        out.push_str(&rest[..i]);
        let after = &rest[i + 6..];
        let Some(j) = after.find('}') else {
            anyhow::bail!("`${{env:` 没有闭合的 `}}`:{s}");
        };
        let name = &after[..j];
        let v = std::env::var(name).map_err(|_| {
            anyhow::anyhow!("环境变量 `{name}` 没设(inventory 里引用了 ${{env:{name}}})")
        })?;
        out.push_str(&v);
        rest = &after[j + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// 字面口令的提醒。**只提醒一次、只在有字面口令时** —— 每次 apply 都刷一屏
/// 警告,结果只会是所有人都学会无视它。
fn warn_literal_passwords(hosts: &[crater_core::spec::Host], path: &Path) {
    let n = hosts
        .iter()
        .filter(|h| h.password.is_some() && h.password_file.is_none() && !h.is_local())
        .count();
    if n == 0 {
        return;
    }
    // 已经在 git 里的才提醒:不在版本库的 inventory 里写明文,是本地的事。
    let in_git = std::process::Command::new("git")
        .args(["ls-files", "--error-unmatch"])
        .arg(path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !in_git {
        return;
    }
    // 同一条命令里 hosts() 会被调用不止一次(hosts / exec_hosts),
    // 不去重就把同一句刷两遍 —— 重复的告警是最快被无视的那一种。
    static WARNED: std::sync::OnceLock<std::sync::Mutex<std::collections::BTreeSet<PathBuf>>> =
        std::sync::OnceLock::new();
    if let Ok(mut set) = WARNED.get_or_init(Default::default).lock() {
        if !set.insert(path.to_path_buf()) {
            return;
        }
    }
    eprintln!(
        "提醒:{} 已被 git 跟踪,其中 {n} 台写着字面口令 —— git 历史删不掉。",
        path.display()
    );
    eprintln!("      改用 `key:`、`password_file:` 或 `password: \"${{env:VAR}}\"`。");
}

pub(crate) fn target_hosts(
    inv: Option<&Path>,
    host: Option<String>,
    user: &str,
    password: Option<String>,
    key: Option<PathBuf>,
    port: u16,
) -> Result<Vec<crater_core::spec::Host>> {
    if let Some(p) = inv {
        let spec = CraterSpec::from_yaml_file(p)?;
        let mut inv = spec.inventory;
        if inv.hosts.is_empty() {
            anyhow::bail!("inventory {} has no hosts", p.display());
        }
        // Derive roles from groups (D-077) + merge the three var levels into each
        // host (D-082: global ⊕ group ⊕ host).
        inv.resolve();
        // 凭据在**执行前**才解成真值。
        //
        // 刻意不在 from_yaml_file 里做:UI 的读取路径也走那条,而那条不该
        // 看见明文 —— 解析得越晚,明文在进程里活得越短、泄进日志/接口的
        // 机会越少。
        let mut hosts = inv.hosts;
        resolve_secrets(&mut hosts)?;
        warn_literal_passwords(&hosts, p);
        Ok(hosts)
    } else if let Some(h) = host {
        let hosts: Vec<_> = h
            .split(',')
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(|addr| crater_core::spec::Host {
                name: addr.to_string(),
                address: addr.to_string(),
                user: user.to_string(),
                port,
                password: password.clone(),
                password_file: None, // --host 是命令行直给,没有文件那一层
                key: key.clone(),
                roles: vec![],
                vars: BTreeMap::new(),
            })
            .collect();
        if hosts.is_empty() {
            anyhow::bail!("--host given but no addresses parsed");
        }
        Ok(hosts)
    } else {
        // No target → local install on the control machine.
        Ok(vec![crater_core::spec::Host::local()])
    }
}

#[cfg(test)]
mod batch_tests {
    use super::batches;
    use crater_core::spec::Host;

    fn hosts(n: usize) -> Vec<Host> {
        (0..n)
            .map(|i| Host {
                name: format!("n{i}"),
                ..Host::local()
            })
            .collect()
    }

    #[test]
    fn no_serial_means_one_batch() {
        assert_eq!(batches(&hosts(5), None).unwrap().len(), 1);
        assert_eq!(batches(&hosts(5), Some("")).unwrap().len(), 1);
    }

    #[test]
    fn count_splits_with_a_short_last_batch() {
        let b = batches(&hosts(5), Some("2")).unwrap();
        assert_eq!(b.iter().map(|c| c.len()).collect::<Vec<_>>(), vec![2, 2, 1]);
    }

    /// 百分比向上取整、且至少 1 台 —— 向下取整会得到 0,
    /// 那会让整条命令什么都不做,而且看不出是为什么。
    #[test]
    fn percentage_rounds_up_and_never_yields_an_empty_batch() {
        assert_eq!(batches(&hosts(5), Some("40%")).unwrap().len(), 3); // 每批 2
        let b = batches(&hosts(5), Some("10%")).unwrap();
        assert_eq!(b.len(), 5, "10% of 5 → 1 台一批,不是 0");
        assert!(b.iter().all(|c| !c.is_empty()));
    }

    #[test]
    fn bad_specs_are_refused_not_guessed() {
        assert!(batches(&hosts(3), Some("abc")).is_err());
        assert!(batches(&hosts(3), Some("0%")).is_err());
        assert!(batches(&hosts(3), Some("101%")).is_err());
    }

    #[test]
    fn a_batch_larger_than_the_fleet_is_just_one_batch() {
        assert_eq!(batches(&hosts(3), Some("99")).unwrap().len(), 1);
    }
}

#[cfg(test)]
mod secret_tests {
    use super::{expand_env, resolve_secrets};
    use crater_core::spec::Host;

    fn host() -> Host {
        Host {
            name: "n1".into(),
            address: "10.0.0.1".into(),
            ..Host::local()
        }
    }

    #[test]
    fn env_refs_expand_in_place() {
        std::env::set_var("CRATER_TEST_PW", "s3cret");
        assert_eq!(expand_env("${env:CRATER_TEST_PW}").unwrap(), "s3cret");
        // 前后缀保留:`${env:X}` 只是值的一部分也算数。
        assert_eq!(
            expand_env("pre-${env:CRATER_TEST_PW}-post").unwrap(),
            "pre-s3cret-post"
        );
        assert_eq!(expand_env("没有引用").unwrap(), "没有引用");
    }

    /// 变量缺失必须**硬失败**。悄悄替成空串,表现出来是"认证失败",
    /// 而真因(忘了 export)要查很久。
    #[test]
    fn a_missing_variable_is_an_error_not_an_empty_string() {
        let e = expand_env("${env:CRATER_TEST_DEFINITELY_UNSET}").unwrap_err();
        assert!(
            e.to_string().contains("CRATER_TEST_DEFINITELY_UNSET"),
            "{e}"
        );
    }

    #[test]
    fn an_unclosed_reference_is_refused() {
        assert!(expand_env("${env:OPEN").is_err());
    }

    #[test]
    fn password_file_is_read_and_trailing_newline_stripped() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("pw");
        // `echo secret > f` 的必然产物 —— 换行不该被当成口令的一部分。
        std::fs::write(&f, "s3cret\n").unwrap();
        let mut hs = vec![Host {
            password_file: Some(f),
            ..host()
        }];
        resolve_secrets(&mut hs).unwrap();
        assert_eq!(hs[0].password.as_deref(), Some("s3cret"));
    }

    /// 两个都给时不许猜 —— "哪个生效"靠猜,是最难复现的一类故障。
    #[test]
    fn password_and_password_file_together_are_refused() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("pw");
        std::fs::write(&f, "x").unwrap();
        let mut hs = vec![Host {
            password: Some("y".into()),
            password_file: Some(f),
            ..host()
        }];
        assert!(resolve_secrets(&mut hs).is_err());
    }
}
