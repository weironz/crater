# crater 常用入口。`just` 不带参数 = 列出全部配方。

# ~/.cargo/config.toml 配了 -fuse-ld=mold(D-079 提速),但没装 mold 的机器
# 会直接链接失败。空 RUSTFLAGS 覆盖掉它:装了 mold 想要提速,删这行即可。
export RUSTFLAGS := ""

# 一键拉起 UI:`just ui`;换端口 `just ui 9000`,对外开放 `just ui 8899 0.0.0.0`
# (注意:UI 无认证,0.0.0.0 仅限可信内网)。
ui port="8899" bind="127.0.0.1":
    cargo build --release -p crater-cli
    ./target/release/crater ui --bind {{bind}} --port {{port}}

# release 构建
build:
    cargo build --release

# 全量测试
test:
    cargo test --release
