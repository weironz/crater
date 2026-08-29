# 机群自检 —— 在**你自己的机器**上验证机群机制

`fleet-check.blueprint.yaml` 不装任何东西。它只做一件事:让每台机器把自己动手的
**时间区间**写下来。

因为报告里看不出"限流有没有真限住、并发有没有真并起来"—— 报告只说做完了,不说
什么时候做的。唯一能证伪的证据是时间戳,而且必须是**目标机自己**记的,不能是控制
端的推断。

## 跑法

需要 ≥3 台 controlplane 与 ≥2 台 worker(少于这个数,有的机制根本无从观察 ——
2 台 controlplane 时 `rest()` 只剩 1 台,限流与不限流跑出来一模一样)。

```bash
crater procedure check -f fleet-check.blueprint.yaml -i inventory.yaml                # 串行基线
crater procedure check -f fleet-check.blueprint.yaml -i inventory.yaml --parallel 2   # 并发
```

两遍跑完,带外(不经 crater)把证据取回来比对:

```bash
for n in cp2 cp3; do ssh $n 'cat /opt/crater-check/follower.start /opt/crater-check/follower.end'; done
for n in w1  w2;  do ssh $n 'cat /opt/crater-check/worker.start   /opt/crater-check/worker.end';   done
```

## 该看到什么

| 检查 | 判据 |
| --- | --- |
| `first()` 只落一台 | 只有一台有 `/opt/crater-check/role.seed` |
| `exports:` 跨主机 | 4 个消费方的 `token.seen` **逐字节相同**,且带 seed 的 hostname |
| `throttle: 1` | cp2 与 cp3 的区间 **不相交** —— 哪怕 `--parallel` 开得再大 |
| 不限流 + `--parallel 2` | w1 与 w2 的区间 **相交**,墙钟约等于单台耗时而非两倍 |
| 选择器边界 | `seed ∪ followers` 恰好等于 `masters`,不多不少 |
| 并发不改语义 | 两遍的执行报告 **逐字相同** |

`throttle: 1` 那条最关键:它护的是 etcd 的仲裁。在没有真机的年代,串行执行让任何
throttle 都**平凡成立** —— 看起来通过,其实什么都没验。

## 参考结果(5 节点,768MB/2vCPU,hold=4s)

```
── 串行 --parallel 1 ──          ── 并发 --parallel 2 ──
  cp2   0.00→ 4.01s                cp2   0.00→ 4.00s
  cp3   4.03→ 8.04s                cp3   4.03→ 8.03s     cp2∩cp3 = 0.000s
  w1    8.04→12.05s                w1    8.04→12.04s
  w2   12.07→16.07s                w2    8.03→12.04s     w1 ∩w2  = 4.001s
  worker 墙钟 = 8.03s              worker 墙钟 = 4.01s
```

## 蓝图里刻意不写 `check:`

lint 会为此报 4 条 warn,这是**对的**:计时探针必须每次都真跑,有了 `check:`
第二遍就成 noop,也就再没有区间可比。warn 不阻断执行。
