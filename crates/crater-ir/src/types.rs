//! L1 内建资源类型的**注册表** —— 可发现性三件套的唯一真源。
//!
//! `crater types`(CLI 字段卡)、JSON Schema、lint 报错三者全部从这张表生成,
//! 因此**永不互相矛盾**。这不是"顺便加的文档",这张表就是文档本身:
//! 作者此前想知道 `systemd_unit` 支持哪些参数,只能读 Rust 源码 —— 那是本项目
//! 最真实的可用性缺口。
//!
//! 清单不照抄 ansible 模块表,而是按**类型化层次**拟:凡是反复出现的
//! "仪式型 shell + 手写 check"都升为类型 —— `swapoff -a` 加改 fstab(旧写法两步
//! 两个 check)变成 `swap: {state: disabled, persist: true}`;systemd unit 不再是
//! `copy` 一坨 INI 文本(不可校验、不可字段级 diff)。
//!
//! 真正的五动词实现在 [`crate::builtins`];这里只登记**形状与说明**。
//! 两者的差距由 [`crate::builtins::pending`] 显式列出,有测试守着不许悄悄发散。

/// 字段的必选性。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Req {
    /// 必须出现。
    Required,
    /// 可选;不写即"不管理这一维度"(而不是"用某个默认值")—— 这个区别很重要:
    /// `service` 不写 `enabled` 表示**不碰**开机自启,不是把它设成 false。
    Optional,
    /// 与同组其它字段**恰择其一**。组名用于报错时说清是哪一组。
    OneOf(&'static str),
}

/// 字段的类型。schema 与报错都据此渲染。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    Str,
    Int,
    Bool,
    /// 字符串列表。
    List,
    /// 键值表。
    Map,
    /// 封闭枚举,取值见 [`Field::values`]。
    Enum,
    /// 文件权限:必须写成带引号的八进制字符串(`"0755"`)——
    /// 不带引号会被 YAML 当十进制整数,是经典脚枪。
    Mode,
    /// 引用一个 `materials:` 里声明过的名字(schema 可枚举本蓝图的物料名)。
    Material,
    /// 路径。
    Path,
}

impl Ty {
    pub fn label(self) -> &'static str {
        match self {
            Ty::Str => "string",
            Ty::Int => "int",
            Ty::Bool => "bool",
            Ty::List => "list",
            Ty::Map => "map",
            Ty::Enum => "enum",
            Ty::Mode => "mode",
            Ty::Material => "material",
            Ty::Path => "path",
        }
    }
}

/// 一个字段的完整说明。
#[derive(Debug, Clone, Copy)]
pub struct Field {
    pub name: &'static str,
    pub ty: Ty,
    pub req: Req,
    /// 枚举取值(仅 `Ty::Enum`)。
    pub values: &'static [&'static str],
    /// 一句话说明。写给**人**看,会出现在 CLI 字段卡与编辑器悬停里。
    pub doc: &'static str,
}

/// 类型的归类,决定它能出现在哪些段落。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// 期望态资源:可出现在 `resources:`,五动词齐全。
    Resource,
    /// 只读探针:可出现在 `preflight:` / `health:` / 自定义类型的 `observe:`。
    /// **绝不改变目标** —— plan 的可信度押在这条纪律上。
    Probe,
    /// 过程性原语:主要住在 `procedures:` 的步骤里。
    Procedural,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Resource => "期望态资源",
            Kind::Probe => "只读探针",
            Kind::Procedural => "过程性原语",
        }
    }
}

#[derive(Debug)]
pub struct BuiltinType {
    pub name: &'static str,
    pub kind: Kind,
    /// 一句话:这个类型管什么。
    pub doc: &'static str,
    pub fields: &'static [Field],
    /// 无参数的自由形式短写法(`- shell: "cmd"`)映射到哪个字段。
    pub freeform: Option<&'static str>,
    /// 补充说明:传导语义、纪律、常见误解。写给读字段卡的人。
    pub note: Option<&'static str>,
    pub see_also: &'static [&'static str],
}

impl BuiltinType {
    pub fn required(&self) -> Vec<&'static str> {
        self.pick(|r| r == Req::Required)
    }
    pub fn optional(&self) -> Vec<&'static str> {
        self.pick(|r| r == Req::Optional)
    }
    /// 所有 one_of 组的字段(不分组;分组信息见 [`one_of_groups`](Self::one_of_groups))。
    pub fn one_of(&self) -> Vec<&'static str> {
        self.pick(|r| matches!(r, Req::OneOf(_)))
    }
    /// 按组名聚合的互斥组。
    pub fn one_of_groups(&self) -> Vec<(&'static str, Vec<&'static str>)> {
        let mut groups: Vec<(&'static str, Vec<&'static str>)> = Vec::new();
        for f in self.fields {
            if let Req::OneOf(g) = f.req {
                match groups.iter_mut().find(|(name, _)| *name == g) {
                    Some((_, members)) => members.push(f.name),
                    None => groups.push((g, vec![f.name])),
                }
            }
        }
        groups
    }
    pub fn field(&self, name: &str) -> Option<&'static Field> {
        self.fields.iter().find(|f| f.name == name)
    }
    /// 全部合法字段名。
    pub fn field_names(&self) -> Vec<&'static str> {
        self.fields.iter().map(|f| f.name).collect()
    }
    fn pick(&self, want: impl Fn(Req) -> bool) -> Vec<&'static str> {
        self.fields.iter().filter(|f| want(f.req)).map(|f| f.name).collect()
    }
}

// ---------------------------------------------------------------- 构造宏

macro_rules! f {
    ($name:literal, $ty:ident, req, $doc:literal) => {
        Field { name: $name, ty: Ty::$ty, req: Req::Required, values: &[], doc: $doc }
    };
    ($name:literal, $ty:ident, opt, $doc:literal) => {
        Field { name: $name, ty: Ty::$ty, req: Req::Optional, values: &[], doc: $doc }
    };
    ($name:literal, $ty:ident, one_of $g:literal, $doc:literal) => {
        Field { name: $name, ty: Ty::$ty, req: Req::OneOf($g), values: &[], doc: $doc }
    };
    ($name:literal, enum [$($v:literal),*], $r:ident, $doc:literal) => {
        Field { name: $name, ty: Ty::Enum, req: Req::$r, values: &[$($v),*], doc: $doc }
    };
}

macro_rules! t {
    ($name:literal, $kind:ident, $doc:literal, [$($f:expr),* $(,)?]
     $(, free: $free:literal)? $(, note: $note:literal)? $(, see: [$($s:literal),*])? $(,)?) => {
        BuiltinType {
            name: $name,
            kind: Kind::$kind,
            doc: $doc,
            fields: &[$($f),*],
            freeform: { #[allow(unused_mut, unused_assignments)] let mut v = None; $(v = Some($free);)? v },
            note: { #[allow(unused_mut, unused_assignments)] let mut v = None; $(v = Some($note);)? v },
            see_also: &[$($($s),*)?],
        }
    };
}

/// 内建类型表。顺序即 `crater types` 的展示顺序。
pub static BUILTINS: &[BuiltinType] = &[
    // ---------------- 文件与内容 ----------------
    t!("file", Resource, "路径的期望态:目录 / 存在 / 不存在,含权限属主", [
        f!("path", Path, req, "目标路径"),
        f!("state", enum ["directory", "touch", "absent"], Required, "directory=目录;touch=文件存在;absent=不存在"),
        f!("mode", Mode, opt, "权限,带引号八进制如 \"0750\";不写 = 不管理权限"),
        f!("owner", Str, opt, "属主"),
        f!("group", Str, opt, "属组"),
    ], see: ["copy", "unarchive"]),

    t!("copy", Resource, "把内容放到目标路径;幂等靠内容寻址(sha256)而非时间戳", [
        f!("dest", Path, req, "目标路径"),
        f!("content", Str, one_of "来源", "内联文本,可含 ${} 插值"),
        f!("src", Path, one_of "来源", "控制端文件(相对 blueprint 目录)"),
        f!("material", Material, one_of "来源", "materials: 里声明的物料名"),
        f!("mode", Mode, opt, "权限,带引号八进制"),
        f!("owner", Str, opt, "属主"),
        f!("group", Str, opt, "属组"),
    ], note: "三种来源恰择其一。用 material 时,若物料声明了 sha256 或是本地文件,\
              plan 期即可内容寻址比对;远端且无 sha256 则只能退回\"上游变没变\"的粗判据。",
      see: ["template", "file"]),

    t!("template", Resource, "渲染一份模板物料到目标路径(纯替换,无条件无循环)", [
        f!("dest", Path, req, "目标路径"),
        f!("material", Material, one_of "来源", "作为模板的物料名"),
        f!("src", Path, one_of "来源", "控制端模板文件"),
        f!("mode", Mode, opt, "权限,带引号八进制"),
        f!("owner", Str, opt, "属主"),
        f!("group", Str, opt, "属组"),
    ], note: "模板只做替换 —— 条件与循环是藏逻辑的第一现场,一律不支持。\
              需要结构化生成请用 file 的键值形式或 each:。",
      see: ["copy"]),

    t!("lineinfile", Resource, "确保某一行在文件里存在或不存在", [
        f!("path", Path, req, "目标文件"),
        f!("line", Str, req, "期望的整行内容"),
        f!("regexp", Str, opt, "匹配待替换行的正则;不写则按整行字面匹配"),
        f!("state", enum ["present", "absent"], Optional, "默认 present"),
        f!("create", Bool, opt, "文件不存在时是否创建,默认 false"),
    ], note: "退役时只删这一行,不删整个文件 —— 那往往是别人的文件。"),

    t!("unarchive", Resource, "把归档展开到目标目录", [
        f!("to", Path, req, "展开到哪个目录"),
        f!("material", Material, one_of "来源", "归档物料名"),
        f!("from", Path, one_of "来源", "目标机上已有的归档路径"),
        f!("strip", Int, opt, "tar --strip-components"),
        f!("creates", Path, opt, "幂等探针:此路径存在即视为已展开"),
    ], note: "不写 creates 就无法判断是否已展开,plan 会报 `?说不清`、每次都重跑 —— \
              引擎不猜\"哪个文件代表展开成功\",那是产品知识,必须由作者声明。\
              退役只删 creates 指的产物:to 常是 /usr/local 这类共享目录,整个删会连累别人。"),

    // ---------------- 服务与主机基线 ----------------
    t!("systemd_unit", Resource, "安装 systemd unit 文件(自动 daemon-reload)", [
        f!("name", Str, req, "unit 名,可省 .service 后缀"),
        f!("from_material", Material, one_of "来源", "整份 unit 来自物料"),
        f!("exec_start", Str, one_of "来源", "由字段渲染 unit 时的 ExecStart"),
        f!("description", Str, opt, "[Unit] Description"),
        f!("after", List, opt, "[Unit] After"),
        f!("wants", List, opt, "[Unit] Wants"),
        f!("environment_file", Path, opt, "[Service] EnvironmentFile;自动加 - 前缀(缺文件不报错)"),
        f!("restart", Str, opt, "[Service] Restart,如 on-failure"),
        f!("restart_sec", Int, opt, "[Service] RestartSec"),
        f!("limits", Map, opt, "[Service] Limit* 如 {NOFILE: 1048576}"),
        f!("dropins", List, opt, "额外落到 <name>.service.d/ 的物料名"),
        f!("wanted_by", Str, opt, "[Install] WantedBy,默认 multi-user.target"),
        f!("type", Str, opt, "[Service] Type"),
    ], note: "字段化而非 copy 一坨 INI:能说出\"只有 ExecStart 变了\",\
              且只比对我们写的那几项 —— 目标机上别人手加的 MemoryMax= 不算漂移。",
      see: ["service"]),

    t!("service", Resource, "systemd 服务的运行态与开机自启", [
        f!("name", Str, req, "服务名,可省 .service 后缀"),
        f!("state", enum ["started", "stopped", "restarted"], Optional, "不写 = 不管理运行态"),
        f!("enabled", Bool, opt, "开机自启;不写 = 不管理"),
    ], note: "**传导**:在本条之前声明的 file/copy/template/systemd_unit/unarchive 若发生变更,\
              本服务会被判定需要重启 —— 作者不写 notify,也就不会因为忘写而漏重启。\
              `restarted` 表示每次都重启,永远不是 noop;想保持运行写 `started`。",
      see: ["systemd_unit"]),

    t!("hostname", Resource, "目标主机名(kubeadm 等按 OS hostname 认节点)", [
        f!("name", Str, req, "期望的主机名,常写 ${substrate.name}"),
    ], free: "name",
      note: "substrate.name 是 inventory 里的名字(身份);substrate.hostname 是当前 OS 主机名(现实)。\
              退役时不动主机名 —— 没有\"原来的名字\"可以改回去。"),

    t!("timezone", Resource, "系统时区", [
        f!("name", Str, req, "IANA 时区名,如 Asia/Shanghai;`timedatectl list-timezones` 可列全"),
    ], free: "name",
      note: "时区只影响**显示**,不影响时间本身 —— 真正影响 k8s 的是时钟是否同步,那是 time_sync 管的。\
              但机群时区不一致会让跨机对日志变成一件苦差事,所以它值得是期望态而不是一条命令。\
              退役时不动时区:没有\"原来的时区\"可以改回去。",
      see: ["time_sync"]),

    t!("time_sync", Resource, "时钟同步(NTP)", [
        f!("enabled", Bool, opt, "是否启用 NTP 同步,默认 true"),
        f!("servers", List, opt, "NTP 服务器;留空用系统默认(境内建议 ntp.aliyun.com)"),
        f!("wait", Bool, opt, "apply 时等到真正同步上再返回(最多 60s),默认 false"),
    ],
      note: "\"启用了同步\"和\"已经同步上\"是两件事,本类型分别观察 NTP 与 NTPSynchronized。\
              只看前者会让一台刚开机、时钟还差几百毫秒的机器显示为绿灯 —— 而 etcd 的仲裁\
              和证书有效期恰恰吃这几百毫秒。跨主机比时间戳的一切判据,精度下限都是它。\
              实现走 systemd-timesyncd(Ubuntu/Debian 默认);退役时不关同步。",
      see: ["timezone"]),

    t!("swap", Resource, "交换分区的启用状态(k8s 要求关闭)", [
        f!("state", enum ["disabled", "enabled"], Required, "期望状态"),
        f!("persist", Bool, opt, "同时注释掉 /etc/fstab 里的 swap 条目,默认 true"),
    ], free: "state",
      note: "只 swapoff 不改 fstab 是经典陷阱:重启后 swap 自己回来、k8s 再次拒绝启动。\
              persist 让这一项在 plan 里**看得见**。退役时不擅自开回来 —— 原来什么样我们并不知道。"),

    t!("kernel_modules", Resource, "内核模块加载与持久化", [
        f!("load", List, req, "要加载的模块名列表"),
        f!("persist", Bool, opt, "写入 /etc/modules-load.d/,默认 true"),
    ], free: "load",
      note: "退役只撤持久化,不 rmmod —— 别的东西可能正用着这些模块。"),

    t!("sysctl", Resource, "内核参数", [
        f!("set", Map, one_of "来源", "键值表,如 {net.ipv4.ip_forward: 1}"),
        f!("from_material", Material, one_of "来源", "整份 sysctl 配置来自物料"),
        f!("reload", Bool, opt, "写完是否 sysctl --system,默认 true"),
    ], note: "逐键比对并逐键报差异;键在本机不存在(如模块未加载)时读作\"(未设置)\",\
              不会把探针的 stderr 当成当前值。"),

    t!("user", Resource, "系统用户", [
        f!("name", Str, req, "用户名"),
        f!("state", enum ["present", "absent"], Optional, "默认 present"),
        f!("system", Bool, opt, "系统用户(--system)"),
        f!("uid", Int, opt, "固定 uid;不写则由系统分配"),
        f!("group", Str, opt, "主组名(-g);不写则由系统按发行版惯例决定"),
        f!("shell", Path, opt, "登录 shell"),
        f!("home", Path, opt, "家目录"),
        f!("groups", List, opt, "附加组(-G)"),
    ],
      note: "`uid` / `gid` 值得固定:数据盘在机器间搬动、备份归档跨机还原时,\
              属主靠的是数字而不是名字 —— 系统分配的 uid 各机不同,搬过去就成了\
              一堆 `nobody`。主组用 `group:`(单数),附加组用 `groups:`(复数)。",
      see: ["group"]),

    t!("group", Resource, "系统组", [
        f!("name", Str, req, "组名"),
        f!("state", enum ["present", "absent"], Optional, "默认 present"),
        f!("system", Bool, opt, "系统组"),
        f!("gid", Int, opt, "固定 gid;不写则由系统分配"),
    ], see: ["user"]),

    t!("mount", Resource, "挂载点", [
        f!("path", Path, req, "挂载点"),
        f!("src", Str, req, "设备或源"),
        f!("fstype", Str, req, "文件系统类型"),
        f!("opts", Str, opt, "挂载选项"),
        f!("state", enum ["mounted", "unmounted", "absent"], Optional, "默认 mounted"),
        f!("persist", Bool, opt, "写入 /etc/fstab"),
    ]),

    t!("cron", Resource, "定时任务", [
        f!("name", Str, req, "任务标识(注释行,用于幂等)"),
        f!("job", Str, req, "要执行的命令"),
        f!("schedule", Str, opt, "cron 表达式,默认每日"),
        f!("user", Str, opt, "以哪个用户运行"),
        f!("state", enum ["present", "absent"], Optional, "默认 present"),
    ]),

    // ---------------- 包与容器 ----------------
    t!("package", Resource, "OS 包(按 family 分叉:引擎只懂怎么装,装什么是数据)", [
        f!("packages", Map, one_of "来源", "按 family 的包名表 {debian: [...], rhel: [...]}"),
        f!("material", Material, one_of "来源", "os_package 类物料(离线闭包)"),
        f!("state", enum ["present", "absent"], Optional, "默认 present"),
    ]),

    t!("image_present", Resource, "容器镜像已在目标运行时里", [
        f!("material", Material, one_of "来源", "单个镜像物料"),
        f!("materials", List, one_of "来源", "一批镜像物料名"),
        f!("namespace", Str, opt, "ctr/nerdctl 命名空间,如 k8s.io"),
        f!("runtime", Str, opt, "运行时 CLI,默认 docker"),
    ], note: "退役不删镜像:别的负载可能正用着,重新拉取代价也高。"),

    t!("container", Resource, "目标运行时上的一个容器", [
        f!("name", Str, req, "容器名"),
        f!("image", Str, opt, "镜像引用;state: started 时必需"),
        f!("state", enum ["started", "stopped", "absent"], Optional, "默认 started"),
        f!("ports", List, opt, "端口映射,docker -p 语法"),
        f!("volumes", List, opt, "挂载,docker -v 语法"),
        f!("env", Map, opt, "环境变量"),
        f!("command", Str, opt, "镜像**之后**的参数 —— 给容器内的程序(如 --config.file=…)"),
        f!("args", List, opt, "镜像**之前**的 docker run 标志(逃生舱,如 --cap-add=…)"),
        f!("restart_policy", Str, opt, "--restart"),
        f!("runtime", Str, opt, "docker 兼容 CLI,默认 docker"),
    ]),

    // ---------------- 过程性原语 ----------------
    t!("cmd", Procedural, "结构化命令:argv 直达 execve,条件是 flag 条目的属性", [
        f!("argv", List, one_of "形态", "命令与固定参数,如 [kubeadm, init]"),
        f!("run", Str, one_of "形态", "自由字符串(过 shell,可用管道);仅建议用于只读探针"),
        f!("flags", List, opt, "有序 flag 条目:{name, value?, when?}"),
        f!("creates", Path, opt, "幂等探针:此路径存在即跳过"),
        f!("env", Map, opt, "环境变量;探针与命令共享同一套"),
        f!("expect", Int, opt, "期望退出码,默认 0"),
        f!("chdir", Path, opt, "工作目录"),
    ], free: "run",
      note: "**按出现位置有两套用法**:动作位(procedures 步骤 / resources)用 argv+flags,\
              可带 creates 幂等护栏;探针位(preflight / health / 自定义类型的 observe)\
              用 run 或 argv,只读纪律由作者保证 —— 在探针位写会改变目标的命令,会让 plan 说谎。\
              \n**条件 flag 的正规写法**:flags 条目带 when,条件为假的条目根本不出现,\
              不留空串占位 —— 因此没有写三元表达式的动机(那会被 E310 拒绝)。\
              flags[].name 禁插值,于是 lint 能静态枚举这条命令的全部展开形态。\
              argv 逐 token 转义,注入与引号事故一并根治。",
      see: ["shell"]),

    t!("shell", Procedural, "逃生舱:一条自由 shell 命令", [
        f!("cmd", Str, req, "命令行(过 shell)"),
        f!("check", Str, opt, "幂等探针:退出 0 即视为已完成"),
        f!("env", Map, opt, "环境变量;check 与 cmd 共享"),
        f!("chdir", Path, opt, "工作目录"),
        f!("creates", Path, opt, "幂等探针:此路径存在即跳过"),
    ], free: "cmd",
      note: "没有 check 就无法预演,plan 显示 `?` 并计入\"模型化欠债\" —— 接住,不羞辱,但可见。\
              退役无逆操作(返回 warn);想要退役行为请写进 procedure。\
              能用 cmd 的结构化形态就别用它。",
      see: ["cmd"]),

    t!("wait", Procedural, "阻塞直到条件满足(只读:它改变不了任何东西)", [
        f!("port", Int, one_of "对象", "TCP 端口"),
        f!("path", Path, one_of "对象", "路径"),
        f!("host", Str, opt, "配合 port,默认 127.0.0.1"),
        f!("state", enum ["started", "stopped", "present", "absent"], Optional, "期望方向,默认 started/present"),
        f!("timeout", Int, opt, "秒,默认 30"),
        f!("delay", Int, opt, "首次探测前等待秒数"),
    ], note: "等待成功报 ok 而非 changed —— 它没有改变世界。"),

    // ---------------- 只读探针 ----------------
    t!("http", Probe, "HTTP 状态码探针", [
        f!("url", Str, req, "目标 URL"),
        f!("status", Int, opt, "期望状态码,默认 200"),
        f!("method", Str, opt, "HTTP 方法"),
        f!("insecure", Bool, opt, "跳过 TLS 校验"),
    ], free: "url"),

    t!("port_open", Probe, "端口可连通探针", [
        f!("port", Int, req, "端口"),
        f!("host", Str, opt, "默认 127.0.0.1"),
    ], free: "port"),

    t!("service_active", Probe, "systemd 服务正在运行", [
        f!("name", Str, req, "服务名"),
    ], free: "name"),
];

pub fn builtin(name: &str) -> Option<&'static BuiltinType> {
    BUILTINS.iter().find(|t| t.name == name)
}

pub fn is_builtin(name: &str) -> bool {
    builtin(name).is_some()
}

/// 拼写纠错:给未知类型名找最接近的内建名(编辑距离 ≤2)。
pub fn suggest(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .map(|t| (t.name, distance(name, t.name)))
        .filter(|&(_, d)| d <= 2)
        .min_by_key(|&(n, d)| (d, n.len()))
        .map(|(n, _)| n)
}

/// 某个类型里与 `field` 最接近的合法字段名。
pub fn suggest_field(ty: &str, field: &str) -> Option<&'static str> {
    let b = builtin(ty)?;
    b.field_names()
        .into_iter()
        .map(|f| (f, distance(field, f)))
        .filter(|&(_, d)| d <= 2)
        .min_by_key(|&(f, d)| (d, f.len()))
        .map(|(f, _)| f)
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
        for t in ["systemd_unit", "swap", "kernel_modules", "sysctl", "hostname", "image_present",
                  "timezone", "time_sync"] {
            assert!(is_builtin(t), "缺内建类型 {t}");
        }
    }

    #[test]
    fn every_type_and_field_carries_a_human_readable_doc() {
        // 这张表就是文档 —— 空的说明等于把作者推回去读源码。
        for t in BUILTINS {
            assert!(!t.doc.is_empty(), "类型 {} 缺说明", t.name);
            for f in t.fields {
                assert!(!f.doc.is_empty(), "{}.{} 缺说明", t.name, f.name);
            }
        }
    }

    #[test]
    fn enum_fields_enumerate_their_values() {
        for t in BUILTINS {
            for f in t.fields {
                assert_eq!(
                    f.ty == Ty::Enum,
                    !f.values.is_empty(),
                    "{}.{}:enum 必须列出取值,非 enum 不该有取值",
                    t.name,
                    f.name
                );
            }
        }
    }

    #[test]
    fn one_of_groups_have_at_least_two_members() {
        // 只有一个成员的"互斥组"是笔误 —— 它本该是 Required。
        for t in BUILTINS {
            for (g, members) in t.one_of_groups() {
                assert!(members.len() >= 2, "{}:互斥组 `{g}` 只有 {members:?}", t.name);
            }
        }
    }

    #[test]
    fn freeform_shorthand_points_at_a_real_field() {
        for t in BUILTINS {
            if let Some(ff) = t.freeform {
                assert!(t.field(ff).is_some(), "{}:短写法指向不存在的字段 {ff}", t.name);
            }
        }
    }

    #[test]
    fn see_also_never_dangles() {
        for t in BUILTINS {
            for s in t.see_also {
                assert!(is_builtin(s), "{}:see_also 指向不存在的类型 {s}", t.name);
            }
        }
    }

    #[test]
    fn no_duplicate_type_names_or_field_names() {
        let mut seen = std::collections::BTreeSet::new();
        for t in BUILTINS {
            assert!(seen.insert(t.name), "重复类型 {}", t.name);
            let mut fields = std::collections::BTreeSet::new();
            for f in t.fields {
                assert!(fields.insert(f.name), "{}:重复字段 {}", t.name, f.name);
            }
        }
    }

    #[test]
    fn suggests_close_misspellings_for_types_and_fields() {
        assert_eq!(suggest("servce"), Some("service"));
        assert_eq!(suggest("fil"), Some("file"));
        assert_eq!(suggest("totally_unrelated_thing"), None);
        assert_eq!(suggest_field("service", "stat"), Some("state"));
        assert_eq!(suggest_field("file", "pth"), Some("path"));
    }

    #[test]
    fn required_optional_and_one_of_partition_the_fields() {
        for t in BUILTINS {
            let n = t.required().len() + t.optional().len() + t.one_of().len();
            assert_eq!(n, t.fields.len(), "{}:字段必选性分类有遗漏", t.name);
        }
    }
}
