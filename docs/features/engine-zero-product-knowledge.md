# 引擎零产品知识（装万物地基）

> ADR: D-017 ｜ 设计: [design.md §1](../design.md)

## 这是什么

crater 的第一性原理：**引擎（Rust）只懂「怎么做」（通用原语），不懂「做什么」**。docker/k3s/mysql 的名字、服务名、别名、镜像源、诊断规则——全是**数据**（YAML），不在代码里。

> 加一个可部署对象 = 丢一个 `component.yaml`，**绝不改 Rust 重编译**。这和 ansible-core 不知道 nginx 是什么是同一架构。

| 允许在代码 | 必须是数据 |
|---|---|
| download/extract/template/service/cmd/module… 通用原语 | 产品名/服务名/别名/镜像源/诊断规则/依赖 |

## 基本 demo

加一个全新可部署对象（yq），只丢一个数据文件，零 Rust：

```bash
cat > components/yq/component.yaml <<'YAML'
name: yq
version_default: "4.53.2"
install:
  - action: download
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64"
    dest: /usr/local/bin/yq
  - action: run_cmd
    cmd: "chmod +x /usr/local/bin/yq"
    check: "test -x /usr/local/bin/yq"
verify:
  - action: run_cmd
    cmd: "/usr/local/bin/yq --version"
YAML

crater yq --host <host> --password <pw> --dry-run   # 立即可用，无需编译
```

别名也是数据：`k3s/component.yaml` 里 `aliases: [k8s, kubernetes]` 让 `crater k8s` 生效（`resolve_component` 扫 `components/` 建表，引擎不认识 k8s）。

## 验证

`crater k8s` 经组件数据别名解析到 k3s；引擎源码内搜不到任何 docker/k3s/mysql 的**逻辑**（仅测试夹具用作示例名）。

## 边界 / 后续

- 通用原语集（Rust enum）本身是"模块库"，扩充走 [modules.md](modules.md) 的四层模型，多数情况零 Rust。
- 镜像源默认值放 `crater-core/src/mirrors.default.yaml`（数据，`$CRATER_MIRRORS`/`./mirrors.yaml` 可覆盖）。
