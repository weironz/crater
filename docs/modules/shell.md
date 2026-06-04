# shell —— 经 shell 跑命令

对齐 Ansible 的 `shell`(**不是** `command`):管道、`&&`、重定向、`ENV=x cmd` 前缀都支持。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `cmd` | ✔ | 命令,`{{var}}` 字面替换后经目标机 shell 执行 |
| `check` | | 幂等探针(ansible `creates:` 风格):此命令退出 0 → 目标已达期望态,跳过 `cmd`(报 `ok`) |

## 语义 / 幂等

- 无 `check` 则每次都跑(报 `changed`)——非幂等命令(如 `daemon-reload`)宜放 **handler**,靠 `notify` 仅在变更时触发。
- `cmd`/`check` 都只做 `{{ 路径 }}` 字面替换,**不许表达式**(过滤器/运算符直接报错,D-036)。

## 示例

```yaml
- id: init
  action: shell
  cmd: "kubeadm init --pod-network-cidr {{pod_cidr}}"
  check: "test -f /etc/kubernetes/admin.conf"
- action: shell
  phase: verify
  cmd: "kubectl get nodes | grep -w Ready"
```

## 关联

ADR:D-067(名字对齐)、D-036(YAML 无逻辑)、D-017(幂等)。相关:[service](service.md)、handlers/notify([action-tasks](../features/action-tasks.md))。
