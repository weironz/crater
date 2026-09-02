//! Task model (D-037): `crater apply <action>` — a declarative set of states to
//! reach on targets. A *task* = ordered `actions` + materials + targeting; an
//! *action* = one primitive call (`place`/`run_cmd`/…).
//!
//! Strictly under D-036: a task is pure DATA. Every action carries only a
//! primitive + its params + **closed-enum** switches (`when_os`/`when_offline`) +
//! declarative `needs`. There is NO `when:` expression, NO `loop:`, NO
//! computation — all control flow (filtering, ordering, looping) is done by the
//! Rust engine in [`crate::engine::plan_from_task`].

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::component::{Action, Material, RegisterSpec};
use crate::engine::Phase;
use crate::module::ModuleDescriptor;

/// A task file: `crater apply <name>.yaml`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TaskFile {
    pub name: String,
    /// One-line human summary, shown by `crater inspect` (D-081).
    #[serde(default)]
    pub description: Option<String>,
    /// Optional embedded inventory (D-084): makes a task file self-contained —
    /// `crater apply -f x.yaml` (no `-i`) targets these hosts. An explicit `-i`
    /// overrides it. **`crater build` strips this** (never bake creds into an OCI).
    #[serde(default)]
    pub inventory: Option<crate::spec::Inventory>,
    /// Target group name, or `all` (default). A declarative label the engine
    /// resolves against the inventory — NOT an expression (D-036).
    #[serde(default = "default_hosts")]
    pub hosts: String,
    /// Admission contract (D-102): which target environments this task
    /// supports (distro / version / arch). Checked at THREE points, all
    /// before anything executes: `inspect` (zero connection — shown from the
    /// artifact), `plan` (connected, zero execution), and apply's fleet-wide
    /// admission pass (ALL targets probed first; one mismatch → nothing runs).
    /// Absent = unrestricted (backward compatible).
    #[serde(default, skip_serializing_if = "Requires::is_empty")]
    pub requires: Requires,
    /// Input contract (D-081, Helm `values.schema`-like): declared parameters
    /// with description / default / required / stage. A param's `default` seeds
    /// the render vars (see [`TaskFile::effective_vars`]); `crater inspect`
    /// prints this so a consumer knows what to put in their inventory.
    #[serde(default)]
    pub params: BTreeMap<String, ParamSpec>,
    /// Static data, exposed to templates as `{{ key }}` (pure substitution).
    /// A simple shorthand / override for `params` defaults.
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    /// Offline material closure (D-034) — what `crater build` packs.
    #[serde(default)]
    pub materials: Vec<Material>,
    /// Facts to capture on each host after its actions run, exposed to later
    /// host groups as `hostvars.<host>.<name>` (D-030, now in the task model).
    #[serde(default)]
    pub register: Vec<RegisterSpec>,
    /// Roles whose host group runs **one host at a time** (D-071). A role-group
    /// holding any of these gets `forks=1` — e.g. control-plane `kubeadm join`
    /// must be serial so additional masters don't race on etcd quorum.
    #[serde(default)]
    pub serial_roles: Vec<String>,
    /// Ordered actions. Dependencies via `needs`; the engine topo-sorts.
    #[serde(default)]
    pub actions: Vec<ActionStep>,
    /// Teardown actions (D-049): the product-specific cleanup/uninstall, run by
    /// `crater delete`. **Authored data, NOT auto-derived** — real software's
    /// cleanup (kubeadm reset, rm /var/lib/mysql, rm /var/lib/docker) targets
    /// runtime-generated state that the install steps never created, so it can't
    /// be inverted from `actions`. Empty = this task has NO delete capability
    /// (delete is opt-in, never forced). Same primitives/engine/idempotency.
    #[serde(default)]
    pub teardown: Vec<ActionStep>,
    /// Handlers (D-037-b): actions run once at the end, only if a `changed`
    /// action `notify`d them (by `id`). Deduped, in notify order.
    #[serde(default)]
    pub handlers: Vec<ActionStep>,
}

fn default_hosts() -> String {
    "all".into()
}

/// One declared input parameter (D-081) — the contract a consumer reads via
/// `crater inspect` to know what to set in their inventory.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParamSpec {
    /// What the parameter is for (shown by `inspect`).
    #[serde(default)]
    pub description: Option<String>,
    /// Default value; seeds the render vars. Absent + `required` ⇒ must be supplied.
    #[serde(default)]
    pub default: Option<String>,
    /// Must be supplied (when no default) or the run errors before deploying.
    #[serde(default)]
    pub required: bool,
    /// `build` (affects materials → frozen into the OCI) vs `apply` (environment
    /// config, supplied at deploy). Drives the online/offline story (D-078/081).
    #[serde(default)]
    pub stage: ParamStage,
}

/// The task's environment admission contract (D-102). Pure data, engine
/// compares (D-036) — no version-range expressions; enumerate versions
/// (prefix matches: `"9"` admits Rocky's `9.4`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct Requires {
    /// Acceptable OSes — ANY entry matching admits the host. Empty = any OS.
    #[serde(default)]
    pub os: Vec<OsRequire>,
    /// Acceptable architectures (`amd64`/`arm64`, uname spellings accepted as
    /// aliases via [`crate::arch::Arch`] naming). Empty = any arch.
    #[serde(default)]
    pub arch: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct OsRequire {
    /// `/etc/os-release` `ID`, lowercase: `ubuntu` / `debian` / `openeuler` /
    /// `kylin` / `rocky` … (distro, NOT family — that's the point, D-102).
    pub distro: String,
    /// Acceptable `VERSION_ID`s; exact or prefix (`"9"` admits `9.4`,
    /// `"22.04"` admits `22.04.3`). Empty = every version of this distro.
    #[serde(default)]
    pub versions: Vec<String>,
}

impl Requires {
    pub fn is_empty(&self) -> bool {
        self.os.is_empty() && self.arch.is_empty()
    }

    /// Human-readable contract, for `inspect` and refusal messages.
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.os.is_empty() {
            let os: Vec<String> = self
                .os
                .iter()
                .map(|r| {
                    if r.versions.is_empty() {
                        r.distro.clone()
                    } else {
                        format!("{} {}", r.distro, r.versions.join("/"))
                    }
                })
                .collect();
            parts.push(os.join(" | "));
        }
        if !self.arch.is_empty() {
            parts.push(format!("arch {}", self.arch.join("/")));
        }
        if parts.is_empty() {
            "不限".to_string()
        } else {
            parts.join(",")
        }
    }

    /// Err(reason) when the detected identity violates the contract (D-102).
    pub fn check(&self, os: &crate::os::OsInfo, arch: crate::arch::Arch) -> Result<(), String> {
        if !self.os.is_empty() {
            let admitted = self.os.iter().any(|r| {
                r.distro == os.distro
                    && (r.versions.is_empty()
                        || r.versions
                            .iter()
                            .any(|v| os.version == *v || os.version.starts_with(&format!("{v}."))))
            });
            if !admitted {
                return Err(format!(
                    "OS 不符:目标是 {} {},task 要求 {}",
                    if os.distro.is_empty() {
                        "<未知发行版>"
                    } else {
                        &os.distro
                    },
                    if os.version.is_empty() {
                        "<未知版本>"
                    } else {
                        &os.version
                    },
                    self.describe()
                ));
            }
        }
        if !self.arch.is_empty() {
            let name = arch.as_str();
            let admitted = self
                .arch
                .iter()
                .any(|a| crate::arch::Arch::from_uname(a) == arch || a == name);
            if !admitted {
                return Err(format!(
                    "架构不符:目标是 {name},task 要求 {}",
                    self.arch.join("/")
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ParamStage {
    /// Resolved at build time (offline: frozen in the OCI), e.g. component version.
    Build,
    /// Resolved at apply time (per-environment), e.g. vip / subnet. The default.
    #[default]
    Apply,
}

/// One action in a task: a primitive (flattened: `action: place` + its params)
/// plus declarative metadata. None of these fields require *execution* to
/// understand — they are read statically by the engine (D-036).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionStep {
    /// Stable id for `needs` references. Defaults to `action<index>`.
    #[serde(default)]
    pub id: Option<String>,
    /// Ids this step depends on; the engine topologically orders (NOT YAML).
    #[serde(default)]
    pub needs: Vec<String>,
    /// `install` (default) / `verify` / `preflight` — affects idempotency report.
    #[serde(default)]
    pub phase: Phase,
    /// Closed-enum condition: run only on these OS families (empty = all).
    #[serde(default)]
    pub when_os: Vec<String>,
    /// Closed-enum condition (D-071): run only on hosts holding one of these
    /// inventory roles (empty = all). Drives asymmetric multi-node topologies —
    /// e.g. `kubeadm init` on `[bootstrap]`, `kubeadm join` on `[worker]`.
    #[serde(default)]
    pub when_role: Vec<String>,
    /// Run only on the **first** target host that matches this step's `when_role`
    /// (inventory order). Mirrors kubekey's `run_once` + `init_kubernetes_node ==
    /// inventory_hostname`: the cluster's init node is implicitly the first
    /// control-plane host — no separate `bootstrap` role needed. Combine with
    /// `when_role: [controlplane]` so `kubeadm init` runs once on the first master
    /// while the rest fall through to a `--control-plane` join (D-077).
    #[serde(default)]
    pub run_once: bool,
    /// Cap how many target hosts run THIS step concurrently (D-077). `Some(1)` =
    /// strictly one at a time across the hosts the step targets (the rest queue);
    /// `None` = no cap (all run together, bounded only by global forks). A generic
    /// concurrency primitive — the engine knows nothing about *why* (e.g. a task
    /// throttles `kubeadm join` to 1 so control-plane nodes don't race on etcd).
    #[serde(default)]
    pub throttle: Option<usize>,
    /// Closed-enum condition: run only offline (`true`) / only online (`false`).
    #[serde(default)]
    pub when_offline: Option<bool>,
    /// Engine retry count (data). Runtime behavior lands in D-037-b. `0` = none.
    #[serde(default)]
    pub retries: u32,
    /// Continue past a failure even after retries (D-037-b): the step reports
    /// `warn` instead of aborting.
    #[serde(default)]
    pub ignore_errors: bool,
    /// Handler ids to trigger when this step reports `changed` (D-037-b).
    #[serde(default)]
    pub notify: Vec<String>,
    /// Step-level iteration — ansible `loop` (D-105). A list of SCALARS (pure
    /// data, no expressions); the step is macro-expanded at plan time into one
    /// step per item, with `{{item}}` substituted into every string field of
    /// the action (data-level, so it works for ALL modules/fields uniformly).
    /// Expanded ids get an `@<index>` suffix; other steps' `needs` referencing
    /// the original id are remapped to ALL expanded steps. Items may themselves
    /// contain `{{var}}` — substitution happens first, normal var rendering
    /// after, so `loop: ["{{port}}", 9001]` composes. Not allowed on handlers.
    #[serde(rename = "loop", default, skip_serializing_if = "Vec::is_empty")]
    pub loop_items: Vec<serde_yaml::Value>,
    /// The primitive + its params, flattened so the YAML stays flat:
    /// `{ id, action: place, dest: …, needs: [..] }`.
    #[serde(flatten)]
    pub action: Action,
}

/// D-105: expand `loop:` steps into per-item steps (see [`ActionStep::loop_items`]).
/// Every returned step has an EXPLICIT id (auto ids are resolved against the
/// ORIGINAL indices first, so expansion can't shift `action<i>` references).
pub fn expand_loops(actions: &[ActionStep]) -> crate::Result<Vec<ActionStep>> {
    use serde_yaml::Value;
    // Walk a YAML tree substituting `{{item}}` in every string scalar.
    fn subst(v: &mut Value, item: &str) {
        match v {
            Value::String(s) if s.contains("{{item}}") => *s = s.replace("{{item}}", item),
            Value::Sequence(xs) => xs.iter_mut().for_each(|x| subst(x, item)),
            Value::Mapping(m) => m.values_mut().for_each(|x| subst(x, item)),
            _ => {}
        }
    }
    let mut out: Vec<ActionStep> = Vec::new();
    let mut remap: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, s) in actions.iter().enumerate() {
        let base = s.id.clone().unwrap_or_else(|| format!("action{i}"));
        if s.loop_items.is_empty() {
            let mut step = s.clone();
            step.id = Some(base);
            out.push(step);
            continue;
        }
        let mut expanded_ids = Vec::new();
        for (j, item) in s.loop_items.iter().enumerate() {
            let item = match item {
                Value::String(x) => x.clone(),
                Value::Number(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                other => anyhow::bail!(
                    "step '{base}': loop 项只支持标量(字符串/数字/布尔),收到 {other:?}"
                ),
            };
            // Data-level expansion: serialize the action, substitute, re-parse —
            // uniform across all modules and fields (no per-field allowlist).
            let mut av = serde_yaml::to_value(&s.action)?;
            subst(&mut av, &item);
            let action: Action = serde_yaml::from_value(av).map_err(|e| {
                anyhow::anyhow!(
                    "step '{base}' loop[{j}] ({item}): 代入 {{{{item}}}} 后解析失败:{e}"
                )
            })?;
            let id = format!("{base}@{j}");
            expanded_ids.push(id.clone());
            let mut step = s.clone();
            step.id = Some(id);
            step.loop_items = Vec::new();
            step.action = action;
            out.push(step);
        }
        remap.insert(base, expanded_ids);
    }
    // `needs: [<looped id>]` now means "after ALL of its expansions".
    if !remap.is_empty() {
        for step in &mut out {
            if step.needs.iter().any(|n| remap.contains_key(n)) {
                step.needs = step
                    .needs
                    .iter()
                    .flat_map(|n| remap.get(n).cloned().unwrap_or_else(|| vec![n.clone()]))
                    .collect();
            }
        }
    }
    Ok(out)
}

impl TaskFile {
    pub fn from_yaml_file(path: &Path) -> crate::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        Ok(serde_yaml::from_str(&text)?)
    }

    /// Flatten role invocations into this task (D-080). Every `action: role`
    /// whose role is a **bundle** (has `actions:`) is replaced in-place by the
    /// role's actions (id- and material-name-prefixed, params rendered), and the
    /// role's `materials`/`handlers` are hoisted into the task — so the build
    /// closure and the planner see one flat task with no role indirection.
    ///
    /// Legacy thin roles (single `act`) are left as `action: role` for the engine
    /// to lower. Idempotent for role-free tasks. Call before build and planning.
    /// Effective template vars (D-081): each param's `default`, overlaid by any
    /// explicit `vars` (and, later, inventory/CLI overrides). Use this everywhere
    /// instead of `vars` directly so declared param defaults take effect.
    pub fn effective_vars(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        for (k, p) in &self.params {
            if let Some(d) = &p.default {
                m.insert(k.clone(), d.clone());
            }
        }
        for (k, v) in &self.vars {
            m.insert(k.clone(), v.clone());
        }
        m
    }

    /// Error if any `required` param has no default and isn't in `provided` (the
    /// merged var set). `stage` filters which params to check — `Some(Build)` at
    /// build (only version-like inputs), `None` at apply (all). Fail fast.
    pub fn validate_params(
        &self,
        provided: &BTreeMap<String, String>,
        stage: Option<ParamStage>,
    ) -> crate::Result<()> {
        let missing: Vec<&String> = self
            .params
            .iter()
            .filter(|(_, p)| stage.is_none_or(|s| s == p.stage))
            .filter(|(k, p)| p.required && p.default.is_none() && !provided.contains_key(*k))
            .map(|(k, _)| k)
            .collect();
        if !missing.is_empty() {
            anyhow::bail!(
                "缺少必填参数 {missing:?} —— 在 inventory 的 vars 提供(`crater inspect` 看完整契约)"
            );
        }
        Ok(())
    }

    /// Inventory roles this task branches on — the union of `when_role` across
    /// actions/teardown/handlers/register. Tells a consumer which groups to define.
    pub fn roles_needed(&self) -> Vec<String> {
        let mut s: BTreeSet<String> = BTreeSet::new();
        for a in self
            .actions
            .iter()
            .chain(&self.teardown)
            .chain(&self.handlers)
        {
            s.extend(a.when_role.iter().cloned());
        }
        for r in &self.register {
            s.extend(r.when_role.iter().cloned());
        }
        s.into_iter().collect()
    }

    pub fn expand_roles(&mut self, roles_dir: &Path) -> crate::Result<()> {
        let mut materials = std::mem::take(&mut self.materials);
        let mut handlers = std::mem::take(&mut self.handlers);
        let actions = std::mem::take(&mut self.actions);
        self.actions = expand_steps(&actions, roles_dir, &mut materials, &mut handlers)?;
        self.materials = materials;
        self.handlers = handlers;
        Ok(())
    }
}

/// Recursively expand role-bundle invocations in `steps`, hoisting role
/// materials/handlers into the out params. Returns the flattened action list.
fn expand_steps(
    steps: &[ActionStep],
    roles_dir: &Path,
    out_materials: &mut Vec<Material>,
    out_handlers: &mut Vec<ActionStep>,
) -> crate::Result<Vec<ActionStep>> {
    let mut result: Vec<ActionStep> = Vec::new();
    // role-step id → the role's terminal action ids, so a *sibling* step that
    // `needs:` the role-step waits for the whole role.
    let mut terminal_map: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for step in steps {
        let Action::Module { uses, with } = &step.action else {
            result.push(step.clone());
            continue;
        };
        // Resolve role as flat `roles/<uses>.yaml` OR dir `roles/<uses>/role.yaml`
        // (Ansible-aligned role dir, D-086). `role_base` = the role's own dir, used
        // to make its `src:` files/templates absolute (so they resolve regardless
        // of which task/delivery uses the role).
        let (role, role_base) = load_role(roles_dir, uses)?;
        role.check_params(uses, with)?;
        if !role.is_bundle() {
            result.push(step.clone()); // legacy thin role → engine lowers it
            continue;
        }

        // Prefix everything this invocation contributes, so two roles (or two
        // uses of one role) never collide. Prefer the step's id, else the role name.
        let prefix = step.id.clone().unwrap_or_else(|| uses.clone());
        let with_vars = with_to_strings(with);

        // Render the role's params into its actions/materials/handlers, then
        // recursively expand any nested roles inside it.
        // `loop:` expands FIRST (D-105): the material-name prefix rewrite below
        // matches by exact name, so `material: "{{item}}"` must already be a
        // concrete name when it runs. Loop is a pure macro — early is harmless.
        let ractions: Vec<ActionStep> = render_yaml(&role.actions, &with_vars)?;
        let ractions = expand_loops(&ractions)?;
        let mut rmaterials: Vec<Material> = render_yaml(&role.materials, &with_vars)?;
        // Make role-private file/template `src:` absolute (relative to the role dir),
        // so the role is self-contained no matter where it's used (D-086).
        for m in &mut rmaterials {
            if let Some(src) = &m.src {
                if src.is_relative() {
                    m.src = Some(role_base.join(src));
                }
            }
        }
        let rhandlers: Vec<ActionStep> = render_yaml(&role.handlers, &with_vars)?;
        let ractions = expand_steps(&ractions, roles_dir, out_materials, out_handlers)?;

        // Give every role action a stable id; collect ids + which are depended on.
        let mut named: Vec<(String, ActionStep)> = ractions
            .into_iter()
            .enumerate()
            .map(|(j, a)| (a.id.clone().unwrap_or_else(|| format!("action{j}")), a))
            .collect();
        let role_ids: BTreeSet<String> = named.iter().map(|(id, _)| id.clone()).collect();
        let depended: BTreeSet<String> = named
            .iter()
            .flat_map(|(_, a)| a.needs.iter().filter(|n| role_ids.contains(*n)).cloned())
            .collect();

        // Prefix material names; build local→prefixed map for ref rewriting.
        let mut materials = rmaterials;
        let mut matmap: BTreeMap<String, String> = BTreeMap::new();
        for m in &mut materials {
            let prefixed = format!("{prefix}.{}", m.name);
            let old = std::mem::replace(&mut m.name, prefixed);
            matmap.insert(old, m.name.clone());
        }
        // Prefix handler ids (so role-internal notify can target them).
        let mut handlers = rhandlers;
        let handler_ids: BTreeSet<String> = handlers.iter().filter_map(|h| h.id.clone()).collect();
        for h in &mut handlers {
            if let Some(id) = &h.id {
                h.id = Some(format!("{prefix}.{id}"));
            }
        }

        // Rewrite each role action: prefix id + internal needs + material refs +
        // role-handler notifies; entry actions inherit the invocation's needs;
        // unset targeting inherits the invocation's when_role/when_os.
        for (id, a) in &mut named {
            let had_internal = a.needs.iter().any(|n| role_ids.contains(n));
            let mut needs: Vec<String> = a
                .needs
                .iter()
                .map(|n| {
                    if role_ids.contains(n) {
                        format!("{prefix}.{n}")
                    } else {
                        n.clone()
                    }
                })
                .collect();
            if !had_internal {
                needs.extend(step.needs.iter().cloned());
            }
            a.needs = needs;
            a.id = Some(format!("{prefix}.{id}"));
            rewrite_material_refs(&mut a.action, &matmap);
            for nfy in &mut a.notify {
                if handler_ids.contains(nfy) {
                    *nfy = format!("{prefix}.{nfy}");
                }
            }
            if a.when_role.is_empty() {
                a.when_role = step.when_role.clone();
            }
            if a.when_os.is_empty() {
                a.when_os = step.when_os.clone();
            }
        }

        // Sibling steps that `needs:` this role-step wait for its terminals
        // (actions nothing else in the role depends on).
        if let Some(rid) = &step.id {
            let terminals: Vec<String> = role_ids
                .iter()
                .filter(|id| !depended.contains(*id))
                .map(|id| format!("{prefix}.{id}"))
                .collect();
            terminal_map.insert(rid.clone(), terminals);
        }

        out_materials.extend(materials);
        out_handlers.extend(handlers);
        result.extend(named.into_iter().map(|(_, a)| a));
    }

    // Rewrite sibling needs that referenced a now-expanded role-step id.
    if !terminal_map.is_empty() {
        for s in &mut result {
            if s.needs.iter().any(|n| terminal_map.contains_key(n)) {
                s.needs = s
                    .needs
                    .iter()
                    .flat_map(|n| {
                        terminal_map
                            .get(n)
                            .cloned()
                            .unwrap_or_else(|| vec![n.clone()])
                    })
                    .collect();
            }
        }
    }
    Ok(result)
}

/// Render a serializable list's `{{ param }}` refs with `vars` (yaml round-trip).
/// Unknown keys (task vars resolved later) are left literal by [`crate::engine::render`].
fn render_yaml<T: Serialize + serde::de::DeserializeOwned>(
    items: &[T],
    vars: &BTreeMap<String, String>,
) -> crate::Result<Vec<T>> {
    if items.is_empty() {
        return Ok(Vec::new());
    }
    let yaml = serde_yaml::to_string(items)?;
    let rendered = crate::engine::render(&yaml, vars)?;
    Ok(serde_yaml::from_str(&rendered)?)
}

/// Resolve a role to its descriptor + its own base dir (D-086): flat
/// `roles/<uses>.yaml` (base = roles_dir) OR dir `roles/<uses>/role.yaml`
/// (base = roles_dir/<uses>, holding its private `files/` + `templates/`).
/// Base is canonicalized so the role's `src:` paths become absolute.
fn load_role(
    roles_dir: &Path,
    uses: &str,
) -> crate::Result<(ModuleDescriptor, std::path::PathBuf)> {
    let flat = roles_dir.join(format!("{uses}.yaml"));
    let dir_file = roles_dir.join(uses).join("role.yaml");
    let (file, base) = if flat.is_file() {
        (flat, roles_dir.to_path_buf())
    } else if dir_file.is_file() {
        (dir_file, roles_dir.join(uses))
    } else {
        anyhow::bail!(
            "role '{uses}' 未找到:{} 或 {}",
            flat.display(),
            dir_file.display()
        );
    };
    let base = std::fs::canonicalize(&base).unwrap_or(base);
    Ok((ModuleDescriptor::from_yaml_file(&file)?, base))
}

/// `with:` param values (yaml scalars) as strings for templating.
fn with_to_strings(with: &BTreeMap<String, serde_yaml::Value>) -> BTreeMap<String, String> {
    with.iter()
        .map(|(k, v)| {
            let s = match v {
                serde_yaml::Value::String(s) => s.clone(),
                serde_yaml::Value::Bool(b) => b.to_string(),
                serde_yaml::Value::Number(n) => n.to_string(),
                other => serde_yaml::to_string(other)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
            };
            (k.clone(), s)
        })
        .collect()
}

/// Rewrite the material name(s) an action references, per `map` (local→prefixed).
fn rewrite_material_refs(action: &mut Action, map: &BTreeMap<String, String>) {
    let one = |s: &mut String| {
        if let Some(n) = map.get(s) {
            *s = n.clone();
        }
    };
    let opt = |o: &mut Option<String>| {
        if let Some(s) = o {
            if let Some(n) = map.get(s) {
                *s = n.clone();
            }
        }
    };
    match action {
        Action::RenderTemplate { material, .. } => one(material),
        Action::Copy { material, .. }
        | Action::Extract { material, .. }
        | Action::PkgInstall { material, .. } => opt(material),
        Action::LoadImage {
            material,
            materials,
            ..
        } => {
            opt(material);
            for m in materials {
                one(m);
            }
        }
        _ => {}
    }
}

/// Cheap probe: does this YAML file look like a task (a top-level `actions:`)?
/// Used by `crater apply <file>` to route task vs. legacy spec without forcing a
/// flag. Returns false on parse failure (caller falls back to spec).
pub fn is_task_file(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_yaml::from_str::<serde_yaml::Value>(&t).ok())
        .map(|v| v.get("actions").is_some())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_expands_per_item_and_remaps_needs() {
        // D-105: 3 items → 3 steps with @i ids and {{item}} substituted;
        // a downstream `needs` on the looped id fans out to all expansions;
        // composing {{var}} inside items survives untouched for later render.
        let yaml = r#"
name: t
actions:
  - id: gate
    action: wait_for
    port: "{{item}}"
    state: stopped
    timeout: 3
    loop: ["{{port}}", 9001, 9002]
  - id: go
    action: shell
    cmd: "echo go"
    needs: [gate]
"#;
        let t: TaskFile = serde_yaml::from_str(yaml).unwrap();
        let steps = expand_loops(&t.actions).unwrap();
        assert_eq!(steps.len(), 4);
        let ids: Vec<&str> = steps.iter().map(|s| s.id.as_deref().unwrap()).collect();
        assert_eq!(ids, ["gate@0", "gate@1", "gate@2", "go"]);
        match &steps[1].action {
            Action::WaitFor { port, .. } => assert_eq!(port.as_deref(), Some("9001")),
            _ => panic!("wait_for expected"),
        }
        match &steps[0].action {
            // item "{{port}}" passes through for normal var rendering later.
            Action::WaitFor { port, .. } => assert_eq!(port.as_deref(), Some("{{port}}")),
            _ => panic!("wait_for expected"),
        }
        assert_eq!(steps[3].needs, ["gate@0", "gate@1", "gate@2"]);

        // Non-scalar items are refused loudly.
        let bad = r#"
name: t
actions:
  - { action: shell, cmd: "echo {{item}}", loop: [{a: 1}] }
"#;
        let t: TaskFile = serde_yaml::from_str(bad).unwrap();
        assert!(expand_loops(&t.actions)
            .unwrap_err()
            .to_string()
            .contains("标量"));
    }

    #[test]
    fn parses_task_with_flattened_action() {
        // The risk point: `#[serde(flatten)]` over an internally-tagged enum.
        let yaml = r#"
name: install-yq
vars: { version: "4.53.2" }
materials:
  - { name: yq-bin, kind: file, url_tmpl: "https://x/{{version}}/yq" }
actions:
  - id: place_yq
    action: copy
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - id: verify
    action: shell
    cmd: "yq --version"
    phase: verify
    needs: [place_yq]
"#;
        let t: TaskFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(t.name, "install-yq");
        assert_eq!(t.hosts, "all");
        assert_eq!(t.actions.len(), 2);
        assert_eq!(t.actions[0].id.as_deref(), Some("place_yq"));
        assert!(matches!(
            t.actions[0].action,
            Action::Copy {
                material: Some(_),
                ..
            }
        ));
        assert_eq!(t.actions[1].phase, Phase::Verify);
        assert_eq!(t.actions[1].needs, vec!["place_yq".to_string()]);
    }

    #[test]
    fn action_names_are_ansible_module_names_only() {
        // D-070: canonical ansible-aligned names only — old crater spellings are
        // GONE (no aliases).
        use crate::component::Action;
        let canonical = [
            ("shell", "RunCmd"),
            ("package", "PkgInstall"),
            ("unarchive", "Extract"),
            ("template", "RenderTemplate"),
            ("role", "Module"),
            ("docker_container", "DockerContainer"),
        ];
        for (name, want) in canonical {
            let yaml = match want {
                "RunCmd" => format!("action: {name}\ncmd: \"true\""),
                "PkgInstall" => format!("action: {name}\npackages: {{ debian: [x] }}"),
                "Extract" => format!("action: {name}\nto: /opt"),
                "RenderTemplate" => format!("action: {name}\nmaterial: a\ndst: /etc/a"),
                "Module" => format!("action: {name}\nuses: lineinfile"),
                "DockerContainer" => format!("action: {name}\nname: app\nimage: x:1"),
                _ => unreachable!(),
            };
            let a: Action = serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("{name}: {e}"));
            let got = match a {
                Action::RunCmd { .. } => "RunCmd",
                Action::PkgInstall { .. } => "PkgInstall",
                Action::Extract { .. } => "Extract",
                Action::RenderTemplate { .. } => "RenderTemplate",
                Action::Module { .. } => "Module",
                Action::DockerContainer { .. } => "DockerContainer",
                _ => "other",
            };
            assert_eq!(got, want, "action: {name} should parse to {want}");
        }
        // Old names must now be rejected.
        for old in [
            "run_cmd",
            "command",
            "pkg_install",
            "extract",
            "render_template",
            "module",
            "write_file",
            "systemd_unit",
        ] {
            let yaml = format!("action: {old}\nname: x");
            assert!(
                serde_yaml::from_str::<Action>(&yaml).is_err(),
                "old name `{old}` should no longer parse"
            );
        }
    }

    #[test]
    fn copy_takes_content_or_src() {
        // D-068/070: `copy` takes `content` or `src`; `write_file`/`dst` are gone.
        use crate::component::Action;
        let by_content: Action =
            serde_yaml::from_str("action: copy\ndest: /etc/x\ncontent: \"hi\"").unwrap();
        let by_src: Action =
            serde_yaml::from_str("action: copy\ndest: /etc/x\nsrc: files/x").unwrap();
        for a in [by_content, by_src] {
            match a {
                Action::Copy { dest, .. } => assert_eq!(dest.to_str(), Some("/etc/x")),
                _ => panic!("should parse to Copy"),
            }
        }
    }

    #[test]
    fn service_canonical_only() {
        // D-069/070: service uses state/enabled; systemd_unit/enable/start are gone.
        use crate::component::{Action, ServiceState};
        let modern: Action =
            serde_yaml::from_str("action: service\nname: foo\nstate: restarted\nenabled: false")
                .unwrap();
        match modern {
            Action::Service { state, enabled, .. } => {
                assert_eq!(state, Some(ServiceState::Restarted));
                assert_eq!(enabled, Some(false));
            }
            _ => panic!("service should parse to Service"),
        }
    }

    #[test]
    fn file_kind_canonical_only() {
        // D-065/070: `kind: file` is the only spelling; `binary` is gone.
        use crate::component::MaterialKind;
        let yaml = "name: t\nmaterials:\n  - name: a\n    kind: file\n    url_tmpl: \"https://x/a\"\nactions: []\n";
        let t: TaskFile = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(t.materials[0].kind, MaterialKind::File);
        assert!(
            serde_yaml::from_str::<TaskFile>(
                "name: t\nmaterials:\n  - name: a\n    kind: binary\n    url_tmpl: x\nactions: []\n"
            )
            .is_err(),
            "kind: binary should no longer parse"
        );
    }

    #[test]
    fn expand_roles_flattens_bundle_and_hoists_materials() {
        // D-080: a role bundle (materials + actions) is spliced into the task,
        // its materials hoisted (name-prefixed), params rendered, ids/needs/refs
        // rewired, and a sibling `needs:` on the role-step → role terminals.
        let dir = std::env::temp_dir().join(format!("crater-roletest-{}", std::process::id()));
        let roles = dir.join("roles");
        std::fs::create_dir_all(&roles).unwrap();
        std::fs::write(
            roles.join("containerd.yaml"),
            r#"
name: containerd
params: [version]
materials:
  - { name: bin, kind: file, url_tmpl: "https://x/v{{version}}/containerd.tgz" }
actions:
  - { id: fetch, action: unarchive, material: bin, to: /usr/local }
  - { id: svc, action: shell, cmd: "systemctl enable containerd", needs: [fetch] }
handlers:
  - { id: reload, action: shell, cmd: "systemctl daemon-reload" }
"#,
        )
        .unwrap();

        let mut task: TaskFile = serde_yaml::from_str(
            r#"
name: t
materials:
  - { name: other, kind: file, url_tmpl: "https://y" }
actions:
  - { id: pre, action: shell, cmd: "echo pre" }
  - { id: cr, action: role, uses: containerd, with: { version: "2.3.1" }, needs: [pre], when_role: [node] }
  - { id: post, action: shell, cmd: "echo post", needs: [cr] }
"#,
        )
        .unwrap();
        task.expand_roles(&roles).unwrap();

        // materials hoisted + prefixed by the step id `cr` + param rendered.
        let names: Vec<&str> = task.materials.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["other", "cr.bin"]);
        let bin = task.materials.iter().find(|m| m.name == "cr.bin").unwrap();
        assert_eq!(
            bin.url_tmpl.as_deref(),
            Some("https://x/v2.3.1/containerd.tgz")
        );
        // handler hoisted + prefixed.
        assert_eq!(task.handlers.len(), 1);
        assert_eq!(task.handlers[0].id.as_deref(), Some("cr.reload"));

        // actions flattened: pre, cr.fetch, cr.svc, post.
        let ids: Vec<&str> = task
            .actions
            .iter()
            .map(|a| a.id.as_deref().unwrap())
            .collect();
        assert_eq!(ids, vec!["pre", "cr.fetch", "cr.svc", "post"]);

        let by = |id: &str| {
            task.actions
                .iter()
                .find(|a| a.id.as_deref() == Some(id))
                .unwrap()
        };
        // entry action inherits the role-step's needs + when_role; material ref prefixed.
        let fetch = by("cr.fetch");
        assert_eq!(fetch.needs, vec!["pre"]);
        assert_eq!(fetch.when_role, vec!["node"]);
        match &fetch.action {
            Action::Extract { material, .. } => assert_eq!(material.as_deref(), Some("cr.bin")),
            other => panic!("expected unarchive, got {other:?}"),
        }
        // internal need prefixed; targeting inherited.
        let svc = by("cr.svc");
        assert_eq!(svc.needs, vec!["cr.fetch"]);
        assert_eq!(svc.when_role, vec!["node"]);
        // sibling `needs: [cr]` → role terminal (svc; fetch is depended on internally).
        assert_eq!(by("post").needs, vec!["cr.svc"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn params_effective_vars_and_validation() {
        // D-081: param defaults seed vars; explicit vars override; required params
        // without a value fail validation (apply checks all, build only build-stage).
        let task: TaskFile = serde_yaml::from_str(
            r#"
name: t
params:
  version:  { default: "1.36.1", stage: build }
  vip:      { description: "control-plane VIP", required: true, stage: apply }
  pod_cidr: { default: "10.244.0.0/16" }
vars:
  version: "9.9.9"
actions: []
"#,
        )
        .unwrap();
        let ev = task.effective_vars();
        assert_eq!(ev.get("version").unwrap(), "9.9.9"); // vars overrides param default
        assert_eq!(ev.get("pod_cidr").unwrap(), "10.244.0.0/16"); // param default seeds var
        assert!(!ev.contains_key("vip")); // required, no default, not supplied

        assert!(task.validate_params(&ev, None).is_err()); // apply: vip missing → fail
        assert!(task.validate_params(&ev, Some(ParamStage::Build)).is_ok()); // build: vip is apply-stage
        let mut provided = ev.clone();
        provided.insert("vip".into(), "192.168.73.14".into());
        assert!(task.validate_params(&provided, None).is_ok()); // supplied → ok
    }
}

#[cfg(test)]
mod requires_tests {
    use super::*;
    use crate::arch::Arch;
    use crate::os::OsInfo;

    fn os(distro: &str, version: &str) -> OsInfo {
        OsInfo {
            family: crate::os::OsFamily::Unknown,
            distro: distro.into(),
            version: version.into(),
        }
    }

    /// D-102: distro+version+arch admission — exact/prefix versions, distro
    /// (not family) granularity, uname aliases for arch, empty = unrestricted.
    #[test]
    fn requires_check_matrix() {
        let req: Requires = serde_yaml::from_str(
            "os:\n  - distro: ubuntu\n    versions: [\"22.04\", \"24.04\"]\n  - distro: openeuler\narch: [amd64]\n",
        )
        .unwrap();
        // distro+version+arch all admitted
        assert!(req.check(&os("ubuntu", "24.04"), Arch::Amd64).is_ok());
        // version prefix: 24.04.1 admitted by "24.04"
        assert!(req.check(&os("ubuntu", "24.04.1"), Arch::Amd64).is_ok());
        // openeuler: any version (empty list)
        assert!(req.check(&os("openeuler", "22.03"), Arch::Amd64).is_ok());
        // wrong version
        let e = req.check(&os("ubuntu", "20.04"), Arch::Amd64).unwrap_err();
        assert!(e.contains("OS 不符"), "{e}");
        // family-mate but wrong DISTRO (debian != ubuntu — the whole point)
        assert!(req.check(&os("debian", "12"), Arch::Amd64).is_err());
        // wrong arch
        let e = req.check(&os("ubuntu", "24.04"), Arch::Arm64).unwrap_err();
        assert!(e.contains("架构不符"), "{e}");
        // empty contract admits anything
        assert!(Requires::default()
            .check(&os("kylin", "V10"), Arch::Unknown)
            .is_ok());
        // arch aliases: requirement spelled x86_64 admits Amd64
        let req2: Requires = serde_yaml::from_str("arch: [x86_64]\n").unwrap();
        assert!(req2.check(&os("", ""), Arch::Amd64).is_ok());
    }
}
