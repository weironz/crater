//! **Stack** —— 蓝图之间的编排。
//!
//! 「先装 containerd,再建 k8s,再上存储」是三份蓝图的**顺序**,不是一个巨型蓝图。
//! 这条界线是 A1/A2 分工的全部:
//! - A1(`parts:`)管**一份蓝图内部**的篇幅;
//! - A2(本模块)管**蓝图之间**的组合。
//!
//! 任何一把刀试图包办两件事,都会退化回 include 树 —— 那正是 Ansible role
//! 依赖最难读的地方:一个 `include_role` 既可能是"拆篇幅",也可能是"上下游",
//! 读的人无从分辨。
//!
//! v1.1 的边界写死一条:**跨蓝图 export 不可见**。蓝图自包含,栈只管顺序;
//! 要传值就升成参数。放开它等于让蓝图之间产生隐式耦合,那时"这份蓝图能不能
//! 单独跑"就再也没有确定答案了。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::eval::Yaml;
use crate::parse::{known_keys, scalar_to_string};
use crate::Error;

type Result<T> = std::result::Result<T, Error>;

/// 一个栈:有序的蓝图清单。
#[derive(Debug, Clone)]
pub struct Stack {
    pub name: String,
    /// **有序**:apply 自上而下,destroy 逆序。
    pub uses: Vec<Use>,
}

/// 栈里的一条:用哪份蓝图、怎么给它参数、它的组名对应 inventory 的哪个组。
#[derive(Debug, Clone)]
pub struct Use {
    /// 蓝图引用:库内名 / 相对路径。
    pub blueprint: String,
    /// 作者侧的参数覆盖 —— 效力是**更强的默认值**,仍可被 CLI `--set` 盖过。
    /// 运行期的优先级层数因此保持不变,栈只是往"默认值"那一层里加了一笔。
    pub params: BTreeMap<String, Yaml>,
    /// 组名重映射:**蓝图组名 → inventory 组名**。未映射的组按同名匹配。
    pub groups: BTreeMap<String, String>,
}

impl Use {
    /// 这条在报告里怎么称呼。
    pub fn label(&self) -> &str {
        &self.blueprint
    }
}

/// 这个 YAML 是不是一份 stack(而非 blueprint)。
pub fn is_stack(v: &Yaml) -> bool {
    v.as_mapping()
        .map(|m| m.contains_key(Yaml::from("stack")) && m.contains_key(Yaml::from("uses")))
        .unwrap_or(false)
}

pub fn from_str(text: &str) -> Result<Stack> {
    let v: Yaml = serde_yaml::from_str(text).map_err(|e| Error::parse(format!("YAML:{e}")))?;
    from_yaml(&v)
}

pub fn from_path(path: &Path) -> Result<Stack> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::parse(format!("读不到 {}:{e}", path.display())))?;
    from_str(&text)
}

pub fn from_yaml(v: &Yaml) -> Result<Stack> {
    let m = v
        .as_mapping()
        .ok_or_else(|| Error::parse("stack 文件应是 map"))?;
    known_keys(m, &["crater", "stack", "description", "uses"], "stack")?;

    let name = m
        .get(Yaml::from("stack"))
        .map(scalar_to_string)
        .ok_or_else(|| Error::parse("stack 缺少 `stack:`(栈名)"))?;

    let items = match m.get(Yaml::from("uses")) {
        Some(Yaml::Sequence(s)) if !s.is_empty() => s,
        Some(Yaml::Sequence(_)) | None => {
            return Err(Error::parse(
                "stack 的 `uses:` 不能为空 —— 一个不装任何蓝图的栈没有意义",
            ))
        }
        Some(_) => return Err(Error::parse("`uses:` 应是有序列表(apply 自上而下)")),
    };

    let mut uses = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let e = item
            .as_mapping()
            .ok_or_else(|| Error::parse(format!("uses[{i}] 应是 map")))?;
        known_keys(e, &["blueprint", "params", "groups"], &format!("uses[{i}]"))?;
        let blueprint = e
            .get(Yaml::from("blueprint"))
            .map(scalar_to_string)
            .ok_or_else(|| Error::parse(format!("uses[{i}] 缺少 `blueprint:`")))?;
        uses.push(Use {
            blueprint,
            params: map_of(e.get(Yaml::from("params")), &format!("uses[{i}].params"))?,
            groups: map_of(e.get(Yaml::from("groups")), &format!("uses[{i}].groups"))?
                .into_iter()
                .map(|(k, v)| (k, crate::eval::scalar_to_string(&v)))
                .collect(),
        });
    }

    // 同一份蓝图在一个栈里出现两次:两次部署会互相覆盖对方的状态记录,
    // 而"哪一次赢"取决于顺序 —— 与其让人事后 debug,不如现在就拒绝。
    let mut seen: Vec<&str> = Vec::new();
    for u in &uses {
        if seen.contains(&u.blueprint.as_str()) {
            return Err(Error::parse(format!(
                "蓝图 `{}` 在栈里出现了两次 —— 两次部署会互相覆盖状态记录。\
                 若要装两套,请给它们不同的蓝图(或不同的 name)",
                u.blueprint
            )));
        }
        seen.push(&u.blueprint);
    }
    Ok(Stack { name, uses })
}

fn map_of(v: Option<&Yaml>, what: &str) -> Result<BTreeMap<String, Yaml>> {
    match v {
        None | Some(Yaml::Null) => Ok(BTreeMap::new()),
        Some(Yaml::Mapping(m)) => Ok(m
            .iter()
            .map(|(k, val)| (scalar_to_string(k), val.clone()))
            .collect()),
        Some(_) => Err(Error::parse(format!("`{what}` 应是 map"))),
    }
}

/// 把 `blueprint:` 引用解析成磁盘路径。
///
/// 三种写法,按"最不容易误判"的顺序试:
/// 1. 显式相对/绝对路径(含 `/` 或以 `.yaml` 结尾)—— 原样解析;
/// 2. 与栈同目录的 `<名>.blueprint.yaml` / `<名>.yaml`;
/// 3. 同目录的 `<名>/<名>.blueprint.yaml`(一蓝图一目录的布局)。
///
/// 找不到时**列出试过的路径**:引用解析失败最烦人的形态是只说"找不到"。
pub fn resolve_ref(reference: &str, stack_dir: &Path) -> Result<PathBuf> {
    let mut tried = Vec::new();
    let mut probe = |p: PathBuf| -> Option<PathBuf> {
        if p.is_file() {
            return Some(p);
        }
        tried.push(p);
        None
    };

    if reference.contains('/') || reference.ends_with(".yaml") || reference.ends_with(".yml") {
        if let Some(p) = probe(stack_dir.join(reference)) {
            return Ok(p);
        }
    } else {
        for cand in [
            stack_dir.join(format!("{reference}.blueprint.yaml")),
            stack_dir.join(format!("{reference}.yaml")),
            stack_dir
                .join(reference)
                .join(format!("{reference}.blueprint.yaml")),
        ] {
            if let Some(p) = probe(cand) {
                return Ok(p);
            }
        }
    }
    Err(Error::parse(format!(
        "栈引用的蓝图 `{reference}` 找不到。试过:\n{}",
        tried
            .iter()
            .map(|p| format!("  {}", p.display()))
            .collect::<Vec<_>>()
            .join("\n")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    const S: &str = r#"
crater: 1
stack: platform
uses:
  - blueprint: containerd
  - blueprint: k8s-cluster
    params: { ha: true }
    groups: { controlplane: k8s_masters }
"#;

    #[test]
    fn a_stack_preserves_the_declared_order() {
        // 顺序**就是**语义:apply 自上而下,destroy 逆序。
        let s = from_str(S).unwrap();
        assert_eq!(
            s.uses
                .iter()
                .map(|u| u.blueprint.as_str())
                .collect::<Vec<_>>(),
            vec!["containerd", "k8s-cluster"]
        );
    }

    #[test]
    fn author_side_params_and_group_remapping_are_read() {
        let s = from_str(S).unwrap();
        assert_eq!(s.uses[1].params["ha"], Yaml::from(true));
        assert_eq!(s.uses[1].groups["controlplane"], "k8s_masters");
        assert!(s.uses[0].params.is_empty() && s.uses[0].groups.is_empty());
    }

    #[test]
    fn an_empty_stack_is_refused() {
        let err = from_str("stack: x\nuses: []\n").unwrap_err().to_string();
        assert!(err.contains("不能为空"), "{err}");
    }

    #[test]
    fn the_same_blueprint_twice_is_refused_with_a_reason() {
        // 两次部署会互相覆盖状态记录,而"哪次赢"取决于顺序。
        let err = from_str("stack: x\nuses:\n  - blueprint: a\n  - blueprint: a\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("两次") && err.contains("状态记录"), "{err}");
    }

    #[test]
    fn a_typo_in_a_top_level_key_is_caught() {
        let err = from_str("stack: x\nuse:\n  - blueprint: a\n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("use"), "{err}");
    }

    #[test]
    fn a_blueprint_is_told_apart_from_a_stack_by_shape() {
        let stack: Yaml = serde_yaml::from_str(S).unwrap();
        assert!(is_stack(&stack));
        let bp: Yaml = serde_yaml::from_str("name: t\nresources: []\n").unwrap();
        assert!(!is_stack(&bp));
    }

    #[test]
    fn an_unresolvable_reference_lists_what_was_tried() {
        // 只说"找不到"是引用解析最烦人的失败形态。
        let dir = tempfile::tempdir().unwrap();
        let err = resolve_ref("nope", dir.path()).unwrap_err().to_string();
        assert!(err.contains("nope.blueprint.yaml"), "{err}");
        assert!(err.contains("试过"), "{err}");
    }

    #[test]
    fn a_reference_resolves_by_convention_then_by_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("db.blueprint.yaml"), "name: db\n").unwrap();
        assert_eq!(
            resolve_ref("db", dir.path()).unwrap(),
            dir.path().join("db.blueprint.yaml")
        );
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/x.yaml"), "name: x\n").unwrap();
        assert_eq!(
            resolve_ref("sub/x.yaml", dir.path()).unwrap(),
            dir.path().join("sub/x.yaml")
        );
    }
}
