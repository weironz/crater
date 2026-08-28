//! 内建资源类型目录(L1)——**lint 期**的参数契约表。
//!
//! 清单不照抄 ansible 模块表,而是按**类型化层次**拟(两块试金石的裁定 C/E):
//! 凡是反复出现的"仪式型 shell + 手写 check"都升为类型 —— `swapoff -a` + 改 fstab
//! (旧写法 2 步 2 个 check)变成 `swap: {state: disabled, persist: true}`,
//! systemd unit 不再是 `copy` 一坨 INI 文本(不可校验、不可字段级 diff)。
//!
//! 这里只登记**形状**(参数名 + 必填性),真正的五动词实现在执行层。有了它,
//! 拼错参数名在 lint 期就报,而不是 Ansible 那样等到跑到那一行。

/// 一个内建类型的参数形状。
pub struct BuiltinType {
    pub name: &'static str,
    /// 至少要出现其中一个(空 = 无此约束)。
    pub one_of: &'static [&'static str],
    pub required: &'static [&'static str],
    pub optional: &'static [&'static str],
    /// 无参数的自由形式短写法(`- shell: "cmd"`)映射到哪个字段。
    pub freeform: Option<&'static str>,
}

macro_rules! t {
    ($name:literal, one_of: $oneof:expr, req: $req:expr, opt: $opt:expr, free: $free:expr) => {
        BuiltinType { name: $name, one_of: &$oneof, required: &$req, optional: &$opt, freeform: $free }
    };
}

/// 内建类型表。顺序即文档顺序。
pub static BUILTINS: &[BuiltinType] = &[
    // —— 文件与内容 ——
    t!("file", one_of: [], req: ["path", "state"], opt: ["mode", "owner", "group"], free: None),
    t!("copy", one_of: ["content", "src", "material"], req: ["dest"],
       opt: ["mode", "owner", "group"], free: None),
    t!("template", one_of: ["material", "src"], req: ["dest"], opt: ["mode", "owner", "group"], free: None),
    t!("lineinfile", one_of: [], req: ["path", "line"], opt: ["regexp", "state", "create"], free: None),
    t!("unarchive", one_of: ["material", "from"], req: ["to"], opt: ["strip", "creates"], free: None),

    // —— 服务与主机基线(裁定 E:仪式型 shell 升类型)——
    t!("systemd_unit", one_of: ["from_material", "exec_start"], req: ["name"],
       opt: ["description", "after", "wants", "environment_file", "restart", "restart_sec",
             "limits", "dropins", "wanted_by", "type"], free: None),
    t!("service", one_of: [], req: ["name"], opt: ["state", "enabled"], free: None),
    t!("hostname", one_of: [], req: ["name"], opt: [], free: Some("name")),
    t!("swap", one_of: [], req: ["state"], opt: ["persist"], free: Some("state")),
    t!("kernel_modules", one_of: [], req: ["load"], opt: ["persist"], free: Some("load")),
    t!("sysctl", one_of: ["from_material", "set"], req: [], opt: ["reload"], free: None),
    t!("user", one_of: [], req: ["name"], opt: ["state", "system", "shell", "home", "groups"], free: None),
    t!("group", one_of: [], req: ["name"], opt: ["state", "system"], free: None),
    t!("mount", one_of: [], req: ["path", "src", "fstype"], opt: ["opts", "state", "persist"], free: None),
    t!("cron", one_of: [], req: ["name", "job"], opt: ["schedule", "user", "state"], free: None),

    // —— 包与容器 ——
    t!("package", one_of: ["packages", "material"], req: [], opt: ["state"], free: None),
    t!("image_present", one_of: ["material", "materials"], req: [],
       opt: ["namespace", "runtime"], free: None),
    t!("container", one_of: [], req: ["name"],
       opt: ["image", "state", "ports", "volumes", "env", "command", "args",
             "restart_policy", "runtime"], free: None),

    // —— 健康探针(health: 段;恒只读,verify 与 drift 共用)——
    t!("http", one_of: [], req: ["url"], opt: ["status", "method", "insecure"], free: Some("url")),
    t!("port_open", one_of: [], req: ["port"], opt: ["host"], free: Some("port")),
    t!("service_active", one_of: [], req: ["name"], opt: [], free: Some("name")),
    // `cmd` 是**结构化命令**(D-117 §3.4):argv 固定头 + flags 有序条目,
    // 条件是**条目的属性**而非字符串里的三元。argv 直达 execve、不过 shell,
    // 注入与引号事故一并根治。`run:`(自由字符串)保留给只读探针位。
    t!("cmd", one_of: ["argv", "run"], req: [], opt: ["flags", "creates", "env", "expect", "chdir"], free: Some("run")),

    // —— 过程性原语(procedure 内为主)——
    t!("shell", one_of: [], req: ["cmd"], opt: ["check", "env", "chdir", "creates"], free: Some("cmd")),
    t!("wait", one_of: ["port", "path"], req: [], opt: ["host", "state", "timeout", "delay"], free: None),
];

pub fn builtin(name: &str) -> Option<&'static BuiltinType> {
    BUILTINS.iter().find(|t| t.name == name)
}

pub fn is_builtin(name: &str) -> bool {
    builtin(name).is_some()
}

/// 拼写纠错:给未知类型名找最接近的内建名(Levenshtein ≤ 2)。
pub fn suggest(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .map(|t| (t.name, distance(name, t.name)))
        .filter(|&(_, d)| d <= 2)
        .min_by_key(|&(_, d)| d)
        .map(|(n, _)| n)
}

fn distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_has_the_types_the_touchstones_demanded() {
        // rustfs 裁定 C、k8s 裁定 E:这些是"仪式型 shell 升类型"的实证清单。
        for t in ["systemd_unit", "swap", "kernel_modules", "sysctl", "hostname", "image_present"] {
            assert!(is_builtin(t), "缺内建类型 {t}");
        }
    }

    #[test]
    fn suggests_close_misspellings() {
        assert_eq!(suggest("servce"), Some("service"));
        assert_eq!(suggest("fil"), Some("file"));
        assert_eq!(suggest("totally_unrelated_thing"), None);
    }

    #[test]
    fn freeform_shorthand_only_where_meaningful() {
        assert_eq!(builtin("shell").unwrap().freeform, Some("cmd"));
        assert_eq!(builtin("copy").unwrap().freeform, None);
    }

    #[test]
    fn no_duplicate_type_names() {
        let mut seen = std::collections::BTreeSet::new();
        for t in BUILTINS {
            assert!(seen.insert(t.name), "重复类型 {}", t.name);
        }
    }
}
