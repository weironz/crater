# crater 架构与设计理念

> 本文记录 crater 的定位、概念模型、离线打包设计,以及与 Ansible / Helm / kubespray 的关系。
> **每节标注 `[现状]`(已实现)或 `[规划]`(目标形态,未实现)**,避免把愿景当事实。
> 配方/执行层的具体决策见 [decisions.md](decisions.md);各能力的用法见 [features/](features/)。

---

## 1. 一句话定位

> **crater = Ansible 的执行/组合模型 + "每个可复用单元能声明一份可烤进 OCI 的离线闭包"。**

懂 Ansible 的人,唯一要学的新概念就是这一句:可复用单元(role)多了个 `materials:`,`build` 时把它的依赖闭包(二进制/镜像/OS 包)烤成一个**自包含 OCI 制品**,air-gap 拎一个 tar 进去即可,**不需要任何镜像基础设施**。

crater 作用在 **Helm 的下面那一层**:Helm 管"已存在 K8s 集群里的应用",crater 管"OS 和集群本身"(SSH 进裸机,装 containerd、跑 kubeadm,把 Helm 所依赖的那个 K8s 建出来),agentless、单静态二进制、目标机零运行时。

---

## 2. 与 Ansible / Helm / kubespray 的关系

| | 作用层 | 离线机制 | air-gap 时要运维什么 |
|---|---|---|---|
| **Ansible** | 通用 OS 编排 | 无(假设在线)| 自己 hack(预镜像 + copy)|
| **Helm** | K8s 之上(集群内应用)| chart 不含镜像,运行时拉 | (镜像在 registry)|
| **kubespray** | OS / 集群层 | **重定向下载地址到你的镜像源** | **自建+运维** registry + 文件服 + apt/yum 源,用 `generate_list.sh` 填充 |
| **crater** | OS / 集群层 | **把闭包烤进一个 OCI 制品** | **什么都不用搭** —— 制品自己就是"源",控制机 SSH 推 |

借鉴关系:
- **执行/组合模型** 完全对齐 **Ansible**(见 §3),认知成本最低。
- **打包 / 参数化 / 分发 / 发现** 的 UX 借鉴 **Helm**(OCI 分发、values 式契约、`show`),但**只借接口形态,不借 Helm 的模板逻辑**(见 §9 守则)。
- **离线闭包清单** 的理念借鉴 kubespray 的 `generate_list`,但 crater 是 **role 显式声明**而非从下载变量反推,且产出**自包含制品**而非"清单 + 你自己搭源"。

---

## 3. 概念模型(对齐 Ansible)

| Ansible | crater | 含 hosts? | 状态 |
|---|---|---|---|
| task(单模块调用)| **action step**(`- action: shell`)| 否 | [现状] |
| role(可复用捆绑:tasks+handlers+templates+files+defaults)| **role**(`action: role`)| 否 | [现状] role 存在(D-029),但**还没有自己的 materials/params/meta.dependencies** → [规划] 长全 |
| play(`hosts: X` + roles/tasks,"在哪里做什么")| **task**(`hosts:` + `actions:`)| 是 | [现状] crater 的 task 目前是 **play+role 合体**(既有 hosts 又有 actions/materials)|
| playbook(有序 play,site.yml)| **project** | —— | [规划] 缺这层 |
| `import_playbook` | project 组合 project | —— | [规划] |
| inventory + group_vars/host_vars | inventory(+ vars 分层)| —— | [现状] 身份+分组;[规划] vars 分层 |

**目标形态 [规划]**:把当前"合体的 task"拆成三层 ——
- **role** = 可复用的"体"(actions + materials + handlers + defaults + templates),**不含 hosts**,是**可 build 成 OCI、可独立版本化分发**的原子单元(k8s-ha、mysql、redis 各一个 role)。
- **task(= play)** = `hosts: X` + `roles: [...]`(+ 可选内联 actions + vars),即"在哪里做什么"。
- **project(= playbook)** = 有序的一组 task = 大型交付的装配清单(`site.yaml`)。

```yaml
# site.yaml —— project(= Ansible playbook,有序 task)[规划]
- name: 主机基线
  hosts: all
  roles: [host-init]
- name: K8s 控制面
  hosts: k8s_cluster
  roles: [k8s-ha, cni-flannel]      # 单 master 不带 apiserver-lb;HA 才带
  vars: { vip: 192.168.73.14, pod_cidr: 10.244.0.0/16 }
- name: 数据库（裸机，与 k8s 并行）
  hosts: db
  roles: [mysql]
```

**命名约定**:crater 沿用 `action / task / project`(task ≈ Ansible play,project ≈ playbook);文档需点明 "crater task ≈ ansible play" 以免 Ansible 老手混淆。

---

## 4. 离线依赖:定义与打包(crater 的核心原创)

### 4.1 定义 [规划:挂到 role;现状:挂在 task]

**离线依赖 = 一个 role 在断网目标机上跑起来所需的一切**,由 role 自己声明 `materials:`:
- `kind: file` —— 二进制 / tarball / 清单(`url_tmpl` 在线拉,或 `src` 读本地)
- `kind: image` —— 容器镜像
- `kind: os_package` —— deb/rpm + 依赖闭包(buildah 解析)

闭包**随复用单元(role)走**,不绑在某次部署上。这与 Ansible role 的 `files/`+`templates/` 一脉,crater 只是把"二进制/镜像/包"也纳入 role 自带范围。

### 4.2 "制品类型即在线/离线开关" [现状]

**crater 不设 `offline: true` 开关** —— 你 apply 的东西决定模式:
- `crater apply -f x.yaml` → 无 blob → **在线**:`copy`/`unarchive`/`load_image` 现拉 `url_tmpl`/`ref`/apt。
- `crater apply x.oci` → 有 blob(`PlanContext.offline_blobs` 被设) → **离线**:同样的 op 改成推已烤好的 blob。

**同一份 recipe,两种模式零改动**,引擎自动切 fetch / replay。不像 kubespray 要手动改一堆 `*_url` 变量。

### 4.3 打包层次 = 执行层次 [规划]

| 层 | 离线打包单元 | 说明 |
|---|---|---|
| **role** | **role OCI** = recipe + 自己的闭包 blob | 原子单元,独立 build/push/分发 |
| **task(play)** | 不带 materials | 纯绑定 hosts × roles → 决定用哪些闭包 |
| **project** | **bundle = 引用的所有 role OCI 的并集**(内容寻址去重)| air-gap 一个 tar |
| **inventory** | 不带 materials | 纯 WHERE + vars + 凭据 |

- role 之间用 `meta.dependencies`(Ansible 风)声明依赖;build 时**沿依赖图递归纳入闭包**——一套图既管执行也管打包。
- **条件依赖**(单 master 不要 LB)= 拆成**可选 role**(`apiserver-lb`),play 按需带;**边界 = role 粒度,build 时不必预判拓扑**。

---

## 5. OCI B类 artifact 结构 [现状,已验证]

`crater build -f task.yaml` 产出的**不是 rootfs 镜像**,是 OCI image-spec 1.1 的 **artifact**(与 Helm OCI / SBOM / WASM 同机制,registry/skopeo/oras 都认)。

```
~/.crater/store/
├── oci-layout
├── index.json                         # ref(org.opencontainers.image.ref.name) → manifest digest
└── blobs/sha256/<digest>              # 一切都是内容寻址 blob
    ├── <manifest>   artifactType=application/vnd.crater.component.v1, config, layers[]
    ├── <config>     {"name","version","runMode"}      # 一小段 JSON,非 rootfs config
    ├── <recipe>     task.yaml 原文                      # layer: mediaType=vnd.crater.recipe.v1+yaml
    └── <material…>  每个 material 一个 blob             # layer: mediaType=vnd.crater.material.v1
```

manifest 的 `layers`:每个 material 是一个 **layer**,靠 **annotation `org.crater.material.name`** 标识(如 `kubeadm@amd64`、`img-apiserver`),**不是路径**。recipe 也是一个 layer。

实物(`crater/k8s-ha:1.36.1`,29 层 = 1 recipe + 28 material):
```
recipe.v1+yaml  k8s-ha            18495B
material.v1     kubeadm@amd64     72978594B
material.v1     img-apiserver     29759488B
material.v1     cfg-keepalived    361B          # 内容 = minijinja 模板
…
```

关键性质:
- **内容寻址**:blob 文件名 = 其内容 sha256(改一字节 digest 就变)→ 天然去重(同二进制多处引用只存一份)+ 防篡改。
- **非 rootfs**:目标机路径不编码进 OCI;`copy dest: /usr/local/bin/kubeadm` 是 recipe 在 apply 时决定的。
- **取用**(`bundle::materialize_component`):遍历 layers,recipe 层 → 写出配方;material 层 → 建 `blobmap[name] = blobs/sha256/<digest>`,供 recipe 的 `copy material:` 等按名引用。
- **增量/惰性友好**:每个 material 已是独立 layer → [现状] `store.pull` 拉镜像时跳过已有 blob(D-078);[现状,D-088] `apply <ref>` 默认**瘦拉**(`pull_thin`)—— 只拉 recipe + 自建文件(`embedded`)层,依赖(`dependency`)层留在 registry、apply 时在线现拉;`--offline` 才全量拉做离线 replay。"task 定义得多"不拖垮单次部署。

### 5.1 OCI 操作栈:协议靠 oci-client,制品语义自写 [现状]

crater **不自实现 OCI registry 协议** —— registry I/O 全部用 `oci-client`(crates.io `oci-client` 0.17,即 **`oras-project/rust-oci-client`**;纯 Rust + rustls,无 C 依赖,musl 可静态编译)。分工:

| 层 | 谁做 | 内容 |
|---|---|---|
| **协议层** | `oci-client`(=rust-oci-client) | HTTP / distribution 协议、token 认证、manifest 与 blob 的 GET/PUT、multi-arch index 解析 |
| **制品语义层** | crater(`store.rs` / `bundle.rs`) | 本地 store(`index.json`/`oci-layout`/内容寻址/`tag`/`retag`/`list`/增量去重)+ B类 artifact 的**合成**(recipe+materials→manifest)与**还原**(`materialize_component`) |

即 **协议靠库、语义自写**:哪怕换成 oras CLI 也得自己实现第二层 —— oras 只负责"搬运 artifact",artifact 长什么样(recipe/material 结构)得 crater 自己定义。

用到的 API:`Client` / `Reference` / `secrets::RegistryAuth` / **`pull_manifest_raw`**(拉层清单)/ **`pull_blob`**(按 digest 逐个拉)。刻意用低层 `pull_blob` 而非高层 `pull` —— 后者面向 image,会**合成 image-manifest、丢掉 `artifactType` 和自定义 layer mediaType**(D-032 已踩并验证此坑)。

**partial pull(选择性拉层)天然可得**:`pull_manifest_raw` 与 `pull_blob` 本就分离 —— 先拉 manifest 看每层 mediaType,再自选拉哪些 blob。这正是上面"只拉计划引用的 layer"和「瘦在线部署」(只拉 recipe + 自建文件层,依赖层在线现拉;`--offline` 才全量)的实现基础,**零新增依赖**。

---

## 6. 变量分层:build 期 vs apply 期

### 6.1 两类变量 [现状:都在 task.vars 混着]
- **build 期变量**(影响 materials 拉什么):`version`、各组件版本。在线 apply 时才用,离线 build 时已冻进 OCI。**应在 task/role,烤进 OCI**。
- **apply 期变量**(纯环境配置,不影响 materials):`vip`、`subnet`、`pod_cidr`、`control_plane_endpoint`、是否启用 LB。**不该进 OCI,apply 时由 inventory 给**。

现状把两类混在 `task.vars`,导致环境特定值(VIP)被烤进 OCI → 制品绑死环境。

### 6.2 inventory 变量层 [现状,D-082]
仿 Ansible group_vars/host_vars,inventory 三级 vars(全局 `inventory.vars` / 组 `groups.<g>.vars` / 主机 `hosts[].vars`),`Inventory::resolve()` 合并进每台 `host.vars`,apply 时叠加到 `ctx.vars`,**优先级 主机 > 组 > 全局 > task params 默认**:
```yaml
inventory:
  vars: { vip: 192.168.73.14, subnet: 192.168.73.0/24 }   # 全局
  groups:
    controlplane: { hosts: [...], vars: { ... } }          # 组级
```
收益:OCI 变环境无关(build 一次到处 apply)、改环境不重建 OCI、凭据天然留在 inventory 不进可分发制品。

---

## 7. 契约与可发现性(让 OCI 可被别人使用)[现状,D-081]

OCI 是黑盒,消费者需要能 introspect 的契约 —— 对标 Helm `values.schema.json` + `helm show`、Terraform `variables.tf`。

1. [现状] **task 声明富参数 `params:`**:`description` / `default` / `required` / `stage: build|apply`。契约=声明的 params;裸 `vars`=内部默认。`effective_vars()` = params 默认 ⊕ vars。
2. [现状] **自动提取角色契约**:`roles_needed()` 扫 `when_role` 汇总"需要哪些组",`inspect`/`--gen-inventory` 用它。
3. [现状] **`crater inspect <ref|file>`**:file→load+expand;OCI→读内嵌(扁平)recipe。打印 name/version/描述/参数/角色/materials。
4. [现状] **`crater inspect --gen-inventory`**:吐骨架 inventory(必需组 + apply 期 params + 默认 + 注释)。
5. [部分] **apply 前校验**:已校验 required params(apply 全部 / build 仅 build 期);[规划] 校验角色(inventory 是否定义所需组)。
6. [规划] **OCI annotations**:一句话描述 + 角色列表写进 manifest annotations,Docker Hub 网页 / `skopeo inspect` 可见摘要。

**安全红线**:inventory 含明文凭据 → **可分发的 OCI 绝不内嵌 inventory**;凭据始终独立于制品。

---

## 8. 三者一致性(task/OCI ↔ inventory)

契约单一来源 = task/role;OCI 是它的离线冻结版(带同一契约);inventory 满足契约。
```
task/role ──声明──▶ 契约(params + 角色)
   ├─在线─▶ crater apply task -i inventory      （inventory 满足契约 → 校验 → 部署）
   └─build─▶ OCI（冻结同一 recipe + materials）
                 └─离线─▶ crater apply oci -i inventory（同契约 → 同校验 → 部署）
```
[规划] apply 前的契约校验让"三者对不上(缺 vip、漏组、版本不符)"在部署前就报出来。

---

## 9. 设计守则(不可逾越)

1. **引擎零产品知识**(见 features/engine-zero-product-knowledge.md + D-036):引擎只认通用指令(when_role / throttle / 可等待 fact / 角色推导 / materials 闭包),**不知道 k8s/etcd/LB 为何物**。所有产品知识(crictl、kubeadm、VIP 含义)只活在 task/role 的 yaml 里。
2. **数据驱动,逻辑在 Rust**:YAML 只放数据 + 闭合枚举条件;控制流(needs/when/拓扑/DAG)在引擎。
3. **学 Helm 的接口,不学它的模板逻辑**:借 values 契约 / `show` / OCI 分发;**拒绝**把 `if/range/tpl` 业务逻辑塞进模板(crater 的 minijinja 只在 `template` 动作渲染配置文件、只做数据展开,**不进 cmd**)。
4. **制品类型即在线/离线开关**,无 flag;同一 recipe 两模式共用。
5. **凭据永不进可分发制品**。

---

## 10. 已实现 vs 规划(速查)

**[现状] 已实现**:
- OCI B类 artifact(content-addressed、带名 material layer、recipe layer、非 rootfs)
- 离线 = `offline_blobs` 自动切换(制品类型即开关)
- `materials:` 三种 kind(file/image/os_package,buildah)
- `when_os` / `when_role` / `run_once` / `throttle` / 可等待 cross-host fact / fail-fast(`HostCoord`)
- register / hostvars / groups(kubekey 式嵌套 + `derive_roles` 角色推导)
- inventory 三级 vars(全局/组/主机,覆盖 task params 默认,D-082)
- **project 编排**(有序 plays,每 play 引用 task + hosts/vars 覆盖,delete 逆序;在线+file source,D-083)
- `template`(minijinja)、copy(content/src/material)/unarchive/load_image/service…
- **role 长全**(自带 materials/actions/handlers/params,展开扁平化,D-080)
- **`params:` 富契约 + `crater inspect` + `--gen-inventory` + apply 前校验**(D-081)
- build 提速:增量镜像拉取 + file 并发取材(D-078)

**[规划] 未实现**:
- role `meta.dependencies`(role 依赖 role,闭包沿图组合);task/role/play/project 分层
- 离线 project(`build -f project.yaml` → bundle 各 play 的 OCI、去重)+ 纯 Ansible `roles:[]` 内联 play + 跨 play hostvars + project inspect
- `--set k=v` CLI 覆盖;build/apply 变量分期再细化
- 惰性 partial pull(apply 只拉计划引用的 layer)
- 条件依赖拆可选 role(如 `apiserver-lb`)+ endpoint 按拓扑派生
- OCI annotations 摘要

---

## 11. 建议实施顺序(✅=已落地)

1. ✅ **role 长全**(materials/params 挂 role,D-080)。
2. ✅ **`params:` 契约 + `crater inspect`**(D-081)。
3. ✅ **inventory 三级 vars**(环境配置出 OCI,D-082)。
4. ✅ **apply 前校验**(D-081/082,per-host 合并后校验)。
5. ✅ **project(playbook)编排**(在线+file source,D-083);离线 bundle 待续。
6. **惰性 partial pull** —— 扩展性(define broad, materialize narrow)。
7. 离线 project bundle / 纯 Ansible `roles:[]` 内联 play / `--set` / role meta.dependencies / OCI annotations。

每一步都复用前一步:project 的 component 传 params 就是 role 的 params 契约,打哪组就是 inventory 的组。
