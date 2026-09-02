# 安装 crater

crater 是一个 **musl 静态二进制**,没有运行时依赖 —— 不需要 Python、不需要
装模块库、目标机上什么都不用预装。装它就是把一个文件放进 `PATH`。

## 一行装好(Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/weironz/crater/main/scripts/install.sh | sh
```

默认装到 `~/.local/bin/crater`,**不需要 sudo**。装完会自己跑一次
`crater --version` 确认能用;如果 `~/.local/bin` 不在你的 `PATH` 里,脚本会
把该加的那一行打出来。

装到别处、或者钉住某一版:

```bash
# 装到系统目录(这一步才需要 sudo)
curl -fsSL https://raw.githubusercontent.com/weironz/crater/main/scripts/install.sh | sudo sh -s -- --prefix /usr/local/bin

# 钉住版本(生产环境建议这样,别跟着 latest 漂)
curl -fsSL https://raw.githubusercontent.com/weironz/crater/main/scripts/install.sh | sh -s -- --version v0.2.0
```

支持 `x86_64` 与 `aarch64`(鲲鹏 / 飞腾 / Graviton / 树莓派)。

### 这个脚本做了什么

管道执行一个网上的脚本,你有权知道它到底干了什么。四步,没有别的:

1. 认平台(`uname -s` / `uname -m`),不是 Linux 就退出;
2. 从 GitHub Release 下 `crater-<target>.tar.gz` 和 `SHA256SUMS`;
3. **核对摘要** —— 对不上就退出,什么都不装;
4. 解包,`install -m 0755` 原子写入,再跑一次 `--version` 自检。

它**不**碰你的 shell 配置文件、**不**装别的东西、**不**往家目录以外写、
默认**不**要 root。

### 摘要校验没有关闭开关

这是有意的。这个脚本从网上取一个二进制、然后让你执行它 —— 跳过校验等于把
"信道没被动过"当成理所当然。发版流程本来就产出 `SHA256SUMS`,校验的成本是
零。所以没有 `--no-verify`。

摘要对不上时脚本会打出期望值与实得值然后**退出码 1**,不会"先装上再说"。

### 不想用管道

完全合理 —— 管道执行是信任脚本作者,先看再跑是信任你自己:

```bash
curl -fsSLO https://raw.githubusercontent.com/weironz/crater/main/scripts/install.sh
less install.sh          # 看完再决定
sh install.sh
```

### 手动装(不跑任何脚本)

```bash
ver=v0.2.0
target=x86_64-unknown-linux-musl        # 或 aarch64-unknown-linux-musl
base=https://github.com/weironz/crater/releases/download/$ver

curl -fsSLO $base/crater-$target.tar.gz
curl -fsSLO $base/SHA256SUMS
grep " crater-$target.tar.gz\$" SHA256SUMS | sha256sum -c -   # 必须 OK
tar -xzf crater-$target.tar.gz
install -m 0755 crater ~/.local/bin/crater
```

## 升级:`crater update`

装好之后,以后升级不用再回来找安装脚本:

```bash
crater update            # 换成最新版
crater update --check    # 只看有没有新版,不换
crater update --version v0.2.0   # 换到指定版本(也能降级)
```

它和安装脚本**是同一套规矩**:同样从 GitHub Release 取 musl 包、同样核对
`SHA256SUMS`、同样原子替换。三点值得知道:

- **装在哪就换哪。** 它换的是 `current_exe()` 那个文件,不猜 `PATH`。你同时
  有 `~/.local/bin/crater` 和 `/usr/local/bin/crater` 的话,换的是你刚才敲的
  那一个。装在系统目录里就需要 `sudo crater update`。
- **换完立刻自检。** 换上去马上跑一次 `--version`,不这么做的话"换成了一个
  跑不起来的二进制"要等到下次用才发现。
- **原子替换。** 先写同目录的临时文件再 `rename`,中途断网留下的是旧的那个,
  不是半个。

### 现在所有已发布的版本都还没有 `update`

**说清楚**:`crater update` 是 v0.2.0 **发布之后**才写的,所以从 Release
装下来的 v0.2.0 里没有它,敲了会报 `unrecognized subcommand 'update'`。
更老的 0.1.x 会报 `not a file, image ref, or named task/project`。两种都不是
坏了,是那个版本里根本没有这个命令。

**下一个版本起才能自更新。** 在那之前(以及任何时候从更老的版本升上来),
都用本页开头那条安装脚本 —— 它对已经装过的机器同样有效,直接覆盖。

## Windows / macOS:目前不支持

说实话:**没有 Windows 和 macOS 的产物,也没有 `install.ps1`。**

不是没想过。2026-09-02 在 `windows-latest` 上实测编了一次,失败在我们自己的
代码上,不是依赖或者环境:

| 位置 | 用了什么 | Windows 上为什么不成立 |
| --- | --- | --- |
| `ui_run.rs` | `pre_exec` + `setsid()` | Windows 没有进程组这个模型,对应物是 Job Object |
| `ui_run.rs` | `kill(-pid, SIGTERM)` | 同上,取消运行要重写 |
| `pkg.rs` | `PermissionsExt::mode()` | Windows 没有 mode 位,包格式记什么要重新定义 |
| `ui_run.rs` | `Command::new("sh")` | 编得过也跑不起来 |

编译错只有 4 处,但**能编 ≠ 能用**:全仓还有 88 处 `sh -c` / `/tmp` / `/etc`
这类路径与 shell 假设。真做 Windows 是一次移植,不是加几个 `#[cfg]`。在那之前
发一个 `install.ps1` 只会让人装上一个跑不起来的东西。

**Windows 上现在怎么用:走 WSL。** crater 是 agentless 的 —— 它在控制端跑,
通过 SSH 操作目标机,所以控制端在 WSL 里完全够用:

```powershell
wsl --install            # 没装过 WSL 的话
```

进 WSL 之后就是上面那条 Linux 一行命令。

**macOS 同理**:没有产物,但可以从源码构建(`cargo build --release`)。
一样的问题 —— 能编不代表跑得对,没在 macOS 上验过,别用在生产上。

## 从源码构建

```bash
git clone https://github.com/weironz/crater && cd crater
cargo build --release              # → target/release/crater
scripts/build-musl.sh all          # 双架构 musl 静态 → dist/
```

## 装完之后

```bash
crater types                       # 能声明哪 26 种资源
crater lint <蓝图>                 # 静态检查,不连机器
crater plan -f <蓝图> -i <机群>    # 零写入预演:会变什么,先看清楚
```

## 卸载

```bash
rm ~/.local/bin/crater             # 或者你装的那个路径
rm -rf ~/.crater                   # 缓存、仓库索引、装好的包(想留就别删)
```

---

## 给维护者:两处规矩,改一处要改两处

`scripts/install.sh` 与 `crates/crater-cli/src/update.rs` 实现的是**同一套
规矩**(取 musl 包 → 核对 SHA256SUMS → 原子替换 → 自检),只是入口不同:一个
是"还没有 crater 的时候",一个是"已经有了"。

它们不能合并 —— 安装脚本必须在没有 crater 的机器上跑,所以只能是 shell。
所以规矩写了两遍,**会漂**。改动任何一条(比如换了产物命名、加了签名校验),
两处都要改,并且这份文档里的说明也要跟着改。
