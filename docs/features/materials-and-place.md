# 物料闭包 `materials:` + `action: place`（D-034）

## 这是什么 / 解决什么

让组件**显式声明它需要的所有外部物料**（二进制 / 容器镜像 / OS 包），再由 `install`
按**逻辑名**引用，而不是把"下载哪个 URL""装哪个包"埋进 `install` 动作里。

为什么要这样——**离线打包的根问题**：早期 `crater build` 靠**扫描结构化 install 动作**
发现要打进离线包的物料，它只看得见 `action: download`。一旦依赖藏在 `run_cmd` 自由文本
（`apt-get install -y mysql-server`）或容器镜像里，打包器就**看不见、必然漏打**，离线机器
上自然装不起来。根因是"依赖"和"动作"耦合在一起。

`materials:` 把"这个组件需要什么"抽出来单独声明，`build` **只读这一段**就知道要打包什么；
`place` 让 `install` 只管"把某个物料放到哪"，"从 GitHub 拉还是从离线包取"交给引擎按
在线/离线决定。**一份组件描述，在线/离线两种形态通吃。**

```yaml
materials:                 # ← build 读这一段，绝不扫 install
  - name: yq-bin
    kind: file             # file | image | os_package（kind = 「怎么获取这份内容」）
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64"

install:
  - action: place          # ← 按逻辑名引用，不写死 URL
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"           # chmod 折进来，一步落地可执行
```

引擎语义：

| 形态 | `place yq-bin` 做什么 |
|------|----------------------|
| **在线** | 目标机自己 `curl` material 的 `url_tmpl` → `dest` → `chmod mode`；已存在则跳过（幂等 ok） |
| **离线** | 控制端把**打进 OCI 包的 blob**（按 material 名索引）推到 `dest` → `chmod mode`；远端 sha256 已匹配则跳过 |

`mode` 之所以折进 `place`：二进制要在同一幂等步里落地为可执行，省掉一条单独的 `chmod` run_cmd。

## 本地文件 material（`src`，D-066）

`kind: file` 有两种获取方式:`url_tmpl`(从 url 下载)或 `src`(读 task 同级的本地文件)。
后者用于**官方不提供、需人工维护的文件**——systemd unit、drop-in、配置文件等。

```yaml
materials:
  - name: unit-containerd
    kind: file
    src: files/containerd.service
actions:
  - id: ctd_unit
    action: place
    material: unit-containerd
    dest: /etc/systemd/system/containerd.service
```

- **build**:从 `<task 目录>/files/...` 读取,和 url 下载的物料一样打成 blob(同 key)。
- **place**:在线从控制机 task 目录 `PushFile` 推送,离线从 OCI 包取 blob——都是 **copy 语义,原样推送、不做 `{{}}` 渲染**(对标 Ansible 的 `copy: src=files/`;要渲染用 `render_template`)。

好处:task.yaml **不再内联任何文件内容**——要么 url 下载、要么 `files/` 维护,文件可独立 diff、审阅、复用,杜绝大段 `content: |` 把 YAML 撑爆。

## 多 arch material（D-048）

二进制是按 CPU arch 编的，所以 `kind: file` 带一个 **arch 维度**：同名 material 各声明
一个 `arch`，`place` 按目标机 `uname -m`（归一化 `x86_64→amd64`/`aarch64→arm64`）选变体。
URL 里 arch 命名各项目不一（docker `x86_64`、yq `amd64`），故**每变体写全自己的 url**，
不用 `{{arch}}` 占位（引擎不塞命名映射表）。

```yaml
materials:
  - name: yq-bin
    kind: file
    arch: amd64
    url_tmpl: ".../yq_linux_amd64"
  - name: yq-bin
    kind: file
    arch: arm64
    url_tmpl: ".../yq_linux_arm64"
```

解析规则（给定 name + 目标 arch）：精确匹配 arch 的变体 → 用它；否则用 `arch` 省略的
**中立**变体（脚本/配置等与 arch 无关的物料）；都没有 → **报错**（packaged for wrong arch，
气隙场景最该早暴露，绝不静默推个跑不起来的二进制）。**单 arch 二进制也应写 `arch`**，
让错配的目标响亮失败，而不是省略当中立。

| 维度 | build | apply |
|------|-------|-------|
| **打包** | 默认打**所有**声明的 arch 变体,每个一层、按 `name@arch` 标注;`--arch amd64[,arm64]` 收窄 | — |
| **在线** | — | `place` 选目标 arch 变体 → 目标机 curl 该变体 url |
| **离线** | blob 按 `name@arch` 进 OCI artifact | `place (offline)` 按 `name@arch` 从包内取对应 arch 的 blob |

> 注:本期 `name@arch` 多 blob 同装一个 artifact(离线正确);registry 路径「按 arch 只 pull
> 自己那条」的 **OCI image index** 优化为后续项(D-048 待续)。`kind: image` 复用镜像原生
> index,`kind: os_package` 的 os×arch 矩阵随它们接线时一并做。

## 基本 demo（yq，最小可复现）

### 在线

```bash
crater yq --host <host> --password <pw>
# [1/2] place yq-bin <- https://.../yq_linux_amd64 → changed
# [2/2] run: /usr/local/bin/yq --version → ok
# done: changed=1 ok=1
crater yq --host <host> --password <pw>     # 再跑：place → ok, changed=0（幂等）
```

### 离线（B 类 artifact，build 从 materials 抓料）

```bash
crater build -f examples/yq/yq.yaml          # → 本地库 crater/yq:4.53.2
#   fetch material yq-bin <- https://.../yq_linux_amd64      ← 读的是 materials 段
#   yq → artifact crater/yq:4.53.2: recipe + 1 material(s)

crater tag crater/yq:4.53.2 <registry>/yq:4.53.2
CRATER_INSECURE_REGISTRIES=<registry> crater push <registry>/yq:4.53.2

# 另一台（或清空 ~/.crater/store 后）：
CRATER_INSECURE_REGISTRIES=<registry> crater apply <registry>/yq:4.53.2 --host <host> --password <pw>
#   ...: crater component artifact → recipe-replay
#   [1/2] place (offline) yq-bin -> /usr/local/bin/yq → changed   ← 按 material 名取包内 blob
```

打进包的每个物料是一层 `application/vnd.crater.material.v1`，按 **material 名**
（`org.crater.material.name`）标注；离线 `place` 就按这个名字从包里取 blob，与在线引用同名。

## 真机验证（2026-05-31）

- 在线 `crater yq --host 192.168.73.11`：首次 `place yq-bin` changed=1（chmod 折入），再跑 changed=0（幂等 ok），`yq --version` = v4.53.2。
- 离线：`build`（日志 `fetch material yq-bin`，按名打包）→ `load` → `push` 到 zot → 清本地 store → `apply <zot>/yq:m --host 192.168.73.12` → `place (offline) yq-bin -> /usr/local/bin/yq` → n12 `yq --version` = v4.53.2。

## 边界 / 后续

- 本期 `kind: file` 全链路打通（yq + docker static），含 **arch 维度**（D-048，见上节）。
- `kind: image`（容器镜像，build 时 pull 进 OCI、离线 import）与 `kind: os_package`
  （build 时下 deb/rpm、离线本地装）**已在数据模型留位、尚未接线**——它们的 arch
  分别复用「镜像原生 index」「os×arch 矩阵」，随接线一并做。
- **OCI image index 优化**（D-048 待续）：现状多 arch 的 `name@arch` blob 同装一个 artifact，
  离线正确但经 registry 分发会拉全量；用 image index 按 arch 组织后，每台只 pull 自己那条。
- yq 作为最小闭环先证明新模型在线/离线 + 多 arch 都通，再推到复杂 task。

## 关联

- ADR：[D-034](../decisions.md)（物料闭包显式化）、[D-033](../decisions.md)（B 类 artifact 迁移）、[D-020](../decisions.md)（在线/离线单管线）。
- 设计：[design.md](../design.md)、[offline-oci.md](offline-oci.md)、[images-registry.md](images-registry.md)。
