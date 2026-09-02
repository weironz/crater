# Issue 驱动的开发流程(人与 Agent 共用)

一句话:**一个 issue = 一次可提交的改动 = 一次可执行的验收**。

## 为什么要有这一页

"给 Agent 一个 issue 让它去修"是当下的行业惯例(SWE-bench 的评测形态就是
issue → 补丁 → PR)。但惯例只保证了**接口**,不保证**结果**:同一套接口下,
把二十个需求一股脑丢进去并发跑,和一次派一件有明确验收判据的活,产出质量
差得很远。这一页写的是本仓库对后者的约定。

## 三条规矩

### 1. 没有可执行的验收判据,就不要开始写代码

判据要是**能跑出来的东西**:一条命令 + 期望输出,最好在真机 / 真 registry 上。
"跑起来没问题""体验更顺"不是判据。

反证同样重要 —— 说清"什么情况下它该失败"。本仓库的教训:D-128 的索引把 yq
两个版本压成一条、D-127 的多架构、D-125 的凭据落盘,**全都是被反证测出来的**,
正向用例一个都没报警。

### 2. 一次一件,依赖显式写出来

issue 之间有先后就用 `blocked` 标签 + 正文里的 `依赖: #12`。并发跑互相依赖的
issue,产出的是三个各自成立、合到一起不成立的补丁 —— 而合并冲突还只是最轻的
那种,重的是语义冲突(两处各自"修好"了同一个约定的两半)。

### 3. 静默失效优先于报错

报错的 bug 人能看见;静默的 bug 要到出事才发现。本仓库栽过至少四次:
`on:` 从未生效(D-113)、`env:` 被丢弃(D-115)、索引版本互相覆盖(D-128)、
`install` 复用同名目录装错版本(D-128)。缺陷模板里那一栏问的就是这个。

## 标签

| 标签 | 含义 |
| --- | --- |
| `task` / `bug` | 一件事 / 一个缺陷 |
| `kind:spike` | 先弄清该不该做、怎么做,产出是判断不是代码 |
| `area:pkg` / `area:closure` / `area:ir` / `area:ui` / `area:ci` | 落在哪一块 |
| `blocked` | 被别的 issue 挡住,**先别动** |
| `agent-ready` | 上下文与判据齐备,可直接交给 Agent |
| `needs-decision` | 要人先拍板,**不要先写代码** |

`agent-ready` 与 `needs-decision` 互斥。一个 issue 没有 `agent-ready`,
默认就是**还不能派给 Agent**。

## 看板(GitHub Projects)

<https://github.com/users/weironz/projects/2>

**标签与看板的分工是刻意划开的,不要让它们重叠:**

| | 管什么 | 谁读它 |
| --- | --- | --- |
| **标签** | 能不能派(`agent-ready` / `blocked` / `needs-decision`) | 脚本与 Agent —— `gh issue list --label agent-ready` |
| **看板 Status** | 做到哪了(Todo / In Progress / Done) | 人 |
| **看板 优先级** | 先做哪个(P0 挡住真实场景 / P1 该做 / P2 可以等) | 人 |

同一个事实**只有一处**:就绪与否只看标签,进度只看看板。两边都记的东西
一定会漂,而漂了之后没人知道该信哪边。

看板上有优先级字段而不是靠手工拖拽排序,是因为**拖拽不可脚本化** ——
一个只能用鼠标维护的顺序,在自动化流程里等于不存在。

新开的 issue 由 `.github/workflows/project-add.yml` 自动进看板。
它需要一个带 `project` scope 的 `PROJECT_TOKEN` secret(`GITHUB_TOKEN` 摸不到
用户级 Project);没配时这一步会**失败而不是静默跳过** —— 看板漏了一条要能看见。

## 常用命令

```bash
gh issue list --label agent-ready              # 现在能派出去的
gh issue list --label blocked                  # 等谁
gh issue view 12                               # 看上下文与判据
gh issue develop 12 --checkout                 # 开分支并切过去
gh pr create --fill                            # 提 PR(正文自动带 issue 链接)

gh project item-list 2 --owner weironz         # 看板全貌
gh project item-edit --id <item> --project-id <pid> \
  --field-id <status> --single-select-option-id <opt>   # 改状态
```

提交信息里写 `Closes #12`,PR 合并时 issue 自动关。

## 与 docs/decisions.md 的分工

- **issue** 管"还没做的"。
- **decisions.md** 管"做完了,以及当时为什么那么选"。

一个 issue 做完,值得记的判断进 decisions.md 拿一个 D 编号;不值得记的就只留
在 PR 里。**issue 不是决策记录** —— 它会被关掉,而决策要能被三个月后的人读到。
