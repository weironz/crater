# service —— 管理 systemd 服务

一步完成 daemon-reload + enable/disable + start/stop/restart。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `name` | ✔ | 服务名 |
| `state` | | `started` / `stopped` / `restarted` |
| `enabled` | | `true`(enable)/ `false`(disable) |

## 语义 / 幂等

- lower 成 `systemctl daemon-reload && [enable|disable] && [start|stop|restart]` 一条链。
- 探针只看 **state**:`started` → `is-active`、`stopped` → `! is-active`;`restarted` 永远跑(永远 `changed`)。
  **不**用 is-enabled 做探针——"已 enable 但 stopped"的服务必须仍被 start(D-075b 教训)。
- 首跑 unit 刚写入时,链内 daemon-reload 让 systemd 看见新 unit;重跑 is-active 命中 → 整步跳过,不重载(幂等)。
- `restarted` 用于 **handler**(配 unit/配置文件的 `notify`),不要放普通 action。

## 示例

```yaml
- id: svc_docker
  action: service
  name: docker
  state: started
  enabled: true
  needs: [docker_unit]
handlers:
  - id: restart_docker
    action: service
    name: docker
    state: restarted
```

## 关联

ADR:D-069(systemd_unit 并入 service)、D-075b(探针只看 state)。相关:handlers/notify([action-tasks](../features/action-tasks.md))。
