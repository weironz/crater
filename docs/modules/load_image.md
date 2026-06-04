# load_image —— 导入容器镜像物料

crater 自有模块(Ansible 无内置对应):把 `kind: image` 物料弄进目标机的容器运行时。
在线 → 运行时自己 pull;离线 → 控制面推打包的 oci-archive,运行时 import。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `material` | 二选一 | 单个 `kind: image` 物料名 |
| `materials` | 二选一 | 批量(D-074):共享一次运行时探测与 namespace |
| `namespace` | | 传给 ctr/nerdctl 的 `-n`(如 `k8s.io`);docker/podman 静默忽略 |
| `runtime` | | 用哪个 CLI 执行 pull/load,见下表;缺省在目标机按 `nerdctl → ctr → docker → podman` 顺序探测,全没有 → 报错 |

## runtime 取值与实际命令

`runtime` 的值**直接当命令名用,不做白名单校验**;已知名字有专门的命令形态,其余按
docker 兼容形态透传(自定义/别名 CLI 也能用,但它得兼容 `pull`/`load -i`):

| runtime | 在线(pull) | 离线(import 包内 oci-archive) | namespace |
|---|---|---|---|
| `ctr` | `ctr -n <ns> images pull <ref>` | `ctr -n <ns> images import <tar>` | ✔ |
| `nerdctl` | `nerdctl -n <ns> pull <ref>` | `nerdctl -n <ns> load -i <tar>` | ✔ |
| `docker` / `podman` / 其他任意值 X | `X pull <ref>` | `X load -i <tar>` | 忽略 |

整批(`materials: [..]`)共享同一个 runtime:显式值或一次探测,不逐镜像重选。

## 语义 / 幂等

- 物料的 `ref` 即镜像引用(`{{var}}` 可渲染);build 时 pull 成 oci-archive blob 打进 OCI。
- 三态:有 blob → 推送 + import;无 blob + 严格离线 → 报错;无 blob + 在线 → 运行时 pull。
- **无已有镜像探针**:每次执行 pull/load(报 `changed`)。pull 对已是最新的镜像是廉价操作
  (digest 比对),但 `:latest` 这类活动 tag 会真拉新版——要严格幂等就 pin 具体版本 tag。
- **日志标来源**:`load image (blob) <ref>` = 导入包内 oci-archive;`(pull)` = 运行时在线 pull;批量显示 `(N blob, M pull)`。

## 示例

```yaml
materials:
  - name: pause-img
    kind: image
    ref: "registry.k8s.io/pause:3.10"
actions:
  - action: load_image
    materials: [pause-img]
    namespace: k8s.io
```

## 关联

ADR:D-061(image 物料)、D-074(批量)。相关:[materials](../features/materials.md)、[package](package.md)(同为"闭包式依赖")。
