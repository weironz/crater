# 卸载/重置 `crater delete`（由 task 的 `teardown:` 驱动，D-049）

## 这是什么 / 为什么不自动逆向

`crater delete <source>` 是 apply 的反向操作（卸载/重置），但它**不是**把 apply 的步骤倒着撤——
而是跑 task **作者声明**的一段 `teardown:` 动作。

为什么不自动逆向 `actions:`：**真实软件的清理对象多是运行时自己生成的状态，install 步骤从没创建过**，
逆推装的步骤永远碰不到它们:

| 产品 | install 定义里有的 | 运行时长出来的（定义里没有） | 正确清理 |
|---|---|---|---|
| k8s | kubeadm init、装包 | etcd 数据、CNI 网卡、iptables/ipvs | **`kubeadm reset`** + 删目录 |
| mysql | 装包、写 my.cnf | `/var/lib/mysql`（mysqld 首次 init 建的） | stop + purge + 删数据目录 |
| docker | copy 二进制、写 unit | `/var/lib/docker`、`/var/lib/containerd`（镜像/容器/卷） | stop + 删数据目录 + 删 binary/unit |

`kubeadm reset` 就是铁证——k8s 专门写了 reset，因为 init 撤不出来。所以清理是**产品知识 → 数据**
（和「装是数据」对称，守 D-017）：写成 `teardown:`，引擎只负责跑，用的还是同一套幂等原语
（`file: absent`/`service: stopped` 天生幂等）+ dry-run + 多机 + agent + 离线。

## opt-in：不声明 `teardown:` 就没有 delete 能力

**delete 不强制**。task 没写 `teardown:` → `crater delete` 明确报错,**绝不**拿 `actions:`
自动逆向兜底(那只会残留、给虚假的「已清干净」安全感)。

```bash
crater delete yq
# Error: task 'yq' defines no `teardown:` — it has no delete capability
#        (delete is opt-in; author a teardown to enable it)
```

## 基本 demo（docker，含运行时状态清理）

`tasks/docker.yaml` 声明了 `teardown:`（stop 服务 → 删 `/var/lib/docker`+`/var/lib/containerd`
→ 删 binary/unit/`/etc/docker` → daemon-reload）：

```bash
crater delete docker --dry-run                  # 只看清理计划(12 步,按 needs 拓扑序)
crater delete docker --host 192.168.73.12 --user root --password 123456
#  teardown 'docker': 12 action(s) ...
#  [1/12] service docker → changed         # stop+disable
#  [4/12] remove /var/lib/docker → changed # ← 运行时数据,作者声明才清得掉
#  ...
#  [12/12] run: systemctl daemon-reload → changed
#  done: changed=12
crater delete docker --host ... # 再删 → changed=1 ok=11(file-absent/service-stopped 幂等)
```

source 形态与 apply 一致:命名 task、`task.yaml`、`x.oci`、镜像 ref(artifact 的 recipe 含
teardown,离线也能 delete)。

## 验证（真机 192.168.73.12）

- `delete yq` → 报错(无 teardown,opt-in 生效)。
- `delete docker --host .12`(.12 已装 docker)→ 12 步全 changed;事后 `docker`/`containerd`
  `inactive`,`/usr/local/bin/dockerd`、`/var/lib/docker`、`/var/lib/containerd`、`/etc/docker`
  **全部不存在**。再删一次 `changed=1 ok=11`(仅 daemon-reload 重跑,余皆幂等 ok)。

## 边界 / 后续

- teardown 是**声明数据**:作者要写对(尤其运行时数据目录的路径是产品知识)。引擎不替你发明清理逻辑。
- `run_cmd`(如 `kubeadm reset`/`daemon-reload`)无 `check:` 则每次重跑——清理场景多数无害;要严格幂等可给 `check:`。
- 没做、也不打算做:对无 `teardown:` 的 task 自动逆向 `actions:`(见上,必然不一致)。

## 关联

- ADR：[D-049](../decisions.md)（delete/teardown）、[D-017](../decisions.md)（引擎零产品知识）、[D-036](../decisions.md)（YAML 纯数据）。
- 相关：[action-tasks.md](action-tasks.md)、[idempotency-and-apply.md](idempotency-and-apply.md)。
