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
| `throttle: 1` | cp2 与 cp3 的区间 **不相交**(容差见下)—— 哪怕 `--parallel` 开得再大 |
| 不限流 + `--parallel 2` | w1 与 w2 的区间 **相交**,墙钟约等于单台耗时而非两倍 |
| 选择器边界 | `seed ∪ followers` 恰好等于 `masters`,不多不少 |
| 并发不改语义 | 两遍的执行报告 **逐字相同** |

## 判据必须带容差:时钟偏差是精度下限

时间戳由**各台机器自己**记录,所以跨主机比区间的精度下限,就是节点间的时钟偏差。
把判据写成"必须恰好不相交"会在时钟未对齐的机群上误报 —— 刚从快照恢复、NTP 尚未
收敛的虚机,偏差到百毫秒量级是常态。

**串行那遍本身就是标定器。** 串行执行的顺序严格 `cp2 → cp3 → w1 → w2`,物理上
不可能并发,所以相邻两台之间出现的**负间隔只可能来自时钟偏差**:

```
串行实测(VMware,刚恢复快照)     cp2 → cp3    +10 ms
                                  cp3 → w1    +180 ms
                                  w1  → w2    −124 ms   ← 偏差下界
```

于是判据应当是:

- 先从**串行**那遍取出最大负间隔 `skew`(没有负间隔则 `skew = 0`);
- `throttle` 判定:并发遍的 `cp2 ∩ cp3 <= skew` 即视为限流生效;
- 并发判定:`w1 ∩ w2 >> skew` 才算真并发 —— 上面那次实测 3937ms vs 124ms,
  信噪比 30 倍。

**把 `hold` 调大是最简单的加固**:它同时放大信号而不放大偏差。默认 4 秒在
百毫秒偏差下已经够用;机群时钟很脏时调到 10 秒。

`throttle: 1` 那条最关键:它护的是 etcd 的仲裁。在没有真机的年代,串行执行让任何
throttle 都**平凡成立** —— 看起来通过,其实什么都没验。

## 参考结果

**multipass,5×768MB/2vCPU,hold=4s**(同宿主机,时钟一致):

```
── 串行 --parallel 1 ──          ── 并发 --parallel 2 ──
  cp2   0.00→ 4.01s                cp2   0.00→ 4.00s
  cp3   4.03→ 8.04s                cp3   4.03→ 8.03s     cp2∩cp3 = 0.000s
  w1    8.04→12.05s                w1    8.04→12.04s
  w2   12.07→16.07s                w2    8.03→12.04s     w1 ∩w2  = 4.001s
  worker 墙钟 = 8.03s              worker 墙钟 = 4.01s     skew = 0 ms
```

**VMware Workstation,5×2vCPU/4G,经 SSH 隧道,hold=4s**(跨物理机,时钟未收敛):

```
── 串行 --parallel 1 ──          ── 并发 --parallel 2 ──
  cp2   0.00→ 4.01s                cp2   0.00→ 4.00s
  cp3   4.02→ 8.03s                cp3   4.01→ 8.02s     cp2∩cp3 = 0.000s
  w1    8.21→12.23s                w1    8.03→12.03s
  w2   12.11→16.46s                w2    8.09→12.10s     w1 ∩w2  = 3.937s
  worker 墙钟 = 8.25s              worker 墙钟 = 4.07s     skew = 124 ms
```

两套环境结论一致。第二套的 `skew = 124ms` 正是从串行遍的 `w1 → w2` 负间隔读出来的
—— 若把判据写成"恰好不相交",它会在这里误报一次限流失效。

## 蓝图里刻意不写 `check:`

lint 会为此报 4 条 warn,这是**对的**:计时探针必须每次都真跑,有了 `check:`
第二遍就成 noop,也就再没有区间可比。warn 不阻断执行。
