//! 机群视角 —— `on:` selector 判定的依据。
//!
//! 在此之前引擎是**逐台独立**跑的:每台机器互不知情,于是 `on:` 只能被忽略
//! (真 bug:写了 `on: role.controlplane` 的资源会在**每一台**机器上跑)。
//! `first()` / `rest()` 更是无从谈起 —— 它们需要跨主机的**稳定序**。
//!
//! 这里只放**静态成员信息**(名字 + 组),它从 inventory 就能得到,不必连机器。
//! 与之相对,`substrate.*` 是连上之后才知道的单机事实,住在 [`Scope`](crate::eval::Scope) 里。

use std::collections::{BTreeMap, BTreeSet};

use crate::eval::Scope;
use crate::ir::FleetContract;
use crate::selector::Selector;

/// 机群里的一台。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub name: String,
    /// 它属于哪些组(inventory 的 groups 推导而来)。
    pub roles: Vec<String>,
}

impl Member {
    pub fn new(name: impl Into<String>, roles: &[&str]) -> Self {
        Member {
            name: name.into(),
            roles: roles.iter().map(|s| s.to_string()).collect(),
        }
    }
    pub fn in_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }
}

/// 一次部署面向的全部目标。**顺序即 inventory 声明序** ——
/// `first()` 的语义全靠它稳定:同一份 inventory 每次跑都选中同一台。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fleet {
    pub members: Vec<Member>,
    /// inventory **声明过**的组名 —— 与"有成员的组"不是一回事。
    ///
    /// 单节点拓扑里 `worker: { hosts: [] }` 是合法且常见的:组存在,只是空的。
    /// 不单独记下来的话,它与拼错的组名无从分辨,合法拓扑会被当成错误拒绝。
    pub declared_roles: BTreeSet<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SelectError {
    /// blueprint 要求的组在 inventory 里不存在。
    ///
    /// 这必须是**错误而非静默跳过**:否则一个拼错的组名会让整段资源悄悄不执行,
    /// 而 plan 看起来一切正常 —— 那是最难查的一类故障。
    UnknownRole { role: String, known: Vec<String> },
    /// `first()` / `rest()` 里嵌了 `where`。
    Nested(String),
    Eval(String),
}

impl std::fmt::Display for SelectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SelectError::UnknownRole { role, known } => {
                write!(f, "selector 引用了组 `{role}`,但目标里没有这个组")?;
                if known.is_empty() {
                    write!(f, "(当前目标未定义任何组 —— 需要 `-i inventory.yaml`)")
                } else {
                    write!(f, "(已知的组:{})", known.join(", "))
                }
            }
            SelectError::Nested(s) => write!(
                f,
                "`{s}`:first()/rest() 需要跨主机的稳定序,而 `where` 条件依赖\
                 单机事实(连上才知道),无法在机群层判定 —— 把 `where` 移到最外层"
            ),
            SelectError::Eval(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SelectError {}

impl Fleet {
    /// 组集合从成员推导 —— 适合测试与没有显式 inventory 的场景。
    pub fn new(members: Vec<Member>) -> Self {
        let declared_roles = members.iter().flat_map(|m| m.roles.iter().cloned()).collect();
        Fleet { members, declared_roles }
    }

    /// 带上 inventory **显式声明**的组(含空组)。
    pub fn with_declared_roles(mut self, roles: impl IntoIterator<Item = String>) -> Self {
        self.declared_roles.extend(roles);
        self
    }

    /// 单机场景(没有 inventory):一台无组成员。
    pub fn single(name: impl Into<String>) -> Self {
        Fleet::new(vec![Member::new(name, &[])])
    }

    /// 按栈的组名映射(**蓝图组名 → inventory 组名**)重投影这个机群。
    ///
    /// 蓝图写 `target: role.controlplane`,而 inventory 的组叫 `k8s_masters` ——
    /// 栈负责把两个词接上,蓝图本身一字不改。这是蓝图能被复用的前提:
    /// 一份蓝图不该因为别人的 inventory 用了别的组名就要改。
    ///
    /// 语义:**被显式映射的蓝图组名不再同名直通**。若 `controlplane → k8s_masters`,
    /// 那么这份蓝图眼里的 `controlplane` 就**只有** `k8s_masters` 的成员 ——
    /// 哪怕 inventory 里恰好也有个叫 `controlplane` 的组。显式优先于巧合。
    pub fn remap(&self, map: &BTreeMap<String, String>) -> Fleet {
        if map.is_empty() {
            return self.clone();
        }
        let project = |roles: &[String]| -> Vec<String> {
            let mut out: Vec<String> = roles
                .iter()
                .filter(|r| !map.contains_key(*r))
                .cloned()
                .collect();
            for (bp_group, inv_group) in map {
                if roles.iter().any(|r| r == inv_group) {
                    out.push(bp_group.clone());
                }
            }
            out.sort();
            out.dedup();
            out
        };
        let members: Vec<Member> = self
            .members
            .iter()
            .map(|m| Member { name: m.name.clone(), roles: project(&m.roles) })
            .collect();
        // 声明过的组同样要投影:空的 `worker: []` 组经映射后仍须是"声明过但为空",
        // 否则单节点拓扑会在栈里退化成"这个组不存在"。
        let declared: Vec<String> = self.declared_roles.iter().cloned().collect();
        let mut fleet = Fleet::new(members);
        fleet.declared_roles.extend(project(&declared));
        fleet
    }

    /// 用蓝图的机群契约校验这批机器 —— **在连任何机器之前**。
    ///
    /// 没有它的时候,"HA 蓝图配了一台 master"这种错要等到 `first(role.controlplane)`
    /// 之后某一步失败才暴露,那时候前面的步骤已经改过机器了。契约把这一类错误
    /// 从"跑到一半炸"提前成"根本不开跑"。
    ///
    /// 一次报全部问题,不是遇到第一个就返回 —— 修 inventory 的人应当一趟改完。
    pub fn check_contract(&self, contract: &FleetContract) -> Result<(), Vec<String>> {
        let mut errs = Vec::new();
        for (name, gc) in &contract.groups {
            let have = self.members.iter().filter(|m| m.in_role(name)).count();
            if !self.declared_roles.contains(name) && gc.min > 0 {
                errs.push(format!("inventory 缺少组 `{name}`(蓝图要求至少 {} 台)", gc.min));
            } else if have < gc.min {
                errs.push(format!(
                    "组 `{name}` 只有 {have} 台,蓝图要求至少 {} 台",
                    gc.min
                ));
            }
        }
        // 反向:inventory 里有蓝图没声明的组,只是提示不到位,不算错 ——
        // 同一批机器常同时承载多个蓝图,多出来的角色是常态。
        if errs.is_empty() {
            Ok(())
        } else {
            Err(errs)
        }
    }

    pub fn get(&self, name: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.name == name)
    }

    fn known_roles(&self) -> Vec<String> {
        self.declared_roles.iter().cloned().collect()
    }

    /// 这台机器是否被 `sel` 选中。
    ///
    /// `scope` 提供 `where` 子句所需的单机事实;`first`/`rest` 只看静态成员信息。
    pub fn matches(&self, sel: &Selector, host: &str, scope: &Scope) -> Result<bool, SelectError> {
        match sel {
            Selector::All => Ok(true),
            Selector::Host(h) => Ok(h == host),
            Selector::Role(r) => {
                self.assert_known_role(r)?;
                Ok(self.get(host).is_some_and(|m| m.in_role(r)))
            }
            Selector::First(inner) => {
                let picked = self.static_select(inner)?;
                Ok(picked.first().is_some_and(|m| m.name == host))
            }
            Selector::Rest(inner) => {
                let picked = self.static_select(inner)?;
                Ok(picked.iter().skip(1).any(|m| m.name == host))
            }
            Selector::Where(inner, cond) => {
                if !self.matches(inner, host, scope)? {
                    return Ok(false);
                }
                scope.eval_bool(cond).map_err(SelectError::Eval)
            }
        }
    }

    /// 仅按**静态**信息(组 / 名字)取子集,保持机群顺序。
    /// 供 `first()`/`rest()` 使用 —— 它们必须在不连机器的前提下也说得清选中谁。
    pub fn static_select(&self, sel: &Selector) -> Result<Vec<&Member>, SelectError> {
        match sel {
            Selector::All => Ok(self.members.iter().collect()),
            Selector::Host(h) => Ok(self.members.iter().filter(|m| &m.name == h).collect()),
            Selector::Role(r) => {
                self.assert_known_role(r)?;
                Ok(self.members.iter().filter(|m| m.in_role(r)).collect())
            }
            Selector::First(inner) => Ok(self.static_select(inner)?.into_iter().take(1).collect()),
            Selector::Rest(inner) => Ok(self.static_select(inner)?.into_iter().skip(1).collect()),
            Selector::Where(_, _) => Err(SelectError::Nested(sel.to_string())),
        }
    }

    fn assert_known_role(&self, role: &str) -> Result<(), SelectError> {
        // 判据是"**声明过**吗",不是"有成员吗" —— 空组是合法拓扑,不是笔误。
        if self.declared_roles.contains(role) {
            return Ok(());
        }
        Err(SelectError::UnknownRole {
            role: role.to_string(),
            known: self.known_roles(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Yaml;
    use std::collections::BTreeMap;

    fn fleet() -> Fleet {
        // 顺序即 inventory 声明序 —— first() 的全部依据。
        Fleet::new(vec![
            Member::new("n11", &["controlplane", "k8s_cluster"]),
            Member::new("n12", &["controlplane", "k8s_cluster"]),
            Member::new("n13", &["controlplane", "k8s_cluster"]),
            Member::new("w01", &["worker", "k8s_cluster"]),
        ])
    }

    fn sel(s: &str) -> Selector {
        Selector::parse(s).unwrap()
    }

    fn scope_with_arch(arch: &str) -> Scope {
        let mut substrate = BTreeMap::new();
        substrate.insert("arch".to_string(), Yaml::from(arch));
        Scope { substrate, ..Default::default() }
    }

    fn hits(f: &Fleet, s: &str) -> Vec<String> {
        f.members
            .iter()
            .filter(|m| f.matches(&sel(s), &m.name, &Scope::default()).unwrap())
            .map(|m| m.name.clone())
            .collect()
    }

    #[test]
    fn all_selects_everyone() {
        assert_eq!(hits(&fleet(), "all").len(), 4);
    }

    #[test]
    fn role_selects_only_group_members() {
        // 这就是那个真 bug 的反面:此前 `on: role.worker` 会在**四台**上都跑。
        assert_eq!(hits(&fleet(), "role.worker"), vec!["w01"]);
        assert_eq!(hits(&fleet(), "role.controlplane"), vec!["n11", "n12", "n13"]);
    }

    #[test]
    fn host_selects_exactly_one() {
        assert_eq!(hits(&fleet(), "host.n12"), vec!["n12"]);
    }

    #[test]
    fn first_and_rest_partition_a_group_in_declaration_order() {
        // HA 场景的核心:首台 init,其余 join。旧模型只能"全组跑 + check 守卫跳过首台"。
        assert_eq!(hits(&fleet(), "first(role.controlplane)"), vec!["n11"]);
        assert_eq!(hits(&fleet(), "rest(role.controlplane)"), vec!["n12", "n13"]);
    }

    #[test]
    fn first_and_rest_are_complementary_and_exhaustive() {
        let f = fleet();
        let a = hits(&f, "first(role.controlplane)");
        let b = hits(&f, "rest(role.controlplane)");
        let mut all: Vec<String> = a.iter().chain(b.iter()).cloned().collect();
        all.sort();
        assert_eq!(all, vec!["n11", "n12", "n13"], "不重不漏");
    }

    #[test]
    fn first_of_a_single_member_group_leaves_rest_empty() {
        // 单 master:first 选中它,rest 为空 —— join 步骤自然不执行。
        assert_eq!(hits(&fleet(), "first(role.worker)"), vec!["w01"]);
        assert!(hits(&fleet(), "rest(role.worker)").is_empty());
    }

    #[test]
    fn where_filters_on_per_host_facts() {
        let f = fleet();
        let s = sel("role.controlplane where substrate.arch == 'arm64'");
        assert!(f.matches(&s, "n11", &scope_with_arch("arm64")).unwrap());
        assert!(!f.matches(&s, "n11", &scope_with_arch("amd64")).unwrap());
        // 组不匹配时短路,不必求值
        assert!(!f.matches(&s, "w01", &scope_with_arch("arm64")).unwrap());
    }

    #[test]
    fn a_declared_but_empty_group_selects_nobody_without_erroring() {
        // 单节点拓扑:inventory 写了 `worker: { hosts: [] }`。
        // 组存在、只是空的 —— 这是合法配置,不该被当成拼错的组名拒绝。
        let f = Fleet::new(vec![Member::new("n1", &["controlplane"])])
            .with_declared_roles(["worker".to_string()]);
        assert!(!f.matches(&sel("role.worker"), "n1", &Scope::default()).unwrap());
        assert!(f.static_select(&sel("role.worker")).unwrap().is_empty());
        // 而首台仍然选得中
        assert!(f
            .matches(&sel("first(role.controlplane)"), "n1", &Scope::default())
            .unwrap());
    }

    #[test]
    fn an_unknown_group_is_an_error_not_a_silent_skip() {
        // 拼错组名会让整段资源悄悄不执行,而 plan 看起来一切正常 ——
        // 那是最难查的一类故障,必须当场报错。
        let err = fleet()
            .matches(&sel("role.controlplna"), "n11", &Scope::default())
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("controlplna"), "{msg}");
        assert!(msg.contains("controlplane"), "要列出已知的组:{msg}");
    }

    #[test]
    fn a_lone_localhost_says_it_has_no_groups_at_all() {
        let f = Fleet::single("localhost");
        assert!(f.matches(&sel("all"), "localhost", &Scope::default()).unwrap());
        let msg = f
            .matches(&sel("role.controlplane"), "localhost", &Scope::default())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("需要 `-i inventory.yaml`"), "要给出下一步:{msg}");
    }

    #[test]
    fn where_nested_inside_first_is_refused_with_a_reason() {
        // first() 要在不连机器的前提下就说得清选中谁;where 依赖单机事实,做不到。
        let err = fleet()
            .matches(
                &sel("first(role.controlplane where substrate.arch == 'amd64')"),
                "n11",
                &Scope::default(),
            )
            .unwrap_err();
        assert!(err.to_string().contains("移到最外层"), "{err}");
    }

    #[test]
    fn static_select_preserves_fleet_order() {
        let f = fleet();
        let picked = f.static_select(&sel("role.k8s_cluster")).unwrap();
        let names: Vec<&str> = picked.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, vec!["n11", "n12", "n13", "w01"]);
    }

    #[test]
    fn a_host_not_in_the_fleet_matches_nothing_positional() {
        let f = fleet();
        assert!(!f.matches(&sel("role.worker"), "ghost", &Scope::default()).unwrap());
        assert!(!f
            .matches(&sel("first(role.controlplane)"), "ghost", &Scope::default())
            .unwrap());
    }
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use crate::ir::GroupContract;

    fn contract(pairs: &[(&str, usize)]) -> FleetContract {
        FleetContract {
            groups: pairs.iter().map(|(n, m)| (n.to_string(), GroupContract { min: *m })).collect(),
        }
    }

    #[test]
    fn a_fleet_that_meets_the_contract_passes() {
        let f = Fleet::new(vec![
            Member::new("cp1", &["controlplane"]),
            Member::new("w1", &["worker"]),
        ]);
        assert!(f.check_contract(&contract(&[("controlplane", 1), ("worker", 1)])).is_ok());
    }

    #[test]
    fn an_ha_blueprint_rejects_a_single_master_before_touching_anything() {
        // 这正是契约存在的理由:不满足在 plan 之前就说清,而不是跑到 rest() 那步才炸。
        let f = Fleet::new(vec![Member::new("cp1", &["controlplane"])]);
        let errs = f.check_contract(&contract(&[("controlplane", 3)])).unwrap_err();
        assert_eq!(errs.len(), 1);
        assert!(errs[0].contains("只有 1 台") && errs[0].contains("至少 3 台"), "{errs:?}");
    }

    #[test]
    fn a_min_of_zero_permits_an_empty_but_declared_group() {
        // 单节点拓扑:worker 组存在但没成员,是合法拓扑而不是打错字。
        let f = Fleet::new(vec![Member::new("cp1", &["controlplane"])])
            .with_declared_roles(["worker".to_string()]);
        assert!(f.check_contract(&contract(&[("controlplane", 1), ("worker", 0)])).is_ok());
    }

    #[test]
    fn a_missing_group_is_reported_as_missing_not_as_undersized() {
        // "缺少组"和"组太小"是两种不同的修法:一个改 inventory 结构,一个加机器。
        let f = Fleet::new(vec![Member::new("cp1", &["controlplane"])]);
        let errs = f.check_contract(&contract(&[("controlplane", 1), ("etcd", 3)])).unwrap_err();
        assert!(errs[0].contains("缺少组 `etcd`"), "{errs:?}");
    }

    #[test]
    fn every_unmet_requirement_is_reported_in_one_pass() {
        // 修 inventory 的人应当一趟改完,而不是修一条重跑一次再看下一条。
        let f = Fleet::new(vec![Member::new("cp1", &["controlplane"])]);
        let errs = f
            .check_contract(&contract(&[("controlplane", 3), ("etcd", 3), ("worker", 2)]))
            .unwrap_err();
        assert_eq!(errs.len(), 3, "{errs:?}");
    }

    #[test]
    fn extra_roles_in_the_inventory_are_not_an_error() {
        // 同一批机器常同时承载多个蓝图,多出来的角色是常态而非错误。
        let f = Fleet::new(vec![Member::new("cp1", &["controlplane", "monitoring", "ingress"])]);
        assert!(f.check_contract(&contract(&[("controlplane", 1)])).is_ok());
    }
}

#[cfg(test)]
mod remap_tests {
    use super::*;

    fn fleet() -> Fleet {
        Fleet::new(vec![
            Member::new("m1", &["k8s_masters"]),
            Member::new("m2", &["k8s_masters"]),
            Member::new("w1", &["k8s_workers", "storage_nodes"]),
        ])
    }

    fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
    }

    #[test]
    fn a_blueprint_sees_its_own_group_names() {
        // 蓝图写 role.controlplane,inventory 叫 k8s_masters —— 栈把两个词接上,
        // 蓝图本身一字不改。这是蓝图能被复用的前提。
        let f = fleet().remap(&map(&[("controlplane", "k8s_masters"), ("worker", "k8s_workers")]));
        let cp = f.static_select(&Selector::Role("controlplane".into())).unwrap();
        assert_eq!(cp.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["m1", "m2"]);
        let w = f.static_select(&Selector::Role("worker".into())).unwrap();
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn unmapped_groups_still_match_by_name() {
        // 大多数组名本来就对得上,不该逼人把它们全列一遍。
        let f = fleet().remap(&map(&[("controlplane", "k8s_masters")]));
        let s = f.static_select(&Selector::Role("storage_nodes".into())).unwrap();
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn an_explicit_mapping_beats_a_coincidental_same_name_group() {
        // inventory 里恰好也有个 controlplane 组,但栈说"controlplane 指的是
        // k8s_masters"—— 显式优先于巧合,否则两处会悄悄合并成一个组。
        let f = Fleet::new(vec![
            Member::new("real", &["k8s_masters"]),
            Member::new("decoy", &["controlplane"]),
        ])
        .remap(&map(&[("controlplane", "k8s_masters")]));
        let cp = f.static_select(&Selector::Role("controlplane".into())).unwrap();
        assert_eq!(cp.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), vec!["real"]);
    }

    #[test]
    fn an_empty_map_changes_nothing() {
        let f = fleet();
        let r = f.remap(&BTreeMap::new());
        assert_eq!(r.members.len(), f.members.len());
        assert_eq!(r.declared_roles, f.declared_roles);
    }

    #[test]
    fn a_declared_but_empty_group_survives_remapping() {
        // 单节点拓扑的 `worker: []`:经映射后仍须是"声明过但为空",
        // 否则栈里会退化成"这个组不存在",契约随之误报。
        let f = Fleet::new(vec![Member::new("m1", &["k8s_masters"])])
            .with_declared_roles(["k8s_workers".to_string()])
            .remap(&map(&[("controlplane", "k8s_masters"), ("worker", "k8s_workers")]));
        assert!(f.declared_roles.contains("worker"), "{:?}", f.declared_roles);
        assert!(f.static_select(&Selector::Role("worker".into())).unwrap().is_empty());
    }

    #[test]
    fn a_remapped_fleet_still_satisfies_its_contract() {
        // 端到端:重映射之后契约用的是**蓝图的**组名。
        use crate::ir::{FleetContract, GroupContract};
        let f = fleet().remap(&map(&[("controlplane", "k8s_masters"), ("worker", "k8s_workers")]));
        let contract = FleetContract {
            groups: [("controlplane", 2usize), ("worker", 1)]
                .iter()
                .map(|(n, m)| (n.to_string(), GroupContract { min: *m }))
                .collect(),
        };
        assert!(f.check_contract(&contract).is_ok());
    }
}
