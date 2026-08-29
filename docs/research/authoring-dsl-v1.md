# crater 作者层 DSL v1 规范
#
# 产出方式:5 个互相隔离的白纸设计(Fable 5,五种设计哲学:ansible-empathy /
# reader-first / toolability-first / minimal-core / wildcard)→ 3 评委横向对比
# (可用性走查 / 语言严谨性敌意审查 / 3am 事故演练+工具链)→ 综合终稿。
# 骨架 = toolability-first,融合各方案单点最优;评委全部致命伤逐条处置(见 §0.2)。
#
# 主笔审校记(opus,两处保留意见,见文末「审校记」一节):
#   R1: 本规范删除了自由字符串 shell,与 product-design.md 的逃生舱原则存在张力 —— 详见审校记。
#   R2: 与现行实现的语法差异巨大(重写 parse 层),但 IR 不动 —— 这正是「IR 是契约、
#       语法是前端」保险单的兑付,不是返工。

# crater 作者层 DSL v1 规范(终稿草案)

> 状态:终审综合稿。载体:YAML 1.2 严格子集。关键字英文,文档中文。
> 语义模型(IR:observe/diff/apply/destroy/upgrade 五动词、procedure、L2 类型、materials、params、fleet、facts、health、preflight)不变,本稿只钉死人写的那一层。

---

## 0. 骨架裁定与融合总账

### 0.1 骨架:方案 3(toolability-first),理由三条

1. **评委投票 2:1**(newcomer、operator 均裁定方案 3;rigor 裁定方案 1),且 rigor 自己承认:方案 3 在记号层返工之后**无致命伤**,而方案 1 的伤(shell 正门、or 文法、传导未定义、双轨 loop)条条要动刀。
2. **E(字符串逻辑杜绝)与 F(工具化上限)双满分是本引擎的存在性前提,不是风格偏好**。crater 以"零连接 lint、plan 可信、对账可审计"立身,五案中只有方案 3 的骨架(判别类型键 + 结构化条件 + 封闭导出)让 JSON Schema 逼近 lint 全功能、三件套真正同源。
3. **加糖易,拆骨难**。方案 3 的全部短板(击键量、条件啰嗦)是可以往上加糖修的;其余方案的短板(双轨记号、键侧语法、词义重载、虚无律静默、shell 示范)都长在骨骼里。

方案 1 的资产没有丢:与 Ansible **同形同义**的词全部保留(`creates`/`serial`/`when`/`on`/`for_each` 直觉),同形不同义的词全部改名或删除(哲学第 9 条)。

### 0.2 评委致命伤处置表(逐条,无一遗漏)

| # | 致命伤(出处) | 归宿 |
|---|---|---|
| 1 | 方案 3:裸 `${}` 在 YAML flow 语境非法,自家示例是非法 YAML(rigor) | **解决**:插值必须写在双引号字符串内,E101 强制,`crater fmt` 自动修复(§3.7) |
| 2 | 方案 3:`{param: ha, equals: true}` 冗长,写五遍会造反(newcomer/operator) | **解决**:条件原子只有一种拼法——字符串子句微文法;结构层只剩 `all/any/not` 组合子(§3.2) |
| 3 | 方案 3:`{param: ha}` 与 `${params.ha}` 双拼写(rigor) | **解决**:同上,原子即 `params.ha` 子句,单拼写 |
| 4 | 方案 3:升级舞 drain 与 uncordon 分离,cordon 窗口跨轮拉长(rigor/operator) | **解决**:S3 升级舞改为每台节点 drain→upgrade→换件→重启→uncordon 闭环(§4.3) |
| 5 | 方案 3:`run_on` 委托边界未论证(rigor) | **解决**:改名 `via`(方案 1 命名),v1 仅对 `cmd` 动作合法,`${host.*}` 恒指当前主语,lint 强制(§2.9) |
| 6 | 方案 1/2:`shell` 自由字符串 / 管道字符串在必答场景示范(全体评委) | **随拒绝消失**:方言中不存在自由字符串命令;唯一舱门是 `from_fact`(类型准入 + `creates` + plan 高亮)(§3.5) |
| 7 | 方案 1:`when` 带 or 且无括号,优先级歧义(rigor) | **解决**:子句文法只有 `and`;or 只以结构化 `any:` 出现,结构可见即可审(§3.2) |
| 8 | 方案 1:传导语义("上游")未定义(rigor) | **解决**:传导范围、传导源类型、flush-point 规则写进规范(§2.13) |
| 9 | 方案 1:插值识别"三种命运"(合法 ref / 误捕 / 静默透传)(rigor) | **解决**:任何 `${...}` 必须解析为合法引用,否则报错;字面 `${` 用 `$${` 转义,无静默透传(§3.1) |
| 10 | 方案 2/4/5:双轨引用记号(裸引用 vs 插值)(newcomer/rigor) | **随拒绝消失**:单记号 `"${ns.path}"`,整值引用保型(§3.1) |
| 11 | 方案 2/5:主语入键("type subject" 作键名)压垮补全与通用工具(全体) | **随拒绝消失**:资源条目 = 判别类型键,干净 oneOf |
| 12 | 方案 2:`each`/`state` 一词多义(全体) | **随拒绝消失**:`for_each` 只有资源级列表展开一义;逐台动作组 = `serial: 1` + `do:` |
| 13 | 方案 2:selector-map 键序承载执行序、无法交错分组(rigor) | **随拒绝消失**(见 §6),HA 段的重复由条件糖压到可接受 |
| 14 | 方案 4:cmd/shell 概念边界坍塌,只读探针执行副作用(rigor) | **解决**:`cmd` 按出现位置分两个 schema——动作位(可带 `creates`,资源位必带)与探针位(仅 `argv`+`exit`,结构上写不了副作用护栏);报错教边界(§2.7/§2.8) |
| 15 | 方案 4/5:正则 capture 进 export(全体) | **随拒绝消失**:`take: all\|first_line\|last_line` 封闭三选一,无正则(§2.10) |
| 16 | 方案 4:`run` 过程跳读、health 併入 state 靠背规则(operator) | **随拒绝消失**:无 procedure 组合;`health` 独立顶层节 |
| 17 | 方案 5:虚无律——正确缺席与错误缺席同形(全体) | **随拒绝消失**:条件必须显式(`when`);但其补偿机制"plan 解释缺席"升格为规范义务(§5.1) |
| 18 | 方案 5:类型驱动 flag 渲染写不出 `--x=false`、flags map 无序(rigor) | **随拒绝消失**:flags 是有序列表,条件显式 |
| 19 | YAML 脚枪:mode 八进制、sha256 Norway、env 值 on/off(rigor) | **解决**:lint 强制 mode/sha256/env 值为带引号字符串,schema 类型钉死(§3.7) |
| 20 | facts 封闭白名单的长尾抱怨(方案 3 自认) | **不解决,因为**:`facts.*` 永远可静态校验是 lint 零连接的地基;泄压阀 = `cmd` 探针 + `export`,痕迹留在文档里 |
| 21 | 无算术(`port+1` 写不出)(多案自认) | **不解决,因为**:多声明一个参数让契约显式外化,好过表达式长出运算符——运算符是 Jinja 化的第一级台阶 |
| 22 | 模板零逻辑覆盖不了"按机群列表生成 haproxy backend"(多案自认) | **不解决(v1)**,列为开放问题 1:这是已知最大表达力缺口,宁可缺,不引入模板循环 |

### 0.3 采纳的点子(来源标注)

| 点子 | 来源 | 落点 |
|---|---|---|
| 条件子句字符串糖(`params.ha` / `is set` / `==`) | 方案 1/2 | §3.2 |
| `flags[].name` 禁插值 → 命令全展开可静态枚举 | 方案 3 | §3.4 |
| E412 条件导出作用域检查 | 方案 3 | §5.3 |
| `creates` 幂等护栏(Ansible 同形同义) | 方案 3 | §2.8 |
| `from_fact` 类型门(`type: command` 准入) | 方案 3 | §3.5 |
| `crater lock` lockfile(build 参数 URL 的 sha 后置固化) | 方案 3(方案 5 bake 同理) | §2.3 |
| `via` 命名与 on × via × serial 正交 | 方案 1 | §2.9 |
| `cast` 选角表 | 方案 5 | §2.4 |
| plan 解释缺席(省略了什么、因为什么) | 方案 5(方案 4 亦有) | §5.1 |
| 自特化 schema(补全你自己的参数/角色/物料/本地文件) | 方案 5 | §5.2 |
| `by` 开关表物料变体(判据一个 ref,分支即键,穷尽可查) | 方案 2/5 | §2.3 |
| `port_free` 命名 + `allow_owner` 字段 | 方案 2 命名 + 方案 1 字段 | §2.7 |
| 无 default 即 required | 方案 2 | §2.2 |
| `- step: <名字>` 名字先行 | 方案 2 | §2.9 |
| secret 污点流 lint(禁入 argv/url、警告弱权限文件) | 方案 2/5 | §2.2/§5.3 |
| E201 式"纠正心智模型"报错 | 方案 1 | §5.3 |
| plan 传导箭头(`↳ 传导重启: service caddy`) | 方案 2 | §5.1 |
| `file` 的 `format: env` + `data` 结构化环境文件 | 方案 1/4 | §2.7 |
| preflight `assert` + `reason` | 方案 1 | §2.12 |
| 导出步骤独立于 init(后加节点与首建同舞) | 方案 1/3/4 共识 | §4.3 |
| `params.doc` 字段 | 方案 3 | §2.2 |

---

## 1. 设计哲学(10 条)

1. **语法为静态分析而设计**:每个合法写法可枚举;lint、schema、`crater types`、报错吃同一份类型注册表,永不打架。
2. **结构承载逻辑,字符串只承载数据**:方言中不存在能容纳三元表达式的位置——不是"不许写",是"没地方写"。
3. **名词优先**:resources 声明持续成立的状态;一次性动作只住在 procedure;五动词属于引擎。
4. **顺序即依赖**:声明序执行,传导规则显式定义(§2.13),无 handler/notify。
5. **空集合法**:selector 选中零台 = no-op;单节点拓扑零特判、零分支。
6. **渐进披露**:S1 作者只见五个顶层键;`procedures`/`types`/`export` 不用不见,用到时形状同构。
7. **缺席必须可解释**:任何被条件裁掉的 flag、资源、步骤,plan 必须说明"省略了什么、因为哪个参数"。
8. **逃生舱显式且有围栏**:`from_fact` 与 `cmd` 存在,但有类型准入、幂等护栏,plan 高亮;没有无围栏的舱门。
9. **老兵免税,但不进口债务**:与 Ansible 同形的词必须同义(`creates`/`serial`/`when`/`on`);同形不同义一律改名。
10. **报错是产品**:每条错误给位置、判决、最近似修复、下一步命令,并且优先纠正心智模型而非拼写。

---

## 2. 文档结构与完整字段参考

> 本节表格即未来 `crater types` 与官方文档的种子。列:字段 / 出现位置 / 类型 / 必选 / 默认 / 说明。

### 2.0 文件族

| 文件 | 谁写 | 内容 |
|---|---|---|
| `<name>.blueprint.yaml` | 作者 | 期望态、物料、参数契约、舞、L2 类型 |
| `files/…` | 作者 | 本地附件(unit/模板),随 blueprint 打包 |
| `blueprint.lock.yaml` | `crater lock` 生成 | 含 build 参数 URL 的物料 sha256 固化 |
| `<name>.inventory.yaml` | 部署方 | 机群 + deploy 期参数取值 |

### 2.1 顶层键

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `crater` | 顶层 | int | 是 | — | 方言格式版本,v1 恒为 `1` |
| `blueprint` | 顶层 | string | 是 | — | 蓝图名,制品标识 |
| `version` | 顶层 | string | 否 | — | 蓝图自身版本号 |
| `fleet` | 顶层 | map | 否 | 单机隐式 all | 组契约:`groups.<名>: {min: int}`;`min: 0` 即空组合法 |
| `cast` | 顶层 | map | 否 | `{}` | 选角表:`角色名: <selector 串>`,角色可用于一切 selector 位 |
| `params` | 顶层 | map | 否 | `{}` | 类型化参数契约(§2.2) |
| `materials` | 顶层 | map | 否 | `{}` | 离线闭包物料(§2.3) |
| `preflight` | 顶层 | list | 否 | `[]` | 只读准入断言,任一失败则整个部署不开始(§2.12) |
| `resources` | 顶层 | list | 否 | `[]` | 期望态资源,声明序收敛(§2.6) |
| `health` | 顶层 | list | 否 | `[]` | 只读健康探针,verify 与漂移检测依据(§2.12) |
| `procedures` | 顶层 | map | 否 | `{}` | 名→步骤列表,多主机有序工作流(§2.9) |
| `types` | 顶层 | map | 否 | `{}` | L2 自定义资源类型(§2.11) |

### 2.2 `params.<name>`

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `type` | param 体 | 类型名 | 是 | — | `string / int / bool / ip / cidr / port / version / path / enum / list[string] / list[int]`,封闭文法 |
| `default` | param 体 | 同 type | 否 | — | **无 default 即 required**(方案 2 规则) |
| `required` | param 体 | bool | 否 | 无 default 时 true | `required: false` 且无 default = 参数可处于 unset 态,可被 `is set` 探测 |
| `values` | param 体 | list | enum 时必选 | — | 枚举值域 |
| `secret` | param 体 | bool | 否 | `false` | 全链路打码;污点流 lint:**禁入** argv/flags/url(E413),进入 mode 宽于 0600 的文件告警(W421) |
| `phase` | param 体 | enum | 否 | `deploy` | `build`(bake 时烤进制品,可出现在 materials)或 `deploy`(禁入 materials,E414) |
| `doc` | param 体 | string | 否 | — | 一句话说明,进 `crater params`、补全悬停与报错 |

### 2.3 `materials.<name>`

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `source` | material 体 | map | 与 `path` 二选一 | — | 远程来源:`{url, sha256}` 或 `by` 开关表(下行) |
| `source.by` | source 体 | fact 引用串 | 变体时必选 | — | 变体判据,如 `facts.arch`;其余键即分支,键 = fact 取值,值 = `{url, sha256?}`;目标机取值未覆盖 → plan 期报错,封闭值域 fact 可做穷尽性 lint |
| `source.url` | source/分支体 | string | 是 | — | 可含 `${params.*}`(仅 build 期参数)插值 |
| `source.sha256` | source/分支体 | string | 条件必选 | — | 内容寻址;URL 含 build 参数时可省,由 `crater lock` 写入 lockfile |
| `path` | material 体 | string | 与 source 二选一 | — | 随蓝图走的本地文件,bake 时自动内容寻址 |
| `extract.member` | material 体 | string | 否 | — | 归档(zip/tar)内取某成员,取出后当单文件用 |

便利规则:`copy`/`template`/`systemd_unit` 的 `source: ./files/x` 相对路径自动注册为隐式本地物料。

### 2.4 `cast`

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `<角色名>` | cast 体 | selector 串 | — | — | 单点定义、全篇引用;lint 对照 fleet 校验,角色名可被 schema 补全 |

### 2.5 selector 微语法与 inventory

selector(唯一的定址字符串微语法,文法封闭):

```
selector := "all" | <group> | <group> ".first" | <group> ".rest"
          | "host:" <name> | <cast 角色> | <selector> "(" fact "=" literal ")"
```

`.first` 取组内首台(**inventory 声明序**,稳定可重放);`.rest` 为其余台;空选择 = 合法 no-op。步骤 `on` 可取 selector 列表(有序并集)。

inventory 文件:

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `inventory` | 顶层 | string | 是 | — | 机群名 |
| `hosts.<name>` | 顶层 | map | 是 | — | `{addr: <ip/主机名>, params?: map}` |
| `groups.<name>` | 顶层 | list | fleet 契约要求时必选 | — | 成员主机名列表;声明序即稳定序;`[]` 合法 |
| `params` | 顶层/host 体 | map | 否 | `{}` | deploy 期参数取值 |

**变量优先级仅 5 层**:`params.default` < inventory 全局 `params` < group `params` < host `params` < CLI `-p k=v`。无 set_fact、register、vars_files。

### 2.6 资源条目与元字段

一个资源条目 = **恰好一个类型键**(判别器,schema oneOf 的根)+ 可选元字段:

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `<type>` | 条目 | map | 是 | — | 21 内建类型或 L2 类型之一,值为该类型字段体 |
| `name` | 条目 | string | 否 | 类型+序号 | 人读标签,原样进 plan/报错 |
| `on` | 条目 | selector | 否 | `all` | 定址 |
| `when` | 条目 | 条件(§3.2) | 否 | 恒真 | 假 = 该资源**不存在**(plan 显示省略原因),而非"跑了但跳过" |
| `for_each` | 条目 | 整值列表引用 | 否 | — | 如 `"${params.data_dirs}"`;展开为 N 个独立可对账实例,体内用 `${item}`;不可嵌套 |

### 2.7 内建类型字段表(`crater types` 种子)

**期望态类型**(字段列:字段/类型/必选/默认/说明;出现位置均为该类型体内):

| 类型 | 字段 |
|---|---|
| `file` | **`path`** string 必;`state` enum `file\|directory\|absent` 默认 file;`content` string(state=file);`format` enum `raw\|env` 默认 raw;`data` map(format=env 时必,值 = 字符串 \| `{value, when}` \| `{join, with}`,diff 按键逐条对账);`mode`/`owner`/`group` string 可 |
| `copy` | **`dest`** string 必;`material`(物料名)或 `source`(本地路径)二选一必;`mode`/`owner`/`group` 可 |
| `template` | **`dest`** string 必;`source` 或 `content` 二选一必;`vars` map 可(模板内名字绑定,值可用 `join` 构造器);`mode`/`owner`/`group` 可。模板**只有 `${}` 替换**,无条件无循环 |
| `lineinfile` | **`path`** 必;**`line`** 必;`match` 锚定正则 可(v1 唯一被批准的正则位:匹配文本行,不提取数据,lint 期编译校验);`state` `present\|absent` 默认 present |
| `unarchive` | **`material`** 必;**`dest`** 必;`strip_components` int 默认 0 |
| `systemd_unit` | **`name`** 必;`source` 或 `content` 二选一必;变更自动 daemon-reload |
| `service` | **`name`** 必;`state` enum `running\|stopped` 可,**无默认 = 不管理运行态**;`enabled` bool 可,无默认 = 不管理;`on_change` enum `restart\|reload` 默认 restart。步骤语境额外允许 `state: restarted\|reloaded`(一次性动作);资源语境写它 → E201 |
| `hostname` | **`value`** 必 |
| `swap` | **`state`** enum `enabled\|disabled` 必 |
| `kernel_modules` | **`load`** list 必;`persist` bool 默认 true |
| `sysctl` | **`settings`** map<string,string> 必(值强制带引号字符串) |
| `user` | **`name`** 必;`uid`/`group`/`groups`/`shell`/`home` 可;`system` bool 默认 false;`state` 默认 present |
| `group` | **`name`** 必;`gid` 可;`system` 默认 false;`state` 默认 present |
| `package` | **`name`** string 或 list 必;`state` `present\|absent` 默认 present;`version` 可 |
| `image_present` | **`material`** 必(镜像物料) |
| `cmd`(动作位) | 见 §2.8;**资源位必须带 `creates`**(幂等身份,observe 即"该路径存在") |
| `wait` | **`until`** 探针结构 必;`timeout` duration 默认 60s;`interval` 默认 2s |

**只读探针**(preflight / health / `wait.until` / L2 `observe` 通用;探针通过 = 断言成立):

| 探针 | 字段 |
|---|---|
| `http` | **`url`** 必;`status` int 默认 200;`insecure` bool 默认 false |
| `port_open` | **`port`** 必 |
| `port_free` | **`port`** 必;`allow_owner` string 可 —— "端口空闲,或占用者进程名是自己人"(幂等重跑语义;编译为 port_open 取反 + 归属豁免) |
| `service_active` | **`name`** 必 |
| `cmd`(探针位) | **`argv`** 必;`exit` int 默认 0。**探针位没有** creates/flags/via/env —— 结构上写不了副作用语义 |

### 2.8 `cmd` 结构(动作位;命令不是字符串)

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `argv` | cmd 体 | list[string] | 与 `from_fact` 二选一 | — | 程序与固定 token,逐项字面量(可含插值;以 `-` 开头的 token 含插值 → W310 建议改 flags) |
| `from_fact` | cmd 体(仅步骤位) | 导出名 | 二选一 | — | 以 `type: command` 的跨主机 fact 为 argv 前缀;**必须配 `creates`**;lint 标注 trusted-fact,plan 高亮 |
| `flags` | cmd 体 | list | 否 | `[]` | 结构化旗标,见下 |
| `args` | cmd 体 | list | 否 | `[]` | 尾参:字符串 或 `{value, when}` |
| `env` | cmd 体 | map | 否 | `{}` | 值 = 字符串 或 `{value, when}` |
| `creates` | cmd 体 | path | 资源位必选 | — | 幂等护栏:路径存在 → observe 判"已完成",跳过(Ansible 同形同义) |
| `exit` | cmd 体 | int | 否 | 0 | 期望退出码 |
| `via` | cmd 体(仅步骤位) | selector | 否 | 无 | 命令在 via 主机上执行;`${host.*}` 仍指当前主语(v1 仅 cmd 支持) |

**flag 条目**(只有 block 风格,fmt 强制):

| 字段 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|
| `name` | string 字面量 | 是 | — | **禁止插值**——这使 lint 能静态枚举一条命令的全部展开组合 |
| `value` | 可插值标量 | 否 | — | 缺省即裸 flag;渲染为 `--name value` 两个 argv token |
| `sep` | string | 否 | 空格 | `"="` 时渲染 `--name=value` 单 token |
| `when` | 条件 | 否 | 恒真 | 假 = 该旗标整体不出现,plan 解释缺席 |

### 2.9 procedure 步骤

步骤以 `- step: <名字>` 开头(名字必选、先行、可 grep、可对讲机喊):

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `step` | 步骤头 | string | 是 | — | 步骤名,plan/日志锚点 |
| `on` | 步骤 | selector 或列表 | 否 | `all` | 主语主机集;**空集 = 合法 no-op**(plan 标注跳过原因) |
| `serial` | 步骤 | int | 否 | 0(全并行) | 逐台限流;`serial: 1` 时 `do` 整组按台滚动 |
| `when` | 步骤 | 条件 | 否 | 恒真 | 条件步骤 |
| 动作 | 步骤 | `cmd` / 任一资源类型 / `wait` | 与 `do`/`export` 至少其一 | — | 单动作直接写类型键 |
| `do` | 步骤 | 动作列表 | 否 | — | 同一主语上的有序动作组(闭环写法的载体);`do` 内动作可各带 `via`/`when` |
| `export` | 步骤 | list | 否 | — | 导出跨主机 fact(§2.10) |
| `retry` | 步骤 | `{attempts, delay}` | 否 | 不重试 | 重试 |
| `on_error` | 步骤 | enum | 否 | `abort` | `abort \| continue` |
| `timeout` | 步骤 | duration | 否 | 引擎默认 | 步骤时限 |

步骤语境中 `service` 允许一次性动作 `state: restarted|reloaded`。

### 2.10 export 条目

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `fact` | export 项 | 标识符 | 是 | — | 导出名,进入 `${exports.*}`(全 fleet 可读,先导出后使用,lint 校验偏序) |
| `type` | export 项 | enum | 是 | — | `string \| command`;仅 `command` 可被 `from_fact` 消费(类型门) |
| `secret` | export 项 | bool | 否 | false | 打码 + 污点流规则同 params |
| `from_cmd` | export 项 | cmd 结构 | 是 | — | 只读命令(仅 argv/flags/args/env/take),stdout 即值 |
| `from_cmd.take` | from_cmd 体 | enum | 否 | `all` | `all \| first_line \| last_line`,封闭裁剪,**无正则** |
| `when` | export 项 | 条件 | 否 | 恒真 | 条件导出;消费点必须携带兼容 `when`,否则 E412 |

### 2.11 `types.<name>`(L2)

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `params` | 类型体 | map | 否 | `{}` | 实例字段契约,格式同 §2.2;类型体内经 `${self.*}` 引用 |
| `observe` | 类型体 | 一个探针条目 | 是 | — | 只读判定;探针通过 = 该主机已收敛 |
| `apply` | 类型体 | `{procedure: 名}` | 是 | — | 存在未收敛主机时起舞;舞按自身 selector 执行,幂等由 `creates`/observe 保证(无 target 动态作用域,见 §6) |
| `destroy` | 类型体 | `{procedure: 名}` | 否 | — | 退役之舞 |
| `upgrade` | 类型体 | `{procedure: 名}` | 否 | — | `crater upgrade` 触发 |

### 2.12 preflight / health 条目

条目 = 探针条目(同 §2.7 探针,支持元字段 `name`/`on`/`when`/`for_each`)或(仅 preflight)assert 条目:

| 字段 | 出现位置 | 类型 | 必选 | 说明 |
|---|---|---|---|---|
| `assert` | preflight 项 | 条件 | 是 | 对参数/拓扑形状的静态断言(可用 `<selector> is empty` 子句) |
| `reason` | preflight 项 | string | 否 | 失败时打印的人话 |

### 2.13 传导语义(规范定义,治"改 sysctl 重启了 caddy")

- **传导源**:`file / copy / template / lineinfile / unarchive / systemd_unit` 六类;`sysctl / package / user / group / swap / kernel_modules` **不是**传导源。
- **flush-point 规则**:每个 `service` 资源收集"上一个 service 之后、本 service 之前"声明的、且与其同主机的传导源变更;有任一变更 ⇒ 本轮 apply 末尾按 `on_change` 执行 restart/reload,**一轮至多一次**。
- 服务当前非 running(或未声明 state)则传导跳过。
- plan 必须画出传导箭头:`↳ 传导重启: service caddy(因 /etc/caddy/Caddyfile 变更)`。

---

## 3. 表达式规范终稿

### 3.1 引用:`${}`,只有名词

```
interp := "${" ns "." ident ("." ident)* "}"        # ${item} 为唯一无点特例
ns     := params | facts | exports | host | self
```

| 命名空间 | 内容 | 可用位置 |
|---|---|---|
| `params.*` | 声明过的参数 | 一切可插值数据位 |
| `facts.*` | 单机事实,封闭白名单:`arch / distro / distro_version / kernel / hostname / ipv4_default / cpu_cores / mem_mb` | 同上 + `materials.source.by` 判据 |
| `exports.*` | 舞步导出的跨主机 fact | 导出步骤之后 |
| `host.name` / `host.addr` | 当前主语主机 | procedure 步骤内(含 via 委托) |
| `item` | `for_each` 当前元素 | for_each 体内 |
| `self.*` | L2 实例参数 | 类型体内 |

规则:

1. **插值必须在双引号字符串内**(E101,fmt 自动修复)——同时解决 YAML flow 语境非法与视觉一致性。
2. **整值引用保型**:字符串恰好是一个 `${...}` 时按引用目标类型取值(`port: "${params.port}"` 得 int;`"${params.data_dirs}"` 得 list);混入其它字符则得 string;**列表禁止拼进混合字符串**(用 `join` 构造器)。
3. **严格解析,无静默透传**:任何 `${` 起始序列必须解析为合法引用,否则报错;字面 `${` 写 `$${`。同一记号只有一种命运。
4. **禁止出现的位置**:一切键名、资源类型键、selector、cast 值、`flags[].name`、`material:` 引用位(收物料声明名)、param 声明、`when`(条件有自己的文法)。

### 3.2 条件语言:一种原子拼法 + 三个组合子

```
cond   := clause-string | {all: [cond, …]} | {any: [cond, …]} | {not: cond}

clause-string := clause { " and " clause }           # 只有 and,无 or,无括号
clause := ref                                        # bool 型参数真值
        | "not " ref
        | ref " == " literal | ref " != " literal
        | ref " in " "[" literal { ", " literal } "]"
        | ref " is set" | ref " is not set"
        | selector " is empty" | selector " is not empty"
ref    := ("params." | "facts.") ident
```

- **or 只以结构化 `any:` 出现**——"或"是要被 reviewer 看见的分叉,值得多两行结构。
- 原子只有 clause-string 一种拼法(消灭 `{param: ha}` vs `params.ha` 双拼写);组合子的叶子就是 clause-string。
- 零连接可判定:引用必须已声明;`==` 两侧类型必须匹配(int 参数比字符串字面量 → 类型错);`is set` 仅对 `required: false` 且无 default 的参数合法(其余参数永远有值,lint 报"恒真");int 参数禁裸真值(必须比较)。
- **允许位置白名单**:资源/探针条目 `when`、步骤 `when`、`do` 内动作 `when`、`flags[].when`、`args[].when`、`env` 值 `when`、`file.data` 值 `when`、export 项 `when`、preflight `assert`。此外无处可写条件。

### 3.3 构造器:v1 只有 `join`

```yaml
RUSTFS_VOLUMES: { join: "${params.data_dirs}", with: " " }
```

list→string 是**声明分隔符的结构**,不是管道过滤器。允许位置:`template.vars` 值、`file.data` 值。将来加构造器也是加一个 YAML 结构、进一次 schema,永远不往字符串里加语法。

### 3.4 「条件拼命令行 flag」终稿

被枪毙的写法(解析期 E310,报错直接给结构化改写):

```yaml
cmd: kubeadm init ${params.ha ? "--upload-certs" : ""}     # 不存在这种语法
```

唯一写法:`argv` 固定头 + `flags` 有序结构列表,条件是**条目的属性**:

```yaml
cmd:
  argv: [kubeadm, init]
  flags:
    - name: --pod-network-cidr
      value: "${params.pod_cidr}"
    - name: --control-plane-endpoint
      value: "${params.cp_endpoint}"
      when: params.cp_endpoint is set
    - name: --upload-certs
      when: params.ha
```

三道保证:`flags[].name` 禁插值 ⇒ lint 静态枚举全部展开组合(此例 4 种);命令以 argv 直达 execve,不过 shell,注入与引号事故根治;plan 逐条解释缺席。同一 `{value, when}` 叶子模式复用于 `args`、`env`、`file.data`——学一次,用四处。

### 3.5 逃生舱:`from_fact`,有围栏的唯一舱门

现实(kubeadm 打印 join 命令)需要"执行运行期才知道的命令"。v1 的答案不是 shell 字符串,而是:

- export 侧:`type: command` 类型标注,`from_cmd` 是 argv 结构,`take` 封闭裁剪;
- 消费侧:`from_fact: <名>` 只接受 command 型 fact(string 型 → 类型错),追加参数仍走结构化 `flags`,**必须配 `creates`** 幂等护栏;
- 工具侧:lint 标注 trusted-fact,plan 高亮此步为"运行期内容"。

**明确禁止**(文法中无产生式):三元/if-else、算术、函数与过滤器、正则 capture、ref 与 ref 互比、嵌套插值、下标、字符串方法、模板内条件与循环、自由字符串命令、YAML anchor/alias/自定义 tag/多文档流。

### 3.6 YAML 载体纪律(脚枪防线)

lint 强制(均零连接可查):`mode` 必须为带引号的 `0[0-7]{3,4}` 字符串;`sha256` 必须带引号(Norway/科学计数防线);`file.data` 与 `env` 值 schema 类型钉死为 string(裸 `on`/`yes` → 类型错并给修复);插值必须带引号(E101);flags 只许 block 风格(fmt 规范化)。

---

## 4. 三个场景终稿 YAML

### 4.1 S1 · caddy(24 内容行)

```yaml
crater: 1
blueprint: caddy

params:
  domain: { type: string, doc: 站点域名 }        # 无 default 即 required

materials:
  caddy_bin:
    source:
      by: facts.arch
      amd64: { url: "https://dl.example.com/caddy/2.8.4/linux-amd64/caddy", sha256: "3f7a9d10ab…" }
      arm64: { url: "https://dl.example.com/caddy/2.8.4/linux-arm64/caddy", sha256: "9c21e0bb47…" }

resources:
  - copy: { material: caddy_bin, dest: /usr/local/bin/caddy, mode: "0755" }
  - template:
      dest: /etc/caddy/Caddyfile
      content: |
        ${params.domain} {
          root * /srv/www
          file_server
        }
  - systemd_unit: { name: caddy.service, source: ./files/caddy.service }
  - service: { name: caddy, state: running, enabled: true }

health:
  - http: { url: "http://localhost:80", status: 200 }
```

说明:五个顶层键,无 fleet/cast/procedures/types,渐进披露达成;`by: facts.arch` 把"按目标机 arch 下载"说成一个维度而非一段 if;四条资源与运维心智动作一一对应,Caddyfile 变更自动传导重启 caddy(plan 画箭头),零 handler。Caddyfile 里的裸 `{`/`}` 不含 `${` 起始序列,不受插值文法影响。

### 4.2 S2 · rustfs

```yaml
crater: 1
blueprint: rustfs

params:
  version:           { type: version, phase: build, default: "1.0.3" }
  port:              { type: port, default: 9000 }
  console_port:      { type: port, default: 9001 }
  access_key:        { type: string }
  secret_key:        { type: string, secret: true }
  data_dirs:         { type: list[string], default: [/data/rustfs] }
  bypass_disk_check: { type: bool, default: false }

materials:
  rustfs_bin:
    extract: { member: rustfs/rustfs }            # zip 内取成员
    source:                                        # URL 含 build 参数:
      by: facts.arch                               #   sha256 由 crater lock 固化
      amd64: { url: "https://dl.example.com/rustfs-${params.version}-linux-amd64.zip" }
      arm64: { url: "https://dl.example.com/rustfs-${params.version}-linux-arm64.zip" }

preflight:
  - port_free: { port: "${params.port}", allow_owner: rustfs }
  - port_free: { port: "${params.console_port}", allow_owner: rustfs }

resources:
  - user: { name: rustfs, system: true }
  - file: { path: "${item}", state: directory, owner: rustfs, mode: "0750" }
    for_each: "${params.data_dirs}"
  - copy: { material: rustfs_bin, dest: /usr/local/bin/rustfs, mode: "0755" }
  - file:
      path: /etc/rustfs/rustfs.env
      format: env
      owner: rustfs
      mode: "0600"
      data:
        RUSTFS_ADDRESS:         ":${params.port}"
        RUSTFS_CONSOLE_ADDRESS: ":${params.console_port}"
        RUSTFS_ACCESS_KEY:      "${params.access_key}"
        RUSTFS_SECRET_KEY:      "${params.secret_key}"
        RUSTFS_VOLUMES:         { join: "${params.data_dirs}", with: " " }
        RUSTFS_SKIP_DISK_CHECK: { value: "on", when: params.bypass_disk_check }
  - systemd_unit: { name: rustfs.service, source: ./files/rustfs.service }
  - service: { name: rustfs, state: running, enabled: true }
  - wait: { until: { port_open: { port: "${params.port}" } }, timeout: 60s }

health:
  - http: { url: "http://localhost:${params.port}/health", status: 200 }
```

说明:params 段即使用说明书(build/deploy 分相、secret、必选一眼可见,`crater params` 直接渲染);`port_free` + `allow_owner` 一行说清"未被非 rustfs 进程占用";`format: env` + `data` 让 diff 按键逐条对账、secret 按键打码,`{join}` 与 `{value, when}` 两个叶子结构使列表拼接与布尔开关都无处写三元;`wait` 显式出现在 plan 里,诚实。

### 4.3 S3 · k8s

inventory(单节点拓扑,同一份蓝图零改动):

```yaml
inventory: lab
hosts:
  n1: { addr: 192.168.1.10 }
groups:
  controlplane: [n1]
  worker: []                      # 空组合法
params:
  ha: false
```

blueprint:

```yaml
crater: 1
blueprint: k8s-cluster

fleet:
  groups:
    controlplane: { min: 1 }
    worker:       { min: 0 }      # 单节点 = 1 cp + 空 worker,天然合法

cast:
  seed:      controlplane.first
  spare_cps: controlplane.rest
  workers:   worker

params:
  k8s_version: { type: version, phase: build, default: "1.31.4" }
  ha:          { type: bool, default: false }
  cp_endpoint: { type: string, required: false, doc: 控制面接入点;不设则不注入对应 flag }
  pod_cidr:    { type: cidr, default: "10.244.0.0/16" }

materials:
  kubeadm:
    source:
      by: facts.arch
      amd64: { url: "https://dl.k8s.io/v${params.k8s_version}/bin/linux/amd64/kubeadm" }
      arm64: { url: "https://dl.k8s.io/v${params.k8s_version}/bin/linux/arm64/kubeadm" }
  kubelet:
    source:
      by: facts.arch
      amd64: { url: "https://dl.k8s.io/v${params.k8s_version}/bin/linux/amd64/kubelet" }
      arm64: { url: "https://dl.k8s.io/v${params.k8s_version}/bin/linux/arm64/kubelet" }
  kubectl:
    source:
      by: facts.arch
      amd64: { url: "https://dl.k8s.io/v${params.k8s_version}/bin/linux/amd64/kubectl" }
      arm64: { url: "https://dl.k8s.io/v${params.k8s_version}/bin/linux/arm64/kubectl" }

preflight:
  - assert:
      any:
        - params.ha
        - spare_cps is empty
    reason: 多 control plane 必须开启 ha
  - port_free: { port: 6443, allow_owner: kube-apiserver }
    target: controlplane

resources:
  # ---- 底座(缺省 on: all)----
  - swap: { state: disabled }
  - kernel_modules: { load: [overlay, br_netfilter] }
  - sysctl:
      settings:
        net.bridge.bridge-nf-call-iptables: "1"
        net.ipv4.ip_forward: "1"
  - package: { name: containerd, state: present }
  - copy: { material: kubeadm, dest: /usr/local/bin/kubeadm, mode: "0755" }
  - copy: { material: kubelet, dest: /usr/local/bin/kubelet, mode: "0755" }
  - copy: { material: kubectl, dest: /usr/local/bin/kubectl, mode: "0755" }
  - systemd_unit: { name: kubelet.service, source: ./files/kubelet.service }
  - service: { name: kubelet, enabled: true }        # 不管理运行态:join 后由 kubeadm 拉起

  # ---- HA 前置(ha=false 时在 plan 中"省略",而非跑了跳过)----
  - package: { name: keepalived, state: present }
    target: controlplane
    when: params.ha
  - template: { dest: /etc/keepalived/keepalived.conf, source: ./files/keepalived.conf.tmpl }
    target: controlplane
    when: params.ha
  - service: { name: keepalived, state: running, enabled: true }
    target: controlplane
    when: params.ha
  - package: { name: haproxy, state: present }
    target: controlplane
    when: params.ha
  - template: { dest: /etc/haproxy/haproxy.cfg, source: ./files/haproxy.cfg.tmpl }
    target: controlplane
    when: params.ha
  - service: { name: haproxy, state: running, enabled: true }
    target: controlplane
    when: params.ha

  # ---- 名词:这台机器应是集群成员(L2)----
  - k8s_member: {}

types:
  k8s_member:
    observe:
      cmd: { argv: [test, -f, /etc/kubernetes/kubelet.conf] }
    apply:   { procedure: bootstrap }
    upgrade: { procedure: upgrade }
    destroy: { procedure: leave }

procedures:
  bootstrap:
    - step: init-first-cp
      target: seed
      cmd:
        argv: [kubeadm, init]
        creates: /etc/kubernetes/admin.conf          # 重跑 bootstrap 安全
        flags:
          - name: --kubernetes-version
            value: "v${params.k8s_version}"
          - name: --pod-network-cidr
            value: "${params.pod_cidr}"
          - name: --control-plane-endpoint
            value: "${params.cp_endpoint}"
            when: params.cp_endpoint is set
          - name: --upload-certs
            when: params.ha

    - step: export-join-facts                        # 独立于 init:后加节点与首建同舞
      target: seed
      export:
        - fact: join_command
          type: command
          from_cmd: { argv: [kubeadm, token, create, --print-join-command], take: first_line }
        - fact: cert_key
          type: string
          secret: true
          when: params.ha
          from_cmd:
            argv: [kubeadm, init, phase, upload-certs, --upload-certs]
            take: last_line

    - step: join-spare-cps                           # 逐台,护 etcd;单节点时空集 no-op
      target: spare_cps
      serial: 1
      retry: { attempts: 2, delay: 30s }
      cmd:
        from_fact: join_command
        creates: /etc/kubernetes/kubelet.conf
        flags:
          - name: --control-plane
          - name: --certificate-key
            value: "${exports.cert_key}"
            when: params.ha                          # 与导出条件兼容,E412 静态可证

    - step: join-workers                             # 并行;空组 no-op
      target: workers
      retry: { attempts: 3, delay: 20s }
      cmd:
        from_fact: join_command
        creates: /etc/kubernetes/kubelet.conf

  upgrade:
    - step: stage-new-kubeadm
      target: all
      copy: { material: kubeadm, dest: /usr/local/bin/kubeadm, mode: "0755" }

    - step: upgrade-seed                             # 每台一个完整闭环,窗口收敛到单台
      target: seed
      do:
        - cmd: { argv: [kubectl, drain, "${host.name}", --ignore-daemonsets, --delete-emptydir-data] }
        - cmd: { argv: [kubeadm, upgrade, apply, "v${params.k8s_version}", --yes] }
        - copy: { material: kubelet, dest: /usr/local/bin/kubelet, mode: "0755" }
        - copy: { material: kubectl, dest: /usr/local/bin/kubectl, mode: "0755" }
        - service: { name: kubelet, state: restarted }
        - cmd: { argv: [kubectl, uncordon, "${host.name}"] }

    - step: upgrade-rest                             # 其余 cp 先、worker 后,逐台闭环
      target: [spare_cps, workers]
      serial: 1
      retry: { attempts: 2, delay: 30s }
      do:
        - cmd: { via: seed, argv: [kubectl, drain, "${host.name}", --ignore-daemonsets, --delete-emptydir-data] }
        - cmd: { argv: [kubeadm, upgrade, node] }
        - copy: { material: kubelet, dest: /usr/local/bin/kubelet, mode: "0755" }
        - copy: { material: kubectl, dest: /usr/local/bin/kubectl, mode: "0755" }
        - service: { name: kubelet, state: restarted }
        - cmd: { via: seed, argv: [kubectl, uncordon, "${host.name}"] }

  leave:
    - step: evict-node
      target: all
      serial: 1
      cmd: { via: seed, argv: [kubectl, delete, node, "${host.name}", --ignore-not-found] }
    - step: reset-node
      target: all
      cmd: { argv: [kubeadm, reset, --force] }

health:
  - http: { url: "https://localhost:6443/livez", status: 200, insecure: true }
    target: controlplane
  - service_active: { name: kubelet }
```

说明(设计要点,非自评):

- **零字符串逻辑**:两个条件 flag(`--control-plane-endpoint`/`--upload-certs`)与 join 的 `--certificate-key` 全部是带 `when` 的结构条目;全文件没有一处三元、拼接或占位空串;lint 能打印 init 命令的全部 4 种展开。
- **跨主机 fact 按简报字面交付**:`join_command`(command 型)与 `cert_key`(string 型、secret、条件导出)两个 fact;消费点的 `when: params.ha` 与导出条件兼容,E412 静态证明 ha=false 时不悬空;preflight 的 `assert any` 把矛盾拓扑(多 cp 无 ha)拦在门外。
- **单节点合法性靠空集语义**:`spare_cps`/`workers` 为空 ⇒ 对应步骤 no-op,plan 标注 `跳过 (0 hosts)`,零分支。
- **升级舞是每台节点的闭环**(drain→升级→换件→重启 kubelet→uncordon),cordon 窗口收敛到单台;这是对简报线性序列的有意偏离(语义等价、窗口更小),三位评委一致要求。`via: seed` + `${host.name}` 表达"在首台执行、对当前主语生效",on × via × serial 三字段正交。
- **L2 叙事**:使用者面对一行名词 `- k8s_member: {}`;新加 worker 后 `crater apply` 由 observe 发现非成员、自动起 bootstrap 舞,init 被 `creates` 跳过、token 照发;`crater upgrade` 走 upgrade 舞。

---

## 5. 可发现性终稿

三个层面共享同一份类型注册表(内建 21 类型 + 本蓝图 L2 类型 + 微语法主题),保证 CLI、schema、报错永不互相矛盾。

### 5.1 CLI 命令面

```
crater lint | fmt | plan | apply | verify | upgrade | destroy | run <procedure>
crater bake | lock | params <bp> | facts <host> | types [<类型>] | explain <微语法主题> | schema
```

`crater types`(列表):

```
$ crater types
期望态资源:
  file          文件/目录/内容(format: env 支持按键对账的环境文件)
  copy          物料或本地文件落盘
  template      纯替换模板渲染(无条件、无循环)
  lineinfile    按锚定行管理配置行
  unarchive     归档展开
  systemd_unit  unit 文件安装(自动 daemon-reload)
  service       systemd 服务运行态/自启(变更传导的 flush point)
  hostname / swap / kernel_modules / sysctl
  user / group / package / image_present
  cmd           幂等命令(资源位必须带 creates)
  wait          阻塞直到探针通过
只读探针(preflight / health / until / observe):
  http  port_open  port_free  service_active  cmd
本蓝图自定义(types:):
  k8s_member    集群成员(observe: kubelet.conf;apply: bootstrap)
微语法: crater explain when | selector | interpolation | flags
```

`crater types <某类型>`(字段卡):

```
$ crater types service
service — systemd 服务的期望态
字段:
  name       string  必选   服务名(可省 .service 后缀)
  state      enum    可选   running | stopped;不写 = 不管理运行态
  enabled    bool    可选   开机自启;不写 = 不管理
  on_change  enum    可选   restart | reload(默认 restart)—— 上游变更传导时的动作
元字段: name / on / when / for_each
传导: 上一个 service 之后声明的 file/copy/template/lineinfile/unarchive/systemd_unit
      变更会触发本服务 on_change;plan 会标注传导原因。
步骤语境额外可用: state: restarted | reloaded(一次性动作;资源语境写它 → error[E201])
示例:  - service: { name: caddy, state: running, enabled: true }
另见:  crater types systemd_unit · crater explain when
```

**plan 的两条规范义务**(不是实现细节,是规范):

1. **解释缺席**:每个因 `when` 为假而省略的 flag/资源/步骤、每个因空集跳过的步骤,逐条给出原因;
2. **标注传导**:`~ template /etc/caddy/Caddyfile  ↳ 传导重启: service caddy`。

```
$ crater plan k8s.blueprint.yaml -i lab.inventory.yaml
n1 (amd64, debian 12)
  = swap disabled                     无变化
  …
  省略: package keepalived            (when: params.ha = false)
  省略: flag --control-plane-endpoint (params.cp_endpoint 未设)
  省略: flag --upload-certs           (params.ha = false)
  跳过: step join-spare-cps           (spare_cps 为空集)
  跳过: step join-workers             (workers 为空集)
  ! step init-first-cp 含 from_fact/creates 幂等命令(运行期内容,已高亮)
计划: 9 变更 / 0 销毁;本次零写入。
```

### 5.2 JSON Schema 生成策略

- `crater schema` 生成 `.crater/schema.json`,蓝图头一行 `# yaml-language-server: $schema=.crater/schema.json` 接入任何 LSP 编辑器。
- **结构**:资源条目 = 判别键 oneOf(`required: [service]` + `additionalProperties: false`),字段、枚举、必选逐层补全,拼错字段就地飘红;条件组合子(`all/any/not`)、flags/args/export 均有子 schema。
- **自特化**(方案 5):schema 按本蓝图生成——`material:` 位枚举你声明过的物料名,`on:` 位枚举 cast 角色与 fleet 组名,`source:` 位提示 `files/` 实存文件,L2 类型作为新的判别键进入 oneOf。契约段(params/cast/materials/types)同时就是编辑器数据源。
- **分工**:schema 管结构与枚举;叶子字符串(`${}` 路径、when 子句、selector)由 `crater lsp` 用与 lint 同一颗校验内核补齐(参数名补全、悬停显示类型与 doc、导出 fact 作用域高亮)。

```jsonc
{ "$defs": { "resource": { "oneOf": [
  { "required": ["service"],
    "properties": {
      "service": { "$ref": "#/$defs/service" },
      "name": { "type": "string" },
      "on":   { "$ref": "#/$defs/selector" },
      "when": { "$ref": "#/$defs/condition" },
      "for_each": { "type": "string", "pattern": "^\\$\\{(params|self)\\.[A-Za-z_][\\w.]*\\}$" } },
    "additionalProperties": false }
  /* … 其余 20 内建类型 + 本蓝图 L2 类型 … */ ]},
  "service": { "additionalProperties": false, "required": ["name"],
    "properties": { "name": {"type": "string"},
      "state": {"enum": ["running", "stopped"]},
      "enabled": {"type": "boolean"},
      "on_change": {"enum": ["restart", "reload"], "default": "restart"} } } } }
```

### 5.3 报错信息风格规范

规则:(1) 错误码分层——E1xx 引用/声明、E2xx 心智模型/语义边界、E3xx 表达式纪律、E4xx 数据流/作用域、W 系警告;(2) 每条四要素:**位置**(行:列 + 源码下划线)、**判决**、**最近似修复**("你是不是想写")、**下一步命令**;(3) 心智模型纠正优先于拼写纠正;(4) 文案与 `crater types`/schema 同源。

```
error[E102]: 未声明的参数 `params.pord`
  --> rustfs.blueprint.yaml:27:33
   |
27 |         RUSTFS_ADDRESS: ":${params.pord}"
   |                            ^^^^^^^^^^^^ params 中没有 `pord`
   = 已声明: version, port, console_port, access_key, secret_key, data_dirs, bypass_disk_check
   = 你是不是想写 `port`?    下一步: crater params rustfs
```

```
error[E201]: `state: restarted` 不是期望态
  --> rustfs.blueprint.yaml:40:30
   |
40 |   - service: { name: rustfs, state: restarted }
   |                              ^^^^^^^^^^^^^^^^
   = resources 声明持续成立的状态;restarted 是一次性动作。
   = 想保持运行 → state: running;想重启一次 → 放进 procedure 步骤。
   = 此处 state 可取: running | stopped    参见: crater types service
```

```
error[E310]: `${…}` 内只允许一条点路径引用,不允许表达式
  --> old.blueprint.yaml:57:24
   |
57 |   argv: [kubeadm, init, "${params.ha ? '--upload-certs' : ''}"]
   |                          ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
   = 条件出现的 flag 请写成结构化条目:
       flags:
         - name: --upload-certs
           when: params.ha
   = 参见: crater explain flags
```

```
error[E412]: `${exports.cert_key}` 在此处可能悬空
  --> k8s.blueprint.yaml:118:22
   = cert_key 由 step `export-join-facts` 在 when: params.ha 下条件导出;
     本引用点缺少兼容的 when —— ha=false 时该值不存在。
   = 修复: 给该 flag 加 when: params.ha
```

```
error[E413]: secret 参数 `params.secret_key` 流入命令行
  --> bad.blueprint.yaml:33:9
   = secret 值禁止进入 argv/flags/url(进程表与日志可见)。
   = 请改走 mode "0600" 的 file(format: env)或 template。
warning[W421]: secret 参数写入 mode "0644" 的文件 —— 建议 mode: "0600"
```

---

## 6. 被拒绝的路(拒绝理由与采纳的设计同等重要)

| 方向 | 一句为什么 |
|---|---|
| `shell` 自由字符串步骤(方案 1/2) | 铁律开一次口子就守不住;旗舰示例怎么写,一万个社区蓝图就怎么写 |
| 双轨引用记号:裸引用 vs 插值(方案 2/4/5) | 新手死穴 + 类型记忆税;单记号 + 整值保型以零歧义覆盖同等能力 |
| 主语入键(`"copy /path"` 作键名)(方案 2/5) | 键侧语法废掉 oneOf 补全与通用 YAML 工具,三件套最贵的一件打对折 |
| selector-map 分节(方案 2) | YAML mapping 无序,键序承载执行序是把语义押在实现上,且无法交错分组 |
| 正则 capture 进 export(方案 4/5) | 解析逻辑回到字符串,上游输出改版即静默错捕;`take` 三选一封闭裁剪替代 |
| 虚无律(none/false 静默蒸发)(方案 5) | 正确缺席与错误缺席同形,对"对账"引擎是方向性风险;条件必须显式,缺席必须由 plan 解释 |
| 类型驱动 flag 渲染(bool→裸 flag)(方案 5) | `--flag=false` 永远写不出、map 无序;保留其精神(plan 解释缺席),渲染走显式 when |
| `each` 一词多义(方案 2) | 资源循环与逐台动作组是两个概念;`for_each` 与 `serial+do` 各司其职 |
| procedure 参数化与 `run` 组合(方案 4) | 三级跳读把 3am 认知步数推到全场最高;v1 用重复买"就地读完",复用走 L2 |
| `target` 动态作用域(方案 4) | 跨层隐式绑定不可静态追踪;L2 apply 以 creates/observe 幂等代替目标集穿透 |
| `when` 中的 `or` 连接词(方案 1) | 无括号双连接词是误读高发区;or 只以结构化 `any:` 出现,分叉可见即可审 |
| 「15 个关键字」预算叙事(方案 4) | 把词表挪进表外账本不减少认知负担,是会计学极简 |
| `(( ))` 插值记号(方案 5) | "与货物的 `{{}}` 不打架"的收益小于与主流 `${}` 直觉一致的收益;冲突由 `$${` 转义 + 严格解析报错解决 |
| 算术与字符串函数(`port+1`) | 多声明一个参数让契约显式外化;运算符是 Jinja 化的第一级台阶 |
| 模板条件/循环 | 模板只做替换;结构化生成走 `format/data`、`for_each`、L2——模板是藏逻辑的第一现场 |
| handler/notify、register/set_fact、include/role 树、22 层变量优先级 | Ansible 被公认做错的部分,IR 已用传导/export/5 层优先级取代 |
| YAML anchor/alias/自定义 tag/多文档流 | 对账工具第一美德是"文件里写的就是发生的";DRY 让位于 WYSIWYG,复用走 L2 |

---

## 7. 开放问题(悬而未决,≤5)

1. **机群级配置生成**:按 controlplane 列表生成 haproxy backend 段这类"每台一行"的配置,v1 无解(模板零逻辑 + 无机群级迭代事实)。候选方向:引擎注入封闭的机群事实结构供 `for_each` 消费,或 L2 生成器资源——两者都要防止变成模板循环的后门。
2. **command 型 fact 的结构化拆解**:三评委认可"join 拆成 endpoint/token/ca_hash 三元组"是比 `from_fact` 更彻底的形态,但无正则时对 kubeadm 单行输出拆不开。是否为 v1.x 增加封闭的 argv-token 拆分器(把 command 型 fact 解析为 token 列表供结构化重组)?
3. **serial > 1 的批语义**:批内并行、批间串行时,`do` 闭环的失败语义(fail-fast 中断整批,还是完成本批再停)与传导的交互尚未钉死。
4. **L2 apply 的目标收窄**:纯 creates/observe 幂等在大机群下意味着全舞重放(虽然每步都快速跳过);是否需要引擎级"只对未收敛主机裁剪 selector"的优化语义,以及它是否应当对作者可见。
5. ~~跨蓝图复用与模块化~~ **已裁定**,见 §8 修订 A1/A2;仍开放的残余:跨蓝图 export 可见性、蓝图仓库分发(v2)。

---

## 审校记(主笔,非面板产出)

**R1 · shell 逃生舱的张力。** 本规范在文法上不存在自由字符串命令 —— 这与
product-design.md 的「`shell` 是合法资源类型,增长路径 = 从一个 shell 步骤开始
永远不撞墙」存在张力。但注意:结构上逃生舱**仍然存在** ——
`cmd: { argv: [bash, -c, "a | b && c"] }` 是合法写法,管道进了一个显式的 argv token,
plan 会高亮它。也就是说:野路子没有被禁止,只是**不再是被祝福的语法糖**,
写的人要多打 12 个字符并被 plan 点名。我认为这个平衡成立,予以保留;
若实践中发现迁移阻力过大,可在 v1.x 加 `sh:` 糖(降解为上述 argv 形态)。

**R2 · 与现行实现的差距(实施清单)。** 语法层几乎全部重写:`${params.x}` 语法不变但
新增严格解析与 E101;`when` 从 CEL 换成子句微文法(CEL 求值器退居内部实现或删除);
`on:` 换 selector 新文法 + `cast`;`shell` 类型删除、`cmd`(argv 结构)接任;
materials 换 `source.by` 开关表;新增 `crater`/`blueprint` 顶层键、lock 文件、
`fmt`/`types`/`explain`/`schema` 命令。IR(五动词/舞/物料/机群)零改动。


---

## 8. 修订(定稿后由用户裁定,2026-08-28)

> 触发:用户审阅时问「一个 yaml 写到底吗?yaml 引用 yaml 有设计吗?」并裁定
> 「kubespray 全写在一个 yaml 里根本不可能」。两条修订均不动 §6 对 include 的
> 拒绝理由 —— 拒绝的是**参数化、条件化、任意位置**的文本包含(Ansible 三级跳读),
> 不是拒绝多文件本身。

### A1 · 约定式分节(v1 即生效)

蓝图在逻辑上是**一个目录**:根文件 + 可外置的顶层节。外置必须在根文件显式声明,
文件名由约定钉死 —— 读者永远不需要猜"还有哪些文件参与"。

```yaml
# k8s.blueprint.yaml(根文件)
crater: 1
blueprint: k8s-cluster
parts: [procedures, types]        # 这两节住在同目录约定文件里
params: …
resources: …
```

```
k8s.blueprint.yaml            # 根:除外置节以外的一切
k8s.procedures.yaml           # 顶层就是 procedures 节的内容
k8s.types.yaml
files/…
```

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `parts` | 顶层(仅根文件) | list[enum] | 否 | `[]` | 可外置节:`resources / procedures / types / materials / health / preflight`;每节对应同目录 `<stem>.<节名>.yaml` |

五条纪律(全部 lint 强制):
1. **只能外置整个顶层节**,part 文件顶层就是该节内容,不得再含 `parts`(无嵌套);
2. **文件名无自由度**:`<根文件 stem>.<节名>.yaml`,不接受任意路径 —— 这是与 include 的本质区别;
3. 同一节**内联与外置二选一**,双定义 → error[E121];`parts` 声明了但文件不存在 → E120;
   同目录存在形如约定名的文件但未声明 → E122(防"幽灵文件静默不生效");
4. **零参数、零条件**:part 文件无条件整体生效,合并后与单文件**逐字节等价**;
5. 工具视角是一个文档:诊断跨文件定位(`k8s.procedures.yaml:12`),
   `crater fmt --join` / `--split procedures` 双向机械转换,任何时刻可无损合回单文件。

### A2 · Stack 层:蓝图的组合(**v1.1 已实现**)

「先装 containerd,再建 k8s,再上存储」是蓝图之间的**编排**,不是一个巨型蓝图。
承接旧 project(D-083)与 product-design.md 的 Stack 名词。

```yaml
# platform.stack.yaml
crater: 1
stack: platform
uses:
  - blueprint: containerd            # 名(库内解析)/ 路径 / OCI ref
  - blueprint: k8s-cluster
    params: { ha: true }             # 作者侧覆盖:效力=更强的默认值
    groups: { controlplane: k8s_masters }   # 蓝图组名 → inventory 组名重映射
  - blueprint: rustfs
    groups: { storage: k8s_workers }
```

| 字段 | 出现位置 | 类型 | 必选 | 默认 | 说明 |
|---|---|---|---|---|---|
| `crater` | 顶层 | int | 是 | — | 同蓝图 |
| `stack` | 顶层 | string | 是 | — | 栈名,制品标识 |
| `uses` | 顶层 | list | 是 | — | **有序**条目:apply 自上而下,destroy 逆序 |
| `uses[].blueprint` | 条目 | string | 是 | — | 蓝图引用 |
| `uses[].params` | 条目 | map | 否 | `{}` | 覆盖该蓝图 params 默认;**属作者侧**,与蓝图 default 合并为"默认值层",仍可被 inventory 组/主机与 CLI 覆盖 —— 运行期优先级保持 5 层不变 |
| `uses[].groups` | 条目 | map | 否 | `{}` | 组名重映射(蓝图 fleet 组名 → inventory 组名);未映射的组按同名匹配 |

边界与语义:
- 每蓝图自校验自己的 fleet 契约;任一失败,整栈不开始;
- **跨蓝图 export 不可见**(v1.1 边界):蓝图自包含,栈只管顺序 —— 需要传值就升参数;
- 离线:栈 bake 为一个制品 = 各蓝图闭包的并集(内容寻址去重,承接 D-101 的闭包分发);
- verify/destroy 按条目逐个执行,报告分蓝图分节。

### kubespray 的映射(检验修订够不够用)

kubespray ≈ 一个 stack × 若干中等蓝图,而不是巨文件 + 50 个 part:

```
kubespray.stack.yaml
├─ os-baseline.blueprint.yaml      (~80 行:swap/内核/sysctl/包)
├─ containerd.blueprint.yaml       (~120 行)
├─ etcd.blueprint.yaml             (~150 行:独立 etcd 拓扑时)
├─ k8s-core.blueprint.yaml         (~300 行,procedures 外置成 part)
├─ cni-flannel.blueprint.yaml      (~80 行;calico 等各自成蓝图,栈里二选一)
└─ addons.blueprint.yaml           (~100 行)
```

**A1 管单蓝图内的篇幅,A2 管蓝图之间的组合** —— 两把刀分别对准两个问题,
互不越界;任何一把试图包办全部,就会退化回 include 树。

#### 实现落点(与上表的差异,以此处为准)

- 命令面:`crater plan/apply/verify <栈>.stack.yaml`,与蓝图**同一条执行路径** ——
  栈只是给每份蓝图多戴一副"透镜"(组名映射 + 作者侧参数)。`crater lint` 也认栈:
  检查引用解析得开、参数名与组名对得上那份蓝图。
- **引用解析**按约定试三条路径(`<名>.blueprint.yaml` / `<名>.yaml` /
  `<名>/<名>.blueprint.yaml`),找不到时**列出试过的每一条** —— 只说"找不到"
  是引用解析最烦人的失败形态。OCI ref 留到闭包分发一并做。
- **组名重映射的语义**:被显式映射的蓝图组名**不再同名直通**。若
  `controlplane → k8s_masters`,这份蓝图眼里的 `controlplane` 就只有
  `k8s_masters` 的成员,哪怕 inventory 里恰好也有个叫 `controlplane` 的组 ——
  显式优先于巧合,否则两处会悄悄合并。
- **参数优先级实测**:蓝图 default → 栈 `params` → CLI `--set`,后者赢。
  运行期的层数没变,栈只是往"默认值"那一层里加了一笔。
- **失败即停**,并报出"其后 N 份未执行"。栈是有序的:containerd 没装上,
  k8s 装了也不会好;继续往下只会把一个清晰的失败变成一堆难解的失败。
- **同一份蓝图在一个栈里出现两次是错误**:两次部署会互相覆盖状态记录,
  而"哪次赢"取决于顺序 —— 与其让人事后 debug,不如解析期就拒绝。
- **退役**:`crater destroy <蓝图或栈> [--yes]`,**默认只预演**。
  plan/apply 的分工在 destroy 这里塌成一条命令,所以安全阀长在命令自己身上。
  栈**逆序**退役(k8s 建在 containerd 上,正序拆会先抽走底座),且
  **失败不停** —— 与 apply 的"失败即停"相反,因为半拆的栈比全拆的栈更糟。
  退役不校验机群契约:契约问的是"够不够装",而一个只剩一台 master 的残破
  集群恰恰最该拆。
- **栈级 bake**:`crater build -f <栈> -o closure.tar` 把整栈烤成**一个**制品,
  各蓝图闭包取并集。去重发生在两层:同一 URL 只下载一次,相同字节只落盘一份。
  栈的 `uses[].params` 会带进物料 URL 的插值 —— 漏掉它会**静默**烤错版本。


### A3 · 篇幅:信息,不是判决(修订,用户裁定)

> **原方案已撤销。** 初稿把篇幅做成 W430/W431/W432 三级 lint 警告 + `--strict` 升失败
> + 禁止文件内豁免。用户质疑"要硬限制篇幅大小吗,这不合适吧" —— 质疑成立,撤销。

#### 为什么撤销(三条,都要认)

1. **那些数字是编的。** 初稿写"80 ≈ 两屏""200 ≈ evaluator 实测的 90 秒认知预算" ——
   **没有任何实测**。那是设计面板的行文,被当成数据复述了。
2. **行数不度量复杂度。** 48 行的物料声明是纯数据(读起来零成本),48 行的嵌套条件不是。
   按行数一刀切,等于对"数据多"和"逻辑绕"给同样的判决。
3. **它在健康文件上叫唤。** 实测本仓库旗舰蓝图 k8s-ha:`procedures` 109 行、全文 248 行,
   会立刻触发原方案两条警告 —— 而那 109 行是三支各自内聚的舞,并不臃肿。
   在健康代码上误报的规则会让整个 lint 输出失去可信度,最终被整体关掉。

#### 改为

- **机制照做**:A1 的 `parts:` 外置是**能力**(让你能拆),不含判断,已实现。
- **篇幅降为信息**:`crater lint --stats` 报各顶层节的内容行数;仅对**可外置**的节
  附一句"可 `crater fmt --split <节>` 外置"。**不计入 error/warn,不影响退出码。**
- **规范里保留经验参考,并写明它不是规则**:`procedures` 超过约一百行通常值得外置;
  单个蓝图超过约四百行时,先问"它是不是在装两个东西"(那是 A2 stack 的信号)。
  这些数字是**起点**,不是阈值 —— 谁也没测过你的蓝图。

原始诉求是"要有个**规范**",初稿给成了"**执法**"。规范可以说"一百行通常值得外置",
执法不该在它 109 行时报警 —— 尤其当那 109 行本身没问题。


---

### A4 · `cast` 选角表(v1 已实现)

蓝图里 `first(role.controlplane)` 在 k8s-ha 里出现了 **7 次**。这不是啰嗦的问题 ——
是**定址逻辑没有单一定义处**:哪天引导节点的选法变了(比如改成 `role.controlplane where facts.etcd`),
要在七个地方同时改对。

```yaml
cast:
  seed:      first(role.controlplane)   # 引导节点:kubeadm init 在这里跑
  followers: rest(role.controlplane)    # 其余控制面:join --control-plane
  masters:   role.controlplane
  workers:   role.worker
```

此后 `target: seed` 处处可用。三个刻意的设计:

1. **解析期展开,不是运行期查表。** `target: seed` 在解析完成时已经是
   `First(Role("controlplane"))`,运行期零间接、plan 输出照旧显示完整 selector
   ——「seed 是谁」在计划里应当是**看得见的**,不该逼人回去翻 `cast:`。
2. **不许套娃。** `cast` 条目自身不能引用另一条 `cast`。一层间接是命名,
   两层就开始要跳读了。
3. **拼错有人管。** `target: sed` 既不像 `role.x` 也不像 `host.x`,只可能是想引用选角表
   —— 报错直接给最近的候选:``是不是想写 cast 里的 `seed`?``

### A5 · `fleet.groups` 机群契约(v1 已实现)

没有契约时,"给 HA 蓝图配了一台 master"这种错**要跑到一半才暴露** ——
`first(role.controlplane)` 正常选中那台、init 跑完、装了 CNI,直到
`rest(role.controlplane)` 选出空集才发现不对。那时候机器已经被改过了。

```yaml
fleet:
  groups:
    controlplane: {min: 1}
    worker:       {min: 0}   # 0 = 允许为空组,与"没声明这个组"是两回事
```

校验发生在**连任何机器之前**:没 SSH、没 preflight、更没改动。规则:

- `min` 省略时是 **1**,不是 0 —— 声明一个组却允许它空着是反直觉的。
- `min: 0` 是单节点拓扑的正当写法:`worker: {hosts: []}` 组存在但没成员,是**合法拓扑**而非打错字。
- inventory 里有蓝图没声明的组**不算错**:同一批机器常同时承载多个蓝图。
- **一次报全部**不满足项。修 inventory 的人应当一趟改完,而不是修一条重跑一次再看下一条。
- 契约一旦存在就是权威:`cast` 引用了 `fleet.groups` 没声明的组,**解析期**就报错并给拼写建议。

两者合起来,`crater schema -f <蓝图>` 生成的 JSON Schema 会把本蓝图的选角名
排进 `target:` 的补全候选 —— 可发现性三件套(字段卡 / Schema / 报错)在这里合上了闭环。

---

### A6 · 已知缺口:滚动升级需要"逐台跑完整组步骤"

procedure 的执行模型是 **步骤为外层、主机为内层**:一步跑遍所有选中的机器,
再进下一步。`throttle` 控制的是"一步之内同时几台"。

这个形状对建集群是对的(首台 init 必须先于其余台 join),但**滚动升级要的是
相反的嵌套**:每台机器跑完"drain → 升级 → 重启 → uncordon"整组步骤,再动下一台。

k8s-ha 的 upgrade 因此只能写成:

```
drain 全部 master → upgrade apply(seed)→ upgrade node(其余)
                  → 换 kubelet(全部)→ 重启(全部)→ uncordon 全部
```

后果是**升级窗口内三台 master 同时处于 cordoned**。对控制面尚可接受(它们跑的是
静态 Pod,不受调度影响);但同样的形状套到 worker 上就不成立了 —— 那意味着一次性
drain 掉全部工作负载承载节点。所以现有蓝图**根本没 drain worker**,代价是 kubelet
重启时工作负载会有短暂扰动。两种都不理想,只是取舍不同。

**这不是蓝图写法的问题,是执行模型缺一层。** 补的方向不是给 `throttle` 加参数
(它管的是并发度,不是嵌套顺序),而是引入**步骤组**:

```yaml
upgrade:
  steps:
    - shell: { cmd: "kubeadm upgrade apply -y v${params.version}" }   # 只在 seed 跑一次
      target: seed
    - rolling:                    # ← 这一组按**主机**外层展开
        target: all
        throttle: 1               # 一次一台跑完整组
        steps:
          - shell: { cmd: "kubectl drain ${substrate.name} ..." }
          - shell: { cmd: "kubeadm upgrade node" }
          - copy:  { material: kubelet, dest: /usr/local/bin/kubelet }
          - service: { name: kubelet, state: restarted }
          - shell: { cmd: "kubectl uncordon ${substrate.name}" }
```

语义:`rolling` 块内的步骤对每台机器按序跑完,再换下一台;块外仍是原来的
步骤外层模型。`exports` 在块内不可用 —— 逐台滚动与"从一台取值给其余台"是
互斥的两种意图,允许它只会制造难解的顺序依赖。

暂不实现的理由:现有形状在控制面上能用,而这个块会引入嵌套的作用域与错误归属
问题(块内第三台失败时,前两台已经完成 —— 报告与重试语义都要重新定义)。
**先把缺口写清楚,比先造一个形状可疑的机制重要。**
