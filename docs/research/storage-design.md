# crater 存储设计参考:三分与窄接口

> 起因是一个具体问题:crater 的 YAML 该存哪 —— git 仓库、rustfs、SQLite 还是
> PostgreSQL?本文给出的答案是**这四个不是备选项,而是三个不同位置上的答案**。
>
> 旁证来自 harness/harness 的实现分析(一个容器同时提供 git 托管与十余种制品
> 仓库),详见 mica 文档 `devops/harness/architecture`。

## 一、两条原则

**原则一:存储三分。** 版本化的文本、不可变的大字节、关于内容的账本,是三类
不同的数据,各有唯一合理的归宿。把它们放进同一个存储,一定会在某一类上付出
不该付的代价。

**原则二:驱动接口要窄。** 存储后端的抽象接口,方法数量决定了换后端的成本。
窄到五个方法,加一个后端就是加一个文件;宽到二三十个方法,每加一个后端都要
实现一堆用不上的能力,于是永远只有一个后端。

## 二、crater 的三类数据

| 数据 | 内容 | 现在在哪 | 该在哪 |
| --- | --- | --- | --- |
| **期望态** | 蓝图、inventory、app 文件、模板 | UI 进程的工作目录(CWD) | **git** |
| **物料与闭包** | 二进制、镜像层、`closure.tar` | 本地 tar,解到临时目录 | **窄接口后面的 blob 存储**(本地目录 / OCI / rustfs) |
| **运行态** | 部署记录、job 日志、对账快照、存活缓存 | `~/.crater/state`、`~/.crater/jobs` | **保持本地文件**,直到出现多控制端 |

这个划分不是抄来的分类学,它对应三种**根本不同的访问模式**:

- 期望态要 diff、要评审、要原子的多文件变更、要按时间回滚。
- 物料只要"按摘要拿到这份字节",永远不改,只增不删。
- 运行态高频写、只对控制端有意义、丢了能靠一次 verify 重建。

## 三、期望态 → git

### 为什么不是数据库

**用数据库存期望态,等于把 git 重新实现一遍,而且实现得更差。**

具体会得到什么:一张 `(path, content, version)` 表,然后要自己写 diff、分支、
合并、blame、原子提交、冲突解决。这些 git 全都有,而且离线可用、格式稳定了
二十年。

这不是假设。crater 走过这条路:旧管线的 `TursoStore`(SQLite)建过
`deployments` / `job_runs` 两张表,D-106 明确以"期望态零入库"把期望态搬回了
文件。**这条路是走过之后退回来的,不是没考虑过。**

### 为什么 git 而不是对象存储

S3/rustfs 的版本号是**按对象**走的。而一次部署变更的最小单位常常是"同时动了
5 个文件"——改了蓝图、改了模板、改了 inventory 的一台机器。对象存储给不了
这个语义,git 的一次 commit 恰好就是它。

### air-gap 不是障碍

- bare repo 可以放在内网任意一台机器上,不需要 GitHub、不需要任何服务端。
- `git bundle` 把整个仓库连历史压成**单个文件** —— 和 crater 的闭包哲学是
  同一个思路:一个文件带走一切。

### 硬前提:密钥外置

**inventory 里现在是明文口令。进 git 就是永久泄漏** —— git 历史删不掉,
rebase 只是把它挪个地方。

这不是"后续优化",是**走 git 之前必须先做完的事**。可选路子,按代价排序:

1. **优先 SSH key 认证**,直接绕开口令这个概念(改动最小,收益最大)。
2. 口令留在一个 gitignore 的本地 secrets 文件里,inventory 只写引用。
3. 需要密文进库时用 age/sops —— 单二进制、离线可用,符合本项目的零依赖气质。

顺带:`.bak` 与 `.crater-trash/` 已进 `.gitignore`,但那只挡住了备份文件,
挡不住 inventory 正文里的口令。

### 三步走

1. **UI 每次写入自动 git commit。** 改动极小,却当场闭合审计链(现在"可 git"
   只是嘴上说的 —— UI 从不提交,所以谁在什么时候改了哪台机器的口令没有任何
   记录),顺便白拿版本回滚。
2. **仓库为源(pull 模式)。** crater 盯一个 repo,对账"仓库里的期望态 vs 机器
   现实"。这才是 ArgoCD 世界观的完整形态,也让多控制机不会各改各的。
3. **UI 编辑 = 提交到分支。** 到这一步才需要认真设计冲突与权限。

## 四、物料与闭包 → 窄接口后面的 blob 存储

### 现状与瓶颈

现在的形态(`crates/crater-cli/src/material_ctx.rs`、`closure.rs`):

```rust
pub type BlobMap = BTreeMap<String, PathBuf>;   // source URL → 本地路径
```

`open_closure()` 只认一种来源:`target.closure: Option<PathBuf>` 指向的本地
tar 文件,解到临时目录后建出这张表。

**问题不在于它做得不对,在于来源被写死了。** 想让闭包从 rustfs、从 OCI
registry、从一个共享目录来,现在只能在 `open_closure` 里加 if。

### 建议的接口:窄到四个方法

```rust
/// 物料字节的来源。**四个方法** —— 窄到加一个后端是加一个文件,
/// 而不是改一层。
pub trait BlobSource: Send + Sync {
    /// 这个来源里有没有这份物料(按蓝图声明的 source URL 查)。
    fn contains(&self, source: &str) -> bool;

    /// 取字节,返回**本地可读路径**。
    ///
    /// 刻意不返回流:部署侧要把它分块推到目标机,需要的是能反复读、
    /// 能问大小的东西。远端来源在这里负责下载到本地缓存并返回缓存路径 ——
    /// 调用方不必知道它来自哪。
    fn fetch(&self, source: &str) -> Result<PathBuf>;

    /// 清单:build 侧核对、UI 侧展示、部署前的完整性检查都读它。
    fn manifest(&self) -> &Manifest;

    /// 人可读的来源标识,只用于报错与日志("从哪拿的"要说得出来)。
    fn origin(&self) -> String;
}
```

写入侧(`crater build -o` 的出口)另立一个,同样窄:

```rust
pub trait BlobSink {
    /// 摘要由调用方算好传进来 —— 校验发生在**写入之前**,
    /// 不是让每个后端各自实现一遍。
    fn put(&self, source: &str, bytes: &Path, sha256: &str) -> Result<()>;
    fn finish(self: Box<Self>, manifest: &Manifest) -> Result<()>;
}
```

`Manifest` 与 `BlobEntry { source_url, sha256, size }` 已经在
`crater_core::bundle` 里,接口直接复用,不新造类型。

### 三个后端,同一个接口

| 后端 | 用途 |
| --- | --- |
| `TarClosure`(现有逻辑搬进来) | 单文件带走一切,U 盘进现场 |
| `OciSource` | 复用已有的 `push`/`pull`/`store`,library 里已有 zot |
| `RustfsSource` / S3 | 多站点共享闭包归档 |

**注意 crater 已经有完整的 OCI 通路**(`build`/`save`/`load`/`push`/`pull`,
本地 store 在 `~/.crater/store`)。引入 rustfs 之前应当先回答它相对 OCI 的
增量是什么 —— 若只是"存 closure.tar 归档、多站点分发",rustfs 合适;若是
"分发带摘要校验的物料",OCI 已经在做,再加一层是重复建设。

### 窄接口的纪律:抵住加方法的冲动

harness 的 `blob.Store` 只有五个方法(`Upload`/`Download`/`Move`/`Delete`/
`GetSignedURL`),支撑了十余种制品仓库。

值得注意的是它**为什么**有 `GetSignedURL` —— 那让大文件下载可以直接重定向到
对象存储,不必穿过应用进程。这是一个真实存在的需求,所以它占了五分之一的
接口面积。

**crater 目前没有这个需求**(物料是从控制端分块推到目标机的,不存在"让第三方
直接下载"的场景)。所以不要加。同理不要预先加 `list`、`delete`、`gc` ——
等到有真实调用方再加,每个方法都要能说出"谁在调它"。

## 五、运行态 → 保持本地文件

`~/.crater/state/*.json` 有一个 OCI 和 git 都比不了的好处:**坏了能用 cat 看、
用 rm 修**。单控制机场景下这完全够用,不要为了"看起来正规"引入数据库。

### 数据库的真正临界点

**逼出数据库的不是"存 YAML",是并发控制。**

现在两台控制机 —— 甚至同一台上的两个人 —— 同时对同一批机器 apply,没有任何
东西拦得住,`~/.crater/state` 会被后写的覆盖。

- 单机:文件锁就能解决。
- 多控制端:需要一个共享租约。**那才是 PostgreSQL 唯一站得住的位置。**

判断标准因此很清楚:**如果 crater 一直是单控制机的操作台,数据库永远不需要;
一旦要多控制端,需要数据库的不是 YAML,是那把锁。**

harness 是这条判断的现成旁证:它做了 git 托管、十余种制品仓库、CI 流水线、
Gitspaces,默认配置仍然是**一个 SQLite 文件**。它的数据库里存的是"关于内容
的事实",不是内容本身。

## 六、这套分层什么时候不成立

诚实地列出边界,否则它会被当成教条:

- **仓库里出现巨型二进制时,git 那一格就破了。** 蓝图旁边的 `files/` 目录如果
  塞进几百 MB 的物料,git 会迅速难用,LFS 也只是补丁。规矩应当是:凡是大到
  该走闭包的,就不该躺在期望态仓库里。
- **期望态需要按记录级并发编辑时**(多人同时改同一份 inventory 的不同主机),
  git 的合并粒度是行,可能不够。真出现这个问题时,答案是把 inventory 拆细
  (每台机器一个文件),而不是搬去数据库。
- **运行态需要跨控制端聚合视图时**,本地文件方案直接失效 —— 这与上面的并发
  租约是同一个临界点。

## 七、落地顺序

1. **密钥外置** —— 解锁一切,也是目前离生产最远的那个洞。
2. **UI 写入自动 commit** —— 审计链闭合,白拿回滚。
3. **`BlobSource` / `BlobSink` 抽出来**,现有 tar 逻辑作为第一个实现搬进去
   (此时应当是纯重构,行为零变化,测试全绿即为验收)。
4. 需要多站点了,再加第二个 blob 后端。
5. 需要多控制端了,再谈租约与数据库。

前三步之间没有依赖,可以并行;第 4、5 步不要提前做 —— 它们的必要性来自真实
场景,不来自架构图的对称性。

## 八、参考

- harness 实现分析:mica `devops/harness/architecture`
- D-106:期望态零入库(旧管线 SQLite → 新管线文件)
- D-118:UI 五阶段落地
- 现有代码:`crates/crater-cli/src/material_ctx.rs`、`closure.rs`、
  `crates/crater-core/src/bundle.rs`、`crates/crater-ir/src/state.rs`
