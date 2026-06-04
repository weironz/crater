# `requires:` 环境准入契约(distro / version / arch)

> ADR: D-102 ｜ 代码: `task.rs Requires`、`os.rs OsInfo`、`apply.rs`(全员预检)

## 这是什么

`crater apply` 的一个真坑:task 往往只在特定环境可用(os_package 闭包按 ubuntu:24.04
解析、镜像物料按 amd64 打包、厂商只认证 openEuler/麒麟),但引擎此前只认 Debian/RHEL
**两族**——发行版、版本、架构全裸奔,跑到一半才炸,甚至 `when_os` 静默跳过后报"成功"。

`requires:` 让 task 声明它支持什么(**纯数据**,守 D-036,无版本范围表达式):

```yaml
requires:
  os:                              # 任一条目匹配即可(OR);不写 = 不限
    - distro: ubuntu               # /etc/os-release 的 ID(发行版,不是族!)
      versions: ["22.04", "24.04"] # VERSION_ID;精确或前缀("9" 匹配 Rocky 9.4)
    - distro: openeuler            # versions 空 = 该发行版全版本
  arch: [amd64]                    # uname 拼法(x86_64)也认;不写 = 不限
```

## 三个时点,都在执行之前

「设了 requires 也要跑起来才知道」是误解——契约是静态数据,三道闸全在动手之前:

| 时点 | 连接 | 执行 | 回答什么 |
|---|---|---|---|
| `crater inspect <ref>` | 零 | 零 | 拿到制品先看「环境要求: ubuntu 22.04/24.04,arch amd64」 |
| `crater plan` | 连 | 零 | 这套机群兼容吗?不符直接拒 |
| apply **全员预检** | 连 | **零步骤** | 并发探测**所有**目标,一台不符 → 整体拒绝并**列出全部**不符主机 |

全员预检的关键:不是"跑到第 7 台才发现,前 6 台已经动过了"——任何一台不符,**一个
步骤都不执行**。

```console
$ crater apply -f x.yaml --host n11,n12 ...
Error: 准入失败:2/2 台目标不符,未执行任何步骤
  n12: 架构不符:目标是 amd64,task 要求 arm64
  n11: 架构不符:目标是 amd64,task 要求 arm64
```

## 验证(真机 192.168.73.11/.12,Ubuntu 24.04 amd64)

- 契约匹配 → `准入通过:1 台目标满足 requires(ubuntu 24.04,arch amd64)` → 正常执行。
- 要求 22.04 → `OS 不符:目标是 ubuntu 24.04,task 要求 ubuntu 22.04`,零步骤。
- 双主机 + arch arm64 → 两台**都列出**,整体拒绝。
- `plan` 同受此门;`inspect` 零连接显示契约。
- 配套堵了 **when_os 静默跳过坑**:action 全被过滤时 warn`0 步可执行…可能不在适用范围`,
  不再假装成功。

## 边界

- **teardown 豁免**:已部署的东西永远可以删(契约收紧不应锁死卸载)。
- `--dry-run` 不连机,查不了(用 `plan`)。
- distro 匹配 `/etc/os-release` 的 `ID` 小写精确值;族级宽匹配(如"任何 debian 系")
  暂不提供——要宽就多列几个 distro,显式优于隐式。
- 库内示例:`library/rustfs` 声明 `arch: [amd64]`(镜像物料单 arch)。
