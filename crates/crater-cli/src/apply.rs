//! The apply/delete pipeline (D-020 单管线): source routing (task / project /
//! OCI bundle / image ref / named), task orchestration across hosts —
//! grouped by role-set, serial_roles, parallel + HostCoord (D-030/071/077) —
//! and per-host plan + execute (agent or control-plane shell).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use futures::StreamExt;
use tracing::{info, warn};

use crater_core::arch;
use crater_core::bundle;
use crater_core::engine::{self, Op, PlanContext};
use crater_core::os::{self, OsFamily};
use crater_core::state::{self, Marker};

use crate::target::{connect_executor, TargetOpts};
use crate::{agent, deployments, images};


/// `crater apply <source>` — one entry point for online & offline (D-020).
/// Auto-detect the source kind and route; the execution engine (idempotency,
/// tracing, agent/shell) is shared — online vs offline differ only in where
/// artifacts come from.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_source(
    name: Option<String>,
    source: Option<String>,
    file: Option<PathBuf>,
    target: TargetOpts,
    dry_run: bool,
    shell: bool,
    teardown: bool,
    offline: bool,
) -> Result<()> {
    let verb = if teardown { "delete" } else { "apply" };
    // `<source>` positional, else `-f`.
    let src = source
        .or_else(|| file.map(|p| p.display().to_string()))
        .ok_or_else(|| anyhow!("{verb} needs a <source>: a task.yaml, an x.oci bundle, an image ref, or a named task"))?;
    let path = PathBuf::from(&src);
    if let Some(n) = &name {
        info!("{verb}: deployment '{n}' ← {src}");
    }

    if path.is_file() && bundle::is_oci_archive(&path) {
        // Offline: OCI bundle. Targets from CLI (D-020: never inside the image)
        // — `-i inventory.yaml`, `--host a,b`, or none → local.
        info!("{verb}: {src} → offline (OCI bundle)");
        let hosts = target.hosts()?;
        return apply_oci_bundle(&path, hosts, !dry_run, shell, teardown, &src, name.as_deref()).await;
    }
    if path.is_file() && crater_core::project::is_project_file(&path) {
        // A project (top-level `plays:`, D-083): orchestrate plays in order.
        info!("{verb}: {src} → project");
        return apply_project(&path, name.as_deref(), &target, dry_run, shell, teardown).await;
    }
    if path.is_file() {
        // A task file (top-level `actions:`, D-037). Component specs are gone.
        if crater_core::task::is_task_file(&path) {
            info!("{verb}: {src} → task");
            let hosts = target.task_hosts(&path)?;
            let opts = RunOpts {
                offline_blobmap: None,
                offline: false,
                do_apply: !dry_run,
                do_shell: shell,
                teardown,
                source: src.clone(),
            };
            return apply_task(&path, hosts, opts, name.as_deref(), None, BTreeMap::new()).await;
        }
        anyhow::bail!(
            "{src}: not a task file (needs top-level `actions:`). Component specs are no \
             longer supported — write a task."
        );
    }
    // Image reference (registry/store): has a registry path or a tag, not a file.
    if src.contains('/') || src.contains(':') {
        info!("{verb}: {src} → image (local store / registry)");
        let hosts = target.hosts()?;
        return images::apply_image_ref(&src, hosts, !dry_run, teardown, &src, name.as_deref(), offline).await;
    }
    // Named task/project: `crater apply <name>` → first match of <name>.yaml under
    // library/ (then tasks/ for back-compat). D-043/D-085.
    if let Some(named) = find_named(&src) {
        if crater_core::project::is_project_file(&named) {
            info!("{verb}: {src} → named project ({})", named.display());
            return apply_project(&named, name.as_deref(), &target, dry_run, shell, teardown).await;
        }
        if crater_core::task::is_task_file(&named) {
            info!("{verb}: {src} → named task ({})", named.display());
            let hosts = target.task_hosts(&named)?;
            let opts = RunOpts {
                offline_blobmap: None,
                offline: false,
                do_apply: !dry_run,
                do_shell: shell,
                teardown,
                source: src.clone(),
            };
            return apply_task(&named, hosts, opts, name.as_deref(), None, BTreeMap::new()).await;
        }
    }
    anyhow::bail!(
        "'{src}': not a file, image ref, or named task/project. Put it under library/<name>.yaml, \
         or pass a path / -f <file> / an image reference."
    )
}

/// `crater apply <project>.yaml` (D-083): run a project's plays in order (delete
/// runs them in REVERSE). Each play resolves its `source` to a task (path or
/// `tasks/<source>.yaml`) and applies it with the play's `hosts`/`vars` overrides.
/// A barrier between plays (each `apply_task` completes first), so ordering like
/// host-init → k8s → cni holds. All plays share one deployment label (the project
/// name, or `--name`), so `task list` groups them.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_project(
    path: &Path,
    name: Option<&str>,
    target: &TargetOpts,
    dry_run: bool,
    shell: bool,
    teardown: bool,
) -> Result<()> {
    use crater_core::project::Project;
    let project = Project::from_yaml_file(path)?;
    let verb = if teardown { "delete" } else { "apply" };
    if project.plays.is_empty() {
        anyhow::bail!("project '{}' 没有 plays", project.name);
    }
    let mut order: Vec<&crater_core::project::Play> = project.plays.iter().collect();
    if teardown {
        order.reverse(); // tear down in reverse: e.g. k8s before host baseline.
    }
    let deployment = name.map(|s| s.to_string()).unwrap_or_else(|| project.name.clone());
    info!(
        "{verb} project '{}': {} play(s){}",
        project.name,
        order.len(),
        if teardown { "(逆序)" } else { "" }
    );
    let total = order.len();
    for (i, play) in order.iter().enumerate() {
        let label = play.name.clone().unwrap_or_else(|| play.source.clone());
        // Resolve source: an explicit path, else a named task/project under library/.
        let src_path = find_named(&play.source).ok_or_else(|| {
            anyhow!(
                "project '{}' play '{label}':source '{}' 未找到(路径或 library/**/{}.yaml)",
                project.name, play.source, play.source
            )
        })?;
        info!(
            "── play {}/{total}: {label}(source={}, hosts={})",
            i + 1,
            play.source,
            play.hosts.as_deref().unwrap_or("<task 默认>")
        );
        let hosts = target.hosts()?;
        // Skip a play whose target group has no hosts (e.g. HA with no separate
        // workers) — don't abort the whole project (D-083).
        if let Some(g) = &play.hosts {
            let matches = g == "all"
                || hosts.iter().any(|h| h.roles.is_empty() || h.name == *g || h.roles.iter().any(|r| r == g));
            if !matches {
                info!("   (跳过:hosts='{g}' 无匹配主机)");
                continue;
            }
        }
        let opts = RunOpts {
            offline_blobmap: None,
            offline: false,
            do_apply: !dry_run,
            do_shell: shell,
            teardown,
            source: play.source.clone(),
        };
        apply_task(
            &src_path, hosts, opts, Some(&deployment),
            play.hosts.clone(), play.vars.clone(),
        )
        .await
        .map_err(|e| anyhow!("project '{}' play '{label}' 失败:{e}", project.name))?;
    }
    info!("{verb} project '{}' 完成", project.name);
    Ok(())
}

/// How to run a task — the mode flags every apply/delete entry point chooses
/// (CLI 重构 2/3). Named fields instead of a positional bool-soup at call sites.
pub(crate) struct RunOpts {
    /// Packed material blobs (recipe-replay, D-045); None = pure online.
    pub(crate) offline_blobmap: Option<BTreeMap<String, PathBuf>>,
    // Strict-offline (air-gap): missing blob = error, not online fetch (D-087).
    // A thin-online ref carries a partial blobmap with this `false`.
    pub(crate) offline: bool,
    pub(crate) do_apply: bool,
    pub(crate) do_shell: bool,
    pub(crate) teardown: bool,
    pub(crate) source: String,
}

/// Shared read-only context for ONE task run, fixed once `apply_task` has
/// parsed/expanded the task and grouped the targets. Per-host calls only add
/// what genuinely varies: the host, the between-groups `hostvars` snapshot,
/// and the per-group coordinator.
pub(crate) struct RunContext {
    task: crater_core::task::TaskFile,
    spec_dir: PathBuf,
    /// role → space-joined member addresses, for `{{ groups.<role> }}` (D-071).
    role_addrs: BTreeMap<String, String>,
    /// role → (name, addr) members, for the template layer's `groups` (D-075).
    role_members: BTreeMap<String, Vec<(String, String)>>,
    /// Ordered (name, roles) of all targets, for `run_once` gating (D-077).
    target_hosts: Vec<(String, Vec<String>)>,
    /// Grouping label for `task list` (D-052), default = task name.
    deployment: String,
    opts: RunOpts,
}

/// `crater apply <task>.yaml` (D-037): run a generic task across the targets.
/// Control flow (when-filter, needs-ordering) is in the engine. Host
/// orchestration mirrors the component pipeline (D-030/D-031): hosts grouped by
/// role-set run group-by-group (so a producer's `register` lands in `hostvars`
/// before a consumer group reads it), parallel within a group.
pub(crate) async fn apply_task(
    path: &Path,
    hosts: Vec<crater_core::spec::Host>,
    opts: RunOpts,
    name: Option<&str>,
    // Project-play overrides (D-083): retarget the task's group / overlay vars.
    hosts_override: Option<String>,
    var_overrides: BTreeMap<String, String>,
) -> Result<()> {
    use crater_core::task::TaskFile;
    let mut task = TaskFile::from_yaml_file(path)?;
    if let Some(h) = &hosts_override {
        task.hosts = h.clone();
    }
    for (k, v) in &var_overrides {
        task.vars.insert(k.clone(), v.clone());
    }
    // Flatten role bundles (D-080): online from a task file → roles read from
    // ./roles; offline from an OCI → recipe is already flat (expanded at build),
    // so this is a no-op (no `action: role` bundles remain).
    task.expand_roles(&roles_dir_for(path.parent().unwrap_or_else(|| Path::new("."))))?;
    // Param-contract validation happens per-host in run_task_on_host (against the
    // merged task ⊕ inventory vars, D-082) — so inventory-supplied required params
    // count and errors are reported before that host plans.
    // Optional deployment/grouping label (D-052), default = task name. Only used
    // for `task list` grouping; apply/delete behavior is identical regardless.
    let deployment = name.unwrap_or(&task.name).to_string();
    // Delete is opt-in (D-049): a task only has delete capability if it authored
    // a `teardown:`. No auto-inversion of `actions:` — real cleanup targets
    // runtime state the install steps never created.
    if opts.teardown && task.teardown.is_empty() {
        anyhow::bail!(
            "task '{}' defines no `teardown:` — it has no delete capability \
             (delete is opt-in; author a teardown to enable it)",
            task.name
        );
    }
    let spec_dir = path.parent().unwrap_or_else(|| Path::new(".")).to_path_buf();
    // `hosts` filter (D-037-b/D-043/D-077/D-084): `all` → every target; else keep
    // hosts matching that **group name** (a role, derived transitively from
    // inventory groups) OR that **host name** (ansible-style). Hosts with no roles
    // (CLI --host / local) always match.
    let hosts: Vec<crater_core::spec::Host> = if task.hosts == "all" {
        hosts
    } else {
        hosts
            .into_iter()
            .filter(|h| {
                h.roles.is_empty()
                    || h.name == task.hosts
                    || h.roles.iter().any(|r| r == &task.hosts)
            })
            .collect()
    };
    if hosts.is_empty() {
        anyhow::bail!("task hosts='{}' matched no target host", task.hosts);
    }
    info!(
        "{} '{}': {} action(s), hosts={}, {} target(s), mode={}",
        if opts.teardown { "teardown" } else { "task" },
        task.name,
        if opts.teardown { task.teardown.len() } else { task.actions.len() },
        task.hosts,
        hosts.len(),
        if opts.do_apply { "apply" } else { "dry-run" }
    );

    let mut hostvars: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();

    // Role → member hosts. `role_members` (name+addr) feeds the template layer's
    // structured `groups.<role>` (D-075); `role_addrs` is the space-joined-address
    // form for the simple `{{ groups.<role> }}` substitution in cmds (D-071).
    let role_members: BTreeMap<String, Vec<(String, String)>> = {
        let mut acc: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
        for h in &hosts {
            for r in &h.roles {
                acc.entry(r.clone()).or_default().push((h.name.clone(), h.address.clone()));
            }
        }
        acc
    };
    let role_addrs: BTreeMap<String, String> = role_members
        .iter()
        .map(|(k, v)| (k.clone(), v.iter().map(|(_, ip)| ip.clone()).collect::<Vec<_>>().join(" ")))
        .collect();
    // host name → roles, so a host's registered facts can also be published under
    // its roles (`hostvars.<role>.<name>`, D-071) for singleton roles like the init node.
    let name_roles: BTreeMap<String, Vec<String>> =
        hosts.iter().map(|h| (h.name.clone(), h.roles.clone())).collect();
    // Ordered (name, roles) of all targets (D-077), for `run_once` gating: a
    // run_once step runs only on the first target matching its when_role.
    let target_hosts: Vec<(String, Vec<String>)> =
        hosts.iter().map(|h| (h.name.clone(), h.roles.clone())).collect();

    // From here on the run-wide context is fixed; only host / hostvars / coord
    // vary per call.
    let rc = RunContext { task, spec_dir, role_addrs, role_members, target_hosts, deployment, opts };

    if !rc.opts.do_apply {
        for h in &hosts {
            run_task_on_host(&rc, h, &hostvars, None).await?;
        }
        info!("dry-run only; omit --dry-run to execute");
        return Ok(());
    }

    let mut applied_hosts: Vec<String> = Vec::new();
    let forks = forks_limit();

    // Merge one host's registered facts into the shared hostvars, published both
    // as `hostvars.<host>.<name>` and `hostvars.<role>.<name>` (D-030/D-071).
    fn merge_regs(
        hostvars: &mut BTreeMap<String, BTreeMap<String, String>>,
        name_roles: &BTreeMap<String, Vec<String>>,
        host_name: &str,
        regs: Vec<(String, String)>,
    ) {
        let roles = name_roles.get(host_name).cloned().unwrap_or_default();
        for (k, v) in regs {
            hostvars.entry(host_name.to_string()).or_default().insert(k.clone(), v.clone());
            for r in &roles {
                hostvars.entry(r.clone()).or_default().insert(k.clone(), v.clone());
            }
        }
    }

    for group in group_hosts_by_role(&hosts) {
        // serial_roles (D-071): a group whose hosts hold a serial role runs one at
        // a time — e.g. control-plane joins must not race on etcd quorum.
        let serial = group
            .iter()
            .any(|h| h.roles.iter().any(|r| rc.task.serial_roles.contains(r)));
        if serial {
            // Sequential WITH progressive fact propagation (D-077): each host's
            // registered facts are merged into hostvars BEFORE the next host plans.
            // (Prefer per-step `throttle` over serial_roles — it parallelizes prep.)
            for h in group {
                let (host_name, regs) = run_task_on_host(&rc, h, &hostvars, None).await?;
                merge_regs(&mut hostvars, &name_roles, &host_name, regs);
                applied_hosts.push(host_name);
            }
        } else {
            // Independent hosts → run in PARALLEL, coordinated by a shared
            // HostCoord (D-077): a step awaiting a cross-host fact blocks until its
            // producer publishes (fail-fast if the producer errors), and a
            // `throttle`d step is capped to N-at-once. Seeded with prior-group facts.
            let mut seed: BTreeMap<String, String> = BTreeMap::new();
            for (scope, kv) in &hostvars {
                for (k, v) in kv {
                    seed.insert(format!("hostvars.{scope}.{k}"), v.clone());
                }
            }
            let coord = engine::HostCoord::new(seed, group.len());
            let (rc_ref, hostvars_ref, coord_ref) = (&rc, &hostvars, &coord);
            let results: Vec<Result<(String, Vec<(String, String)>)>> = futures::stream::iter(
                group.iter().map(|h| async move {
                    // Signal the coordinator on finish so peers awaiting this host's
                    // facts fail fast on error / never-produced rather than blocking
                    // to the timeout (D-077). Facts (if any) are published inside.
                    let r = run_task_on_host(rc_ref, h, hostvars_ref, Some(coord_ref)).await;
                    if r.is_err() {
                        coord_ref.mark_aborted();
                    }
                    coord_ref.host_done();
                    r
                }),
            )
            .buffer_unordered(forks)
            .collect()
            .await;
            let mut first_err = None;
            for r in results {
                match r {
                    Ok((host_name, regs)) => {
                        merge_regs(&mut hostvars, &name_roles, &host_name, regs);
                        applied_hosts.push(host_name);
                    }
                    Err(e) => {
                        if first_err.is_none() {
                            first_err = Some(e);
                        }
                    }
                }
            }
            if let Some(e) = first_err {
                return Err(e);
            }
        }
    }

    // Record to the control-side aggregate DB (D-051). Best-effort: the markers
    // on the targets are authoritative; the DB is a cache/history for list/UI.
    if let Err(e) = deployments::record_deployments(&rc.task, &rc.opts.source, &rc.deployment, rc.opts.teardown, &applied_hosts).await {
        warn!("state DB update failed (targets' markers are authoritative): {e}");
    }
    Ok(())
}


/// Plan + execute one task on one host. The run-wide fixed inputs live in
/// `rc` (CLI 重构 2/3); only the genuinely per-call ones remain parameters:
/// the host, the between-groups `hostvars` snapshot, the per-group coordinator.
pub(crate) async fn run_task_on_host(
    rc: &RunContext,
    host: &crater_core::spec::Host,
    hostvars: &BTreeMap<String, BTreeMap<String, String>>,
    coord: Option<&engine::HostCoord>,
) -> Result<(String, Vec<(String, String)>)> {
    let RunContext { task, spec_dir, role_addrs, role_members, target_hosts, deployment, opts } = rc;
    let RunOpts { offline_blobmap, offline, do_apply, do_shell, teardown, source } = opts;
    let (offline, do_apply, do_shell, teardown) = (*offline, *do_apply, *do_shell, *teardown);
    if host.is_local() {
        info!("▶ host {} (local)", host.name);
    } else {
        info!("▶ host {} ({})", host.name, host.address);
    }
    let exec = connect_executor(host, do_apply).await?;
    let (osf, target_arch) = if do_apply {
        (os::detect_via(exec.as_ref()).await, arch::detect_via(exec.as_ref()).await)
    } else {
        // Dry-run preview: no target connection — use the control machine's arch
        // so `place` can resolve a concrete variant for the plan.
        (OsFamily::Unknown, arch::detect_local())
    };
    let ver = task
        .effective_vars()
        .get("version")
        .cloned()
        .unwrap_or_else(|| "latest".into());
    let mut ctx = PlanContext::new(osf, ver, spec_dir.to_path_buf());
    ctx.target_arch = target_arch;
    ctx.roles_dir = roles_dir_for(spec_dir); // legacy thin roles, relative to delivery (D-086)
    for (k, v) in &task.effective_vars() {
        ctx.vars.insert(k.clone(), v.clone());
    }
    // Inventory vars (D-082) override task param defaults: host.vars is the merged
    // global ⊕ group ⊕ host set (resolved in target_hosts). This is how apply-stage
    // env config (vip/subnet/…) comes from inventory rather than baked in the OCI.
    for (k, v) in &host.vars {
        ctx.vars.insert(k.clone(), v.clone());
    }
    // Validate the param contract against the merged vars (D-081/082): every
    // required param must now have a value (default / inventory). Fail before planning.
    task.validate_params(&ctx.vars, None)?;
    // Other hosts' registered facts become template vars (D-030).
    for (h, kv) in hostvars {
        for (k, v) in kv {
            ctx.vars.insert(format!("hostvars.{h}.{k}"), v.clone());
        }
    }
    // Role membership (D-071): `{{ groups.<role> }}` = member addresses; the
    // target's own roles drive `when_role` step filtering.
    for (role, addrs) in role_addrs {
        ctx.vars.insert(format!("groups.{role}"), addrs.clone());
    }
    ctx.groups = role_members.clone(); // structured groups for the template layer (D-075)
    ctx.host_roles = host.roles.clone();
    // run_once gating (D-077): this host's identity + the ordered target list.
    ctx.self_host = host.name.clone();
    ctx.target_hosts = target_hosts.to_vec();
    // Facts THIS host will itself register (mirrors the register gating below):
    // a step must never await a fact its own host produces — the register runs
    // after the step, so awaiting it would deadlock (D-077). Keyed exactly as the
    // consumers reference them: hostvars.<host>.<name> and hostvars.<role>.<name>.
    for reg in &task.register {
        let role_match = |roles: &[String]| {
            reg.when_role.is_empty() || reg.when_role.iter().any(|r| roles.iter().any(|h| h == r))
        };
        if !role_match(&host.roles) {
            continue;
        }
        if reg.run_once {
            let leader = target_hosts.iter().find(|(_, roles)| role_match(roles));
            if leader.is_some_and(|(name, _)| name != &host.name) {
                continue;
            }
        }
        ctx.self_produced.insert(format!("hostvars.{}.{}", host.name, reg.name));
        for r in &host.roles {
            ctx.self_produced.insert(format!("hostvars.{r}.{}", reg.name));
        }
    }
    // The target's own inventory identity, for templates that need a stable
    // unique per-host value (e.g. kubeadm `--node-name`, D-071).
    ctx.vars.insert("inventory_hostname".to_string(), host.name.clone());
    ctx.vars.insert("inventory_addr".to_string(), host.address.clone());
    for m in &task.materials {
        ctx.add_material(m.clone());
    }
    // Offline (recipe-replay, D-045): `place` pushes packed blobs from control.
    // `offline` distinguishes strict air-gap (missing blob = error) from
    // thin-online (partial blobmap, missing material fetched online) — D-087.
    if let Some(bm) = offline_blobmap {
        ctx.offline_blobs = Some(bm.clone());
        ctx.offline = offline;
    }
    // delete → run the authored `teardown:` actions; apply → `actions`.
    let action_list = if teardown { &task.teardown } else { &task.actions };
    let steps = engine::plan_from_task(action_list, &ctx)?;
    let handlers = engine::plan_handlers(&task.handlers, &ctx)?;
    info!(
        "[{}] {} {} — {} step(s)",
        host.name,
        if teardown { "teardown" } else { "task" },
        task.name,
        steps.len()
    );
    if !do_apply {
        let ops: Vec<Op> = steps.iter().map(|s| s.op.clone()).collect();
        print_plan(&ops);
        return Ok((host.name.clone(), Vec::new()));
    }
    // Default: self-bootstrap agent runs the task plan on the target (D-044).
    // Offline (blobs on control), --shell, or local → control-plane execute_task.
    // A PushFile step reads a CONTROL-side path at execution time (e.g. `copy
    // material:` whose material is a local `src:` file) — the agent on the
    // target can't reach it, so such plans must also execute control-plane.
    let has_push_file = steps.iter().any(|s| matches!(s.op, Op::PushFile { .. }));
    if offline_blobmap.is_some() || do_shell || host.is_local() || has_push_file {
        engine::execute_task(&steps, &handlers, exec.as_ref(), coord).await?;
    } else {
        // Agent path runs the plan on the target without the control-side coord;
        // cross-host throttle/awaited-facts (D-077) only apply to control-plane
        // execute (the offline branch above, which HA always takes).
        agent::run_task_via_agent(exec.as_ref(), &steps, &handlers, None).await?;
    }

    // Record on-target deployment state (D-051): apply writes the marker,
    // delete removes it. Best-effort — the deployment already succeeded.
    let marker_res = if teardown {
        state::remove_marker(exec.as_ref(), &task.name).await
    } else {
        let m = Marker {
            name: task.name.clone(),
            deployment: deployment.to_string(),
            version: ctx.version.clone(),
            source: source.to_string(),
            applied_at: state::now_epoch(),
        };
        state::write_marker(exec.as_ref(), &m).await
    };
    if let Err(e) = marker_res {
        warn!("[{}] state marker update failed (deployment still applied): {e}", host.name);
    }

    // Capture this host's facts for later groups (D-030). Apply only — teardown
    // has no fact consumers, and the register cmds (e.g. `kubeadm token create`)
    // would run against tooling teardown just removed (D-077: it deleted kubeadm).
    let mut registered: Vec<(String, String)> = Vec::new();
    for reg in &task.register {
        if teardown {
            break;
        }
        // when_role (D-071): only gather this fact on hosts holding the role
        // (e.g. the join command is produced on the control-plane, not workers).
        let role_match = |roles: &[String]| {
            reg.when_role.is_empty() || reg.when_role.iter().any(|r| roles.iter().any(|h| h == r))
        };
        if !role_match(&host.roles) {
            continue;
        }
        // run_once (D-077): gather only on the first target matching when_role
        // (the implicit init node) — `kubeadm token create` / upload-certs once.
        if reg.run_once {
            let leader = target_hosts.iter().find(|(_, roles)| role_match(roles));
            if leader.is_some_and(|(name, _)| name != &host.name) {
                continue;
            }
        }
        let out = exec.run(&engine::render(&reg.cmd, &ctx.vars)?).await?;
        if !out.ok() {
            anyhow::bail!(
                "register '{}' on {} failed (exit {}): {}",
                reg.name,
                host.name,
                out.code,
                out.stderr.trim()
            );
        }
        let val = out.stdout.trim().to_string();
        info!("[{}] registered {} ({} bytes)", host.name, reg.name, val.len());
        registered.push((reg.name.clone(), val));
    }
    // Publish to the group coordinator (D-077) so concurrently-running peers
    // awaiting these facts (e.g. a control-plane join awaiting the init node's
    // token) unblock immediately — keyed as the consumers reference them.
    if let Some(c) = coord {
        let mut kv: Vec<(String, String)> = Vec::new();
        for (name, val) in &registered {
            kv.push((format!("hostvars.{}.{name}", host.name), val.clone()));
            for r in &host.roles {
                kv.push((format!("hostvars.{r}.{name}"), val.clone()));
            }
        }
        if !kv.is_empty() {
            c.publish(&kv).await;
        }
    }
    Ok((host.name.clone(), registered))
}




/// Group hosts by their role-set (sorted), preserving each signature's first
/// appearance order. Same-role hosts land in one group (run in parallel);
/// distinct role-sets form ordered groups (run sequentially) so a producer role
/// registers its facts before a consumer role consumes them (F17 + D-030).
pub(crate) fn group_hosts_by_role(hosts: &[crater_core::spec::Host]) -> Vec<Vec<&crater_core::spec::Host>> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<&crater_core::spec::Host>> = BTreeMap::new();
    for h in hosts {
        let mut roles = h.roles.clone();
        roles.sort();
        let sig = roles.join(",");
        if !groups.contains_key(&sig) {
            order.push(sig.clone());
        }
        groups.entry(sig).or_default().push(h);
    }
    order.into_iter().filter_map(|s| groups.remove(&s)).collect()
}

/// Max hosts to deploy concurrently within a group (`CRATER_FORKS`, default 10).
pub(crate) fn forks_limit() -> usize {
    std::env::var("CRATER_FORKS")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n >= 1)
        .unwrap_or(10)
}






/// Offline deploy via the SAME pipeline as online (D-020 单管线): unpack the OCI
/// bundle, build a synthetic spec (components from the bundle's crater-manifest,
/// inventory from CLI), set `Artifacts::Offline`, and run `run_pipeline`. So
/// offline gets the same host grouping / parallelism / register / idempotency —
/// the only difference is where artifacts come from (the bundle, on control).
pub(crate) async fn apply_oci_bundle(
    bundle_file: &Path,
    hosts: Vec<crater_core::spec::Host>,
    do_apply: bool,
    do_shell: bool,
    teardown: bool,
    source: &str,
    name: Option<&str>,
) -> Result<()> {
    let dest_root = std::env::temp_dir().join(format!("crater-deploy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dest_root);
    bundle::unpack(bundle_file, &dest_root)?; // unpacks the OCI layout into dest_root

    // crater task artifact (B 类, D-045) → replay via the task pipeline. (A
    // bundle is homogeneous: built from one task.)
    let recipe_dir = dest_root.join("__components");
    let mats = bundle::read_artifact_components(&dest_root, &recipe_dir)?;
    if mats.is_empty() {
        let _ = std::fs::remove_dir_all(&dest_root);
        anyhow::bail!(
            "{}: not a crater task artifact (legacy component bundles are no longer \
             supported — rebuild with `crater build -f tasks/<name>.yaml`)",
            bundle_file.display()
        );
    }
    if !mats
        .iter()
        .all(|mc| crater_core::task::is_task_file(&recipe_dir.join(&mc.name).join("component.yaml")))
    {
        let _ = std::fs::remove_dir_all(&dest_root);
        anyhow::bail!(
            "{}: legacy component artifact; rebuild as a task",
            bundle_file.display()
        );
    }
    info!("offline (task artifact): {} task(s)", mats.len());
    for mc in mats {
        let recipe_file = recipe_dir.join(&mc.name).join("component.yaml");
        let opts = RunOpts {
            offline_blobmap: Some(mc.blobmap),
            offline: true, // a .oci bundle is the full closure → strict air-gap
            do_apply,
            do_shell,
            teardown,
            source: source.to_string(),
        };
        apply_task(&recipe_file, hosts.clone(), opts, name, None, BTreeMap::new()).await?;
    }
    let _ = std::fs::remove_dir_all(&dest_root);
    Ok(())
}

/// Resolve deploy targets for image/oci/component sources, three layers:
///   `-i inventory.yaml`  → fleet, per-host creds (from the file);
///   `--host a,b,c`        → small fleet, ONE shared credential (user+password|key);
///   neither               → a single LOCAL host (runs on the control machine).
///
/// Heterogeneous per-host credentials are intentionally NOT expressible via
/// `--host` (it shares one credential) — use an inventory file for that.
/// The roles dir for a delivery (D-086): the task/project file's sibling `roles/`
/// (self-contained delivery), falling back to `./roles` (cwd) for back-compat.
pub(crate) fn roles_dir_for(spec_dir: &Path) -> PathBuf {
    let local = spec_dir.join("roles");
    if local.is_dir() {
        local
    } else {
        PathBuf::from("roles")
    }
}

/// Resolve a bare `<name>` to a task/project file (D-085): an explicit path, else
/// the first `<name>.yaml` found under `library/` (then `tasks/` for back-compat).
pub(crate) fn find_named(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from(name);
    if p.is_file() {
        return Some(p);
    }
    for root in ["library", "tasks"] {
        if let Some(f) = find_yaml_under(Path::new(root), name) {
            return Some(f);
        }
    }
    None
}

/// Recursively find `<name>.yaml` under `dir` (first match, dirs after files).
pub(crate) fn find_yaml_under(dir: &Path, name: &str) -> Option<PathBuf> {
    let target = format!("{name}.yaml");
    let rd = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let path = e.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if path.file_name().and_then(|n| n.to_str()) == Some(target.as_str()) {
            return Some(path);
        }
    }
    subdirs.sort();
    subdirs.into_iter().find_map(|d| find_yaml_under(&d, name))
}





// ---------------------------------------------------------------------------
// M4: AI copilot — natural language -> validated crater.yaml
// ---------------------------------------------------------------------------

/// systemd unit names mentioned by tasks under `tasks/` (their `service` /
/// `systemd_unit` actions). `doctor` derives per-unit journal probes from this

pub(crate) fn print_plan(plan: &[Op]) {
    for (i, op) in plan.iter().enumerate() {
        println!("{:>2}. [{:?}] {}", i + 1, op.phase(), op.describe());
        if let Some(p) = op.preview() {
            let oneline: String = p.lines().next().unwrap_or("").chars().take(120).collect();
            println!("      $ {oneline}");
        }
    }
}
