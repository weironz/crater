//! 部署记录与漂移检测 —— 把对账循环**闭上**。
//!
//! `observe → diff → plan → converge` 之后还差最后一环:**记下来**。
//! 有了记录,才分得清三件本质不同的事:
//! - 从没部署过(没有记录);
//! - 部署过、现实仍符合期望(记录 + plan 空);
//! - 部署过、**现实漂了**(记录 + plan 非空)—— 这才是 drift。
//!
//! 没有记录时,后两者在 plan 里长得一模一样,`verify` 也就无从谈起。
//!
//! **存储后端**:这里定义 [`Store`] 契约并给一个零依赖的文件实现。
//! SQLite / Postgres 实现属于 server 形态(D-106 §7.1),它们是 async + sqlx,
//! 放在这个刻意保持最小依赖面的 crate 里并不合适 —— 换后端只需再实现这个 trait。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::plan::Plan;
use crate::verbs::Change;

/// 一条资源的实况记录 —— 数字孪生的一行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecord {
    pub id: String,
    pub ty: String,
    /// 上次收敛后观察到的现实(observe 的字段快照)。
    pub observed: BTreeMap<String, String>,
}

/// 一次部署实例:`blueprint × 目标` 的绑定。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeploymentRecord {
    /// 稳定标识:`<blueprint>@<target>`。
    pub id: String,
    pub blueprint: String,
    pub version: Option<String>,
    pub target: String,
    /// unix 秒。用整数而非日期字符串:比较不必解析,时区永不出错。
    pub applied_at: u64,
    pub verified_at: Option<u64>,
    pub resources: Vec<ResourceRecord>,
}

impl DeploymentRecord {
    pub fn make_id(blueprint: &str, target: &str) -> String {
        format!("{blueprint}@{target}")
    }

    /// 从一次成功的收敛结果生成记录。
    pub fn from_plan(blueprint: &str, version: Option<&str>, target: &str, plan: &Plan) -> Self {
        DeploymentRecord {
            id: Self::make_id(blueprint, target),
            blueprint: blueprint.to_string(),
            version: version.map(str::to_string),
            target: target.to_string(),
            applied_at: now(),
            verified_at: Some(now()),
            resources: plan
                .items
                .iter()
                .map(|i| ResourceRecord {
                    id: i.id.clone(),
                    ty: i.ty.clone(),
                    observed: i.observed.fields.clone(),
                })
                .collect(),
        }
    }
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 漂移判定的结论。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriftVerdict {
    /// 从没部署过 —— 这不是漂移。
    NeverDeployed,
    /// 部署过,现实仍符合期望。
    InSync,
    /// 部署过,但现实变了。
    Drifted(Vec<DriftItem>),
    /// 部署过,但有些项 plan 说不清 —— 不能断言 in-sync。
    Indeterminate { drifted: Vec<DriftItem>, unknown: usize },
}

impl DriftVerdict {
    pub fn is_drifted(&self) -> bool {
        matches!(self, DriftVerdict::Drifted(_) | DriftVerdict::Indeterminate { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftItem {
    pub id: String,
    /// 人类可读的差异描述(来自 diff 的字段级结果)。
    pub detail: String,
    /// 该资源是否在记录里出现过 —— 没出现说明是**新增**声明,不是漂移。
    pub known: bool,
}

/// 把一次(只读的)plan 与已有记录对照,给出漂移结论。
///
/// 注意:漂移的判据是**现实 vs 期望**(即 plan 本身),记录的作用是提供
/// "有没有部署过"与"上次看到的是什么样"的上下文,让报告说得清。
pub fn assess(plan: &Plan, record: Option<&DeploymentRecord>) -> DriftVerdict {
    let Some(record) = record else {
        return DriftVerdict::NeverDeployed;
    };
    let known: Vec<&str> = record.resources.iter().map(|r| r.id.as_str()).collect();

    let mut drifted = Vec::new();
    let mut unknown = 0usize;
    for item in &plan.items {
        match &item.change {
            Change::Ok => {}
            Change::Unknown(_) => unknown += 1,
            change => drifted.push(DriftItem {
                id: item.id.clone(),
                detail: describe(change),
                known: known.contains(&item.id.as_str()),
            }),
        }
    }
    match (drifted.is_empty(), unknown) {
        (true, 0) => DriftVerdict::InSync,
        (_, 0) => DriftVerdict::Drifted(drifted),
        _ => DriftVerdict::Indeterminate { drifted, unknown },
    }
}

fn describe(change: &Change) -> String {
    let fields: Vec<String> = change.fields().iter().map(|f| f.to_string()).collect();
    if fields.is_empty() {
        format!("{:?}", change)
    } else {
        fields.join("; ")
    }
}

// ---------------------------------------------------------------- 存储契约

/// 部署记录的存储。业务代码只碰这个接口 —— 换 SQLite/Postgres 不动上层。
pub trait Store {
    fn load(&self, id: &str) -> anyhow::Result<Option<DeploymentRecord>>;
    fn save(&self, record: &DeploymentRecord) -> anyhow::Result<()>;
    fn list(&self) -> anyhow::Result<Vec<DeploymentRecord>>;
    fn remove(&self, id: &str) -> anyhow::Result<bool>;
}

/// 零依赖的文件实现:一条记录一个文件,内容是 YAML。
///
/// 单机形态够用,且**可读可 diff 可进 git**;记录量级是"部署过几个 blueprint",
/// 不是事件流,所以不需要数据库。真要多实例共享状态时,换 [`Store`] 实现即可。
pub struct FileStore {
    root: PathBuf,
}

impl FileStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        FileStore { root: root.into() }
    }

    /// 默认位置:`~/.crater/state/`。
    pub fn default_location() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
        FileStore::new(Path::new(&home).join(".crater").join("state"))
    }

    fn path_of(&self, id: &str) -> PathBuf {
        // id 里有 `@` 和可能的 `/`(目标地址),编码成安全文件名。
        let safe: String = id
            .chars()
            .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
            .collect();
        self.root.join(format!("{safe}.yaml"))
    }
}

impl Store for FileStore {
    fn load(&self, id: &str) -> anyhow::Result<Option<DeploymentRecord>> {
        let path = self.path_of(id);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(Some(decode(&text)?))
    }

    fn save(&self, record: &DeploymentRecord) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.root)?;
        let path = self.path_of(&record.id);
        // 先写临时文件再改名:中途崩溃不会留下半截记录。
        let tmp = path.with_extension("yaml.tmp");
        std::fs::write(&tmp, encode(record))?;
        std::fs::rename(&tmp, &path)?;
        Ok(())
    }

    fn list(&self) -> anyhow::Result<Vec<DeploymentRecord>> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
                continue;
            }
            if let Ok(text) = std::fs::read_to_string(&path) {
                // 坏掉的单条记录不该让 `list` 整个失败。
                if let Ok(r) = decode(&text) {
                    out.push(r);
                }
            }
        }
        out.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(out)
    }

    fn remove(&self, id: &str) -> anyhow::Result<bool> {
        let path = self.path_of(id);
        if !path.exists() {
            return Ok(false);
        }
        std::fs::remove_file(path)?;
        Ok(true)
    }
}

// 手写编解码:避免为一个简单结构给 crater-ir 引入 serde derive 之外的依赖,
// 也让记录文件保持人类可读(可 diff、可进 git)。

fn encode(r: &DeploymentRecord) -> String {
    let mut y = serde_yaml::Mapping::new();
    let mut put = |k: &str, v: serde_yaml::Value| {
        y.insert(serde_yaml::Value::from(k), v);
    };
    put("id", r.id.clone().into());
    put("blueprint", r.blueprint.clone().into());
    if let Some(v) = &r.version {
        put("version", v.clone().into());
    }
    put("target", r.target.clone().into());
    put("applied_at", r.applied_at.into());
    if let Some(v) = r.verified_at {
        put("verified_at", v.into());
    }
    let resources: Vec<serde_yaml::Value> = r
        .resources
        .iter()
        .map(|res| {
            let mut m = serde_yaml::Mapping::new();
            m.insert("id".into(), res.id.clone().into());
            m.insert("type".into(), res.ty.clone().into());
            let observed: serde_yaml::Mapping = res
                .observed
                .iter()
                .map(|(k, v)| (serde_yaml::Value::from(k.clone()), serde_yaml::Value::from(v.clone())))
                .collect();
            m.insert("observed".into(), serde_yaml::Value::Mapping(observed));
            serde_yaml::Value::Mapping(m)
        })
        .collect();
    put("resources", serde_yaml::Value::Sequence(resources));
    serde_yaml::to_string(&serde_yaml::Value::Mapping(y)).unwrap_or_default()
}

fn decode(text: &str) -> anyhow::Result<DeploymentRecord> {
    let v: serde_yaml::Value = serde_yaml::from_str(text)?;
    let m = v.as_mapping().ok_or_else(|| anyhow::anyhow!("记录格式错误"))?;
    let s = |k: &str| m.get(serde_yaml::Value::from(k)).and_then(|v| v.as_str()).map(str::to_string);
    let n = |k: &str| m.get(serde_yaml::Value::from(k)).and_then(|v| v.as_u64());
    Ok(DeploymentRecord {
        id: s("id").ok_or_else(|| anyhow::anyhow!("记录缺少 id"))?,
        blueprint: s("blueprint").unwrap_or_default(),
        version: s("version"),
        target: s("target").unwrap_or_default(),
        applied_at: n("applied_at").unwrap_or(0),
        verified_at: n("verified_at"),
        resources: m
            .get(serde_yaml::Value::from("resources"))
            .and_then(|v| v.as_sequence())
            .map(|seq| {
                seq.iter()
                    .filter_map(|item| {
                        let im = item.as_mapping()?;
                        Some(ResourceRecord {
                            id: im.get(serde_yaml::Value::from("id"))?.as_str()?.to_string(),
                            ty: im
                                .get(serde_yaml::Value::from("type"))
                                .and_then(|v| v.as_str())
                                .unwrap_or_default()
                                .to_string(),
                            observed: im
                                .get(serde_yaml::Value::from("observed"))
                                .and_then(|v| v.as_mapping())
                                .map(|om| {
                                    om.iter()
                                        .filter_map(|(k, v)| {
                                            Some((k.as_str()?.to_string(), v.as_str()?.to_string()))
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::PlanItem;
    use crate::verbs::{FieldDiff, Observed};

    fn item(id: &str, change: Change) -> PlanItem {
        PlanItem {
            id: id.into(),
            ty: "file".into(),
            args: Default::default(),
            observed: Observed::present([("mode", "750".into())]),
            change,
        }
    }

    fn plan_of(items: Vec<PlanItem>) -> Plan {
        Plan { items }
    }

    fn record() -> DeploymentRecord {
        DeploymentRecord {
            id: "demo@local".into(),
            blueprint: "demo".into(),
            version: Some("1.0".into()),
            target: "local".into(),
            applied_at: 1_700_000_000,
            verified_at: Some(1_700_000_000),
            resources: vec![ResourceRecord {
                id: "file".into(),
                ty: "file".into(),
                observed: [("mode".to_string(), "750".to_string())].into(),
            }],
        }
    }

    #[test]
    fn never_deployed_is_not_drift() {
        // 关键区分:没有记录时,一个"全是 +"的 plan 是**待部署**,不是漂移。
        let p = plan_of(vec![item("file", Change::Create(vec![]))]);
        assert_eq!(assess(&p, None), DriftVerdict::NeverDeployed);
        assert!(!assess(&p, None).is_drifted());
    }

    #[test]
    fn a_clean_plan_against_a_record_is_in_sync() {
        let p = plan_of(vec![item("file", Change::Ok)]);
        assert_eq!(assess(&p, Some(&record())), DriftVerdict::InSync);
    }

    #[test]
    fn a_changed_resource_is_reported_with_field_level_detail() {
        let p = plan_of(vec![item(
            "file",
            Change::Update(vec![FieldDiff::change("mode", "777", "0750")]),
        )]);
        match assess(&p, Some(&record())) {
            DriftVerdict::Drifted(items) => {
                assert_eq!(items.len(), 1);
                assert_eq!(items[0].detail, "mode: 777 → 0750");
                assert!(items[0].known, "这条资源上次部署时就在");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_newly_declared_resource_is_flagged_as_not_previously_known() {
        // blueprint 加了新资源 ≠ 目标机漂了 —— 报告要分得清。
        let p = plan_of(vec![item("service", Change::Create(vec![]))]);
        match assess(&p, Some(&record())) {
            DriftVerdict::Drifted(items) => assert!(!items[0].known),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn unknowns_prevent_claiming_in_sync() {
        // 有说不清的项时,不能断言"一切正常" —— 那是假的安心。
        let p = plan_of(vec![
            item("file", Change::Ok),
            item("shell", Change::Unknown("无 check".into())),
        ]);
        let v = assess(&p, Some(&record()));
        assert!(matches!(v, DriftVerdict::Indeterminate { unknown: 1, .. }), "{v:?}");
        assert!(v.is_drifted(), "说不清也要促使人去看");
    }

    #[test]
    fn records_round_trip_through_the_file_store() {
        let d = tempfile::tempdir().unwrap();
        let store = FileStore::new(d.path());
        let r = record();
        assert!(store.load(&r.id).unwrap().is_none());
        store.save(&r).unwrap();
        assert_eq!(store.load(&r.id).unwrap().unwrap(), r);
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(store.remove(&r.id).unwrap());
        assert!(!store.remove(&r.id).unwrap(), "删第二次应返回 false 而非报错");
    }

    #[test]
    fn stored_records_are_human_readable_yaml() {
        // 记录要能 diff、能进 git、能被人看懂 —— 这是选文件而非二进制的理由。
        let text = encode(&record());
        assert!(text.contains("blueprint: demo"), "{text}");
        assert!(text.contains("applied_at: 1700000000"), "{text}");
        assert!(text.contains("mode: '750'") || text.contains("mode: \"750\""), "{text}");
    }

    #[test]
    fn ids_with_slashes_and_at_signs_become_safe_filenames() {
        let d = tempfile::tempdir().unwrap();
        let store = FileStore::new(d.path());
        let mut r = record();
        r.id = "k8s-ha@root@10.0.0.5:22/x".into();
        store.save(&r).unwrap();
        assert_eq!(store.load(&r.id).unwrap().unwrap().id, r.id);
    }

    #[test]
    fn one_corrupt_record_does_not_break_listing() {
        let d = tempfile::tempdir().unwrap();
        let store = FileStore::new(d.path());
        store.save(&record()).unwrap();
        std::fs::write(d.path().join("broken.yaml"), "{{{ not yaml").unwrap();
        assert_eq!(store.list().unwrap().len(), 1, "坏的那条跳过,好的仍要列出来");
    }

    #[test]
    fn saving_is_atomic_leaving_no_temp_files_behind() {
        let d = tempfile::tempdir().unwrap();
        let store = FileStore::new(d.path());
        store.save(&record()).unwrap();
        let leftovers: Vec<_> = std::fs::read_dir(d.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().to_string_lossy().ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "留下了临时文件");
    }
}
