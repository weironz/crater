# package —— 安装系统包

OS 族(debian/rhel)各列各的包名。在线走系统源;离线走打包好的 deb/rpm 闭包。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `packages` | 二选一 | `{debian: [..], rhel: [..]}`,按目标 OS 族取 |
| `material` | 二选一 | `kind: os_package` 物料名:包列表声明在物料上,build 时解依赖闭包下齐 deb/rpm 打进 OCI(D-062,需 buildah) |

## 语义 / 幂等

- 在线:`apt-get install -y` / `dnf install -y`;探针 `dpkg -s` / `rpm -q`(全部已装 → ok)。
- 离线(material 有 blob):推送闭包到目标,`apt-get install ./*.deb` / `dnf install ./*.rpm`(本地装,不出网)。
- 三态同 copy:有 blob 用 blob;无 blob + 严格离线报错;无 blob + 在线走系统源。
- **日志标来源**:`install packages (blob): [..]` = 推包内闭包本地装;`(target repo)` = 目标机自己的源在线装。

## 示例

```yaml
- action: package
  packages:
    debian: [keepalived, haproxy]
    rhel: [keepalived, haproxy]
```

## 关联

ADR:D-048/D-061/D-062(os_package 物料)、D-087(三态)。相关:[materials](../features/materials.md)。
