# role —— 调用可复用角色

对齐 Ansible 的 `include_role`:`uses` 指向 `roles/<名>`,`with` 传参。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `uses` | ✔ | 角色名:task 同级 `roles/<uses>/role.yaml`(目录式,可带私有 files/、templates/,D-086)或 `roles/<uses>.yaml` |
| `with` | | 参数映射,渲染进角色内部 |

## 语义

- **bundle 角色**(有 `actions:`):**计划前整体展开**(D-080)——动作内联进调用方 task,id/物料名加 `<调用步 id>.` 前缀,内部 needs 同步改写;入口动作继承调用步的 needs/notify。build 出的 OCI recipe 已是展平形态(离线无 roles 目录依赖)。
- **thin 角色**(`check` + `act` 模板):lower 成一条带探针的 shell(D-029 遗留形态)。
- 角色自带 `params:` 契约,`with` 缺必填参数报错。

## 示例

```yaml
# library/demo/demo.yaml
actions:
  - id: install_yq
    action: role
    uses: install-binary
    with:
      url: "https://.../yq_linux_amd64"
      dest: /usr/local/bin/yq
```

## 关联

ADR:D-029(四层模型)、D-080(展开式 role)、D-086(目录式角色/自包含交付)。详见 [roles](../features/roles.md)。
