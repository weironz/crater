# crater action 层设计提案(Ansible 能力,严守 D-036)

> 状态:**设计提案,待签字**。引擎代码在你批准本文件的形态后再动。
> 约束:本设计的每一处都必须通过 [D-036](decisions.md) 的检验——YAML 是数据,逻辑在 Rust。

## 1. 心智转变:从"装产品"到"做任意事"

- **现在**:`crater apply <产品>`——`component.yaml` 描述"怎么装 yq/docker"。crater 是"装万物器"。
- **终态**:`crater apply <动作>`——一个 **task** 描述"在目标机上要达成的一组状态",可以是装软件,也可以是改配置、发文件、起服务、跑巡检、批量执行任意操作。crater 是一个**声明式远程执行引擎**(Ansible/kubectl 的心智),"装产品"只是它的一个用例。

术语(全文固定):
- **task**:一个高层目标(文件或命名),如 `install-yq`。= 有序的 **action** 列表 + 物料 + targeting。
- **action**:task 里的一项,一次**原语调用**(`place` / `run_cmd` / `copy` …)。
- "`crater apply <动作>`"里的"动作"= 一个 task。

**这不改变 D-036,只是把能力做大**:task 仍是纯声明数据,所有控制流(循环/条件/计算/排序/重试/幂等)在 Rust。

## 2. 终态命令形态(方案 A:后缀自识别)

```
crater apply yq                       # 裸名(无后缀、无 / :)→ 命名 task(查 task 库)
crater apply install-yq.yaml          # .yaml/.yml 后缀 → 文件
crater apply -f ./x.yaml              # -f 显式文件(强制/消歧)
crater apply docker.io/lib/app:v1     # 含 / 或 : → 镜像(形态自识别)
crater apply yq.tar                   # .tar/.oci 后缀 → 离线包
```

识别优先级(引擎按此判定 source 形态,**纯静态、不执行**):
1. 有 `-f` → 文件。
2. 含 `/` 或 `:` → 镜像引用。
3. 后缀 `.yaml`/`.yml` → 文件;`.tar`/`.oci` → 离线包。
4. 否则(裸名)→ 命名 task,从 task 库解析。

目标(targeting)三层,**保留现有设计**(D-035 已实现):

| 写法 | 规模 | 凭据 |
|---|---|---|
| 无 `--host`/`-i` | 本机 | — |
| `--host a,b,c` | 少量 | 共用一套(`--user`+`--password\|--key`) |
| `-i inventory.yaml` | 大量 | 每主机各自(文件内声明) |

## 3. task 文件的 YAML schema(D-036 下)

```yaml
name: install-yq
hosts: all                 # targeting:组名 / all(声明式;引擎按 inventory 解析成员)
vars:                      # 静态数据,可被 {{ }} 纯取值
  version: "4.53.2"
materials:                 # 物料闭包(D-034),build 据此打离线包
  - { name: yq-bin, kind: file, url_tmpl: "https://.../v{{version}}/yq_linux_amd64" }
actions:                   # 动作清单;依赖用 needs 声明,排序由引擎做
  - id: place_yq
    action: place
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - id: verify
    action: run_cmd
    cmd: "/usr/local/bin/yq --version"
    needs: [place_yq]
```

**这份 YAML 能被静态读取/diff/生成而无需执行**(D-036 检验通过):没有 `when:` 表达式、没有 `loop:` 语法、没有模板里的计算或 filter。`{{ }}` 只做取值(#1 已用残废渲染器从机制上锁死)。

字段全集(每个都是**声明式数据**,引擎认识的封闭集合):
- 顶层:`name`、`hosts`(组名/all)、`vars`(静态键值)、`materials`(D-034)、`actions`。
- action 项:`id`、`action`(原语名)、原语参数、`needs`(依赖 id 列表)、`when_os`/`when_offline`(**封闭枚举开关**,见 §4)、`retries`(数值)、`ignore_errors`(布尔)、`notify`(handler id 列表)。
- 这些都不是"需要执行才知道结果"的东西——是引擎读得懂的有限词汇。

## 4. Ansible 的每项能力 → 如何全落在 Rust(YAML 保持愚蠢)

这是本设计的核心。**每一个"似乎要在 YAML 写逻辑"的需求,都给出引擎侧实现**:

| Ansible 里的逻辑 | 反例(禁止进 YAML) | crater 的做法(逻辑在引擎) |
|---|---|---|
| **循环 loop** | `loop: "{{ nodes }}"` | ① action 参数直接收**列表**(如 `pkg_install.packages: [a,b]`、`copy.items: [...]`),引擎内部遍历;② 跨主机的"循环"= targeting(`hosts: group`),引擎对组内每台执行;③ 动态列表(如所有 master IP)由引擎**预计算成 fact** → `{{ master_ips }}` 取值传给收列表的 action |
| **条件 when** | `when: "{{ env=='prod' }}"` | **封闭枚举开关**:`when_os: [debian, rhel]`、`when_offline: true`——引擎认识的有限字段;复杂分叉拆成不同 action 变体或不同 task |
| **计算** | `{{ (mem*0.5)\|int }}` | 引擎 fact/函数算好(如 `mem_half`)→ `{{ mem_half }}` 取结果,YAML 不放公式 |
| **依赖/排序** | (Ansible 靠顺序+block) | `needs: [id]` 声明,引擎**拓扑排序**(复用 dag.rs) |
| **幂等** | (Ansible 各模块自理) | 每个 action 原语在引擎内 `check→act→report`(D-023 已有) |
| **重试** | `until/retries/delay` 表达式 | `retries: 3`(纯数值字段),重试循环在引擎 |
| **错误处理** | `failed_when: "{{ ... }}"` | `ignore_errors: true`(布尔)/ `soft_fail`(已有),引擎策略;**不支持**自由 `failed_when` 表达式 |
| **handlers/notify** | (Jinja 条件触发) | `notify: [restart_x]` + `handlers:` 区(也是 action 列表);引擎在某 action 报 `changed` 时触发对应 handler——纯引擎机制 |
| **变量/facts** | 复杂 Jinja 取值 | 引擎采集 facts(os/arch/mem/hostvars,D-030)作只读取值源,YAML 只 `{{ path }}` |
| **模板** | Jinja `{% if %}{% for %}` | 残废渲染器,只 `{{ path }}` 代入,逻辑模板**报错**(#1 已落地) |

口诀:**循环→引擎遍历或 targeting;条件→封闭枚举;计算→引擎算;排序→needs;其余→引擎策略字段。YAML 永远只声明"用哪个原语 + 参数 + 取哪个值"。**

## 5. action 原语清单(Rust 白盒,收敛 ≤ ~30)

新增标准(三条全满足才加):**高频** + **run_cmd 表达别扭** + **值得引擎白盒理解(为了幂等/dry-run)**。否则一律 `run_cmd` 组合。

- **现有(9)**:`pkg_install`、`place`、`extract`、`render_template`、`write_file`、`systemd_unit`、`run_cmd`、`load_image`、`module`(`download` 已删,获取外部文件统一走 `place`+`materials`,D-047)。
- **建议补齐(Ansible 高频,白盒收益大)**:
  - `file`(建/删/权限/属主/软链,幂等)
  - `copy`(推控制端文件到目标,幂等 sha256 比对)
  - `service`(`systemd_unit` 泛化:start/stop/restart/enable)
  - `lineinfile`(幂等改一行配置)
  - `user` / `group`
- **不新增、用现有**:`command/shell`=`run_cmd`,`template`=`render_template`,`get_url`=`place`(声明 material),`unarchive`=`extract`(只做别名映射,不增种类)。

总数控制在 ~15–20,远未触及 30 上限——上限是给"真高频白盒需求"留的,不是用来堆的。

## 6. 与现有 component / materials 的关系与迁移

- `component.yaml` 是 task 的**一个特例**(目标恰好是"装某产品")。`preflight/install/verify` 三段 → 合并为有序 `actions` + 可选 `phase` 标签(或保留三段,二选一,见 §8)。
- **命名 task 库**:保留 `components/` 作为库(裸名 `crater apply yq` 解析它),或更名 `tasks/`(见 §8)。
- `materials`(D-034)、`place`、register/hostvars(D-030)、DAG(needs)、幂等(D-023)、targeting 三层(D-035)**全部直接复用**——这次是"把已有积木重组为通用 task 模型 + 补几个原语",不是推倒重来。
- **渐进迁移**:旧 `component.yaml` 继续可加载(兼容),新写法用 `actions`。yq/docker/es/node_exporter/zot 不破坏。

## 7. 明确不做(防止滑回 Ansible 坑)

- 不加 `when` 布尔表达式、`loop` 语法、模板里的计算/filter/`if`/`for`。
- 不把渲染器升级成 Tera/minijinja(永久残废,D-036/#1)。
- action 原语种类严格收敛;能 run_cmd 组合的不新增原语。
- `failed_when`/`changed_when` 这类"自由表达式"不做;只给封闭策略字段。

## 8. 待你拍板的问题

1. **顶层动作列表字段名**:`actions:`(贴合"crater apply 一个动作")/ `tasks:` / `steps:`?(我倾向 `actions:`)
2. **phase 去留**:`preflight/install/verify` 三段保留,还是并入单一有序 `actions:` + 每项可选 `phase: verify` 标签?(我倾向**并入 + 标签**,更通用)
3. **命名 task 库目录**:保留 `components/` 还是更名 `tasks/`?(我倾向保留 `components/` 避免无谓迁移,仅概念上叫 task 库)
4. **targeting 写在哪**:task 文件内 `hosts: <group>`(像 Ansible playbook)+ `-i` 提供 inventory/groups;CLI `--host` 临时覆盖。确认这个分工?
5. **handlers/notify 是否本期纳入**,还是先做 actions+needs,handlers 后置?
6. **条件开关的封闭集合**:首批就 `when_os` + `when_offline` 两个够吗?

签字后我会:落一条 ADR(D-037)锁定形态 → 实现 spec/schema + 引擎 task 层(复用现有积木)→ 真机验证 → 补 feature 文档。**期间任何一处我发现自己想给 YAML 加逻辑,会立即停下找你,按 D-036 把它挪进 Rust。**
