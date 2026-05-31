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
    kind: binary           # binary | image | os_package
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
crater build --image -f examples/yq/yq.yaml -o /tmp/yq.oci
#   fetch material yq-bin <- https://.../yq_linux_amd64      ← 读的是 materials 段
#   yq → artifact crater/yq:4.53.2: recipe + 1 material(s)

crater load /tmp/yq.oci --as <registry>/yq:4.53.2
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

- 本期 `kind: binary` 全链路打通（yq 闭环）。
- `kind: image`（容器镜像，build 时 pull 进 OCI、离线 import）与 `kind: os_package`
  （build 时下 deb/rpm、离线本地装）**已在数据模型留位、尚未接线**——它们是给
  mysql/docker/k3s 这类有真实依赖闭包的组件准备的下一阶段。
- `build` 的 **version × os 矩阵**：`os_package` 按 OS 分叉（deb vs rpm），多 OS 物料拟用
  OCI image index 按平台/annotation 组织。yq 是纯二进制单一维度，未触发；mysql 会撞上。
- yq 作为最小闭环先证明新模型在线/离线都通，再把 `materials`+`place` 推到复杂组件。

## 关联

- ADR：[D-034](../decisions.md)（物料闭包显式化）、[D-033](../decisions.md)（B 类 artifact 迁移）、[D-020](../decisions.md)（在线/离线单管线）。
- 设计：[design.md](../design.md)、[offline-oci.md](offline-oci.md)、[images-registry.md](images-registry.md)。
