# materials 段设计预览（D-034 下一阶段）

把 `materials:` + `place`（已在 yq 闭环验证）推到**有真实依赖闭包**的组件上，暴露
[D-034](../../docs/decisions.md) 评审里的问题一/四。**这些 yaml 是设计示意，尚未生效**
（当前引擎只接线 `kind: binary`）——它们让"yq 看不出来的坑"在 mysql 上变具体。

## 三种 material kind，build 各自怎么"算出该打包什么"

| kind | 声明字段 | build 时做什么（在线机制包） | 部署时引擎做什么（在线 / 离线） |
|------|---------|---------------------------|------------------------------|
| `binary` ✅已接线 | `url_tmpl` | fetch URL → 一层 `material.v1`，digest 即 sha256 | 在线：target 自己 curl；离线：推包内 blob（按名） |
| `image` ⬜待接线 | `ref` | `oci-client` pull 镜像 blob 进 OCI layout | 在线：target 自行 pull；离线：推镜像 blob + 运行时 import |
| `os_package` ⬜待接线 | `packages{os:[...]}` | 在目标 OS 容器/chroot 里 `apt-get download` / `dnf download` 解依赖下 `.deb`/`.rpm` | 在线：包管理器直装；离线：摆本地 file repo 从本地装 |

`build` **只读 `materials` 段**（`collect_materials`），install 段藏 `run_cmd` 也不影响
打包完整性——这是问题一的解。

## 问题四：build 的输入维度（version × os）

`os_package` 按 OS 分叉（deb vs rpm），`binary`/`image` 可能按 arch 分叉。所以离线包不是
"一个文件管所有"，而是一个 **OCI image index** 按 `platform`/annotation 聚合多份 artifact：

```
crater build --component mysql --version 8.0.36 --os ubuntu,rhel
  → index.json
     ├─ manifest(artifactType=crater.component, os=ubuntu) → mysql 的 .deb 物料层
     └─ manifest(artifactType=crater.component, os=rhel)   → mysql 的 .rpm 物料层
```

部署时引擎按目标机实际 OS/arch 选对应 manifest。yq 是纯二进制单一维度（amd64 一个就够），
所以没撞上；mysql 一定撞。

## 对比一眼

- `mysql.component.yaml`：纯 `os_package`、无 URL、按 OS 分叉——证明"扫 install 找不到物料"，
  且暴露 version×os 打包矩阵（问题四）。

## 接线状态 / 下一步

- ✅ `binary` 全链路（yq：在线 place + 离线 B 类 artifact recipe-replay）。
- ⬜ `image`：build 把 `images:`（现有字段）的 pull-into-OCI 复用过来按 material 打包 + 目标 import。
- ⬜ `os_package`：最硬——离线下 deb/rpm 的依赖解析 + 本地 repo 安装，按 OS 矩阵。
- ⬜ `crater build` 增 `--os` / 多版本输入维度 + image index 聚合。

建议下一个可证增量：先啃 `os_package`（mysql），打通离线"有 OS 包依赖"的组件。
