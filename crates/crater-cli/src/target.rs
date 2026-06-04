//! Target selection & connection: the TargetOpts six-tuple (CLI 重构 1/3),
//! inventory/--host/local resolution (D-084), executor construction, and the
//! `crater create inventory` starter template.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
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
}

impl TargetOpts {
    /// Resolve to a concrete host list: inventory > `--host` > localhost.
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
        Ok(inv.hosts)
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
