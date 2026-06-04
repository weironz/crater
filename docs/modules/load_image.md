# load_image —— 导入容器镜像物料

crater 自有模块(Ansible 无内置对应):把 `kind: image` 物料弄进目标机的容器运行时。
在线 → 运行时自己 pull;离线 → 控制面推打包的 oci-archive,运行时 import。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `material` | 二选一 | 单个 `kind: image` 物料名 |
| `materials` | 二选一 | 批量(D-074):共享一次运行时探测与 namespace |
| `namespace` | | 传给 ctr/nerdctl 的 `-n`(如 `k8s.io`);docker/podman 忽略 |
| `runtime` | | 指定运行时;缺省按 nerdctl → ctr → docker → podman 顺序探测 |

## 语义 / 幂等

- 物料的 `ref` 即镜像引用(`{{var}}` 可渲染);build 时 pull 成 oci-archive blob 打进 OCI。
- 三态:有 blob → 推送 + import;无 blob + 严格离线 → 报错;无 blob + 在线 → 运行时 pull。
- 目标机已有同 ref 镜像 → 跳过(运行时 images 探针)。
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
