# wait_for —— 等条件成立再继续

等 TCP 端口开/关、或路径出现/消失;条件成立立即放行,超时**响亮失败**。
对齐 ansible `wait_for`,取代手搓的 `for i in $(seq 1 30); do …; sleep 1; done`。

## 参数

| 字段 | 必填 | 说明 |
|---|---|---|
| `port` | 二选一 | TCP 连接探测(**在目标机上发起**);过 `{{var}}` 渲染(端口常是 apply 参数) |
| `path` | 二选一 | 路径存在性探测(`test -e`) |
| `host` | | 仅配 `port`,默认 `127.0.0.1`(目标机自己);过 `{{var}}` 渲染 |
| `state` | | `started`/`present` = 开/存在(**默认**);`stopped`/`absent` = 关/消失。四个名字两两同义,按 port/path 选读着顺的 |
| `timeout` | | 探测窗口秒数,默认 30;到点报 `wait_for 超时(Ns): …` 并失败 |
| `delay` | | 首次探测前先睡几秒,默认 0 |

## 语义 / 幂等

- **只读**:lower 成纯探测循环(每秒一次),不改变任何状态。
- 单次探针同时充当 `check:`——条件**已经成立**则整步跳过报 `ok`(连 `delay` 都省);
  要等才进循环(等到了报 `changed`,语义是"这步真等了一会儿")。`crater plan`
  跑的也是这个探针(✓ 已成立 / ~ 要等)。
- 端口探测链:目标机有 `nc` 用 `nc -z -w 2`(含 busybox),否则
  `bash -c 'exec 3<>/dev/tcp/…'` 兜底(步骤经 `sh` 跑,dash 没有 /dev/tcp,
  必须显式过 bash)。两者皆无 → 探测一直失败 → 超时报错,不会误判成功。
- 典型位置:`phase: verify` 等服务开门(rustfs 即此用法);普通步骤等依赖就绪
  (如等 etcd 9279 再起下一个);`state: stopped` 等旧进程退出/排空。

## 示例

```yaml
- id: wait_api
  action: wait_for
  port: "{{port}}"        # 渲染自 params/vars;直接写 9000 也行
  timeout: 30
  phase: verify
  needs: [svc]

- id: old_gone
  action: wait_for
  port: 8080
  state: stopped          # 等旧进程让出端口
  timeout: 60

- id: sock_up
  action: wait_for
  path: /var/run/app.sock
```

## 关联

ADR:D-104。相关:[shell](shell.md)(`check:` 探针)、[service](service.md)、
[plan](../features/plan.md)(只读探针同源)。
