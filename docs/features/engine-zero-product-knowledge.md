# 引擎零产品知识（装万物地基）

> ADR: D-017 ｜ 设计: [design.md §1](../design.md)

## 这是什么

crater 的第一性原理：**引擎（Rust）只懂「怎么做」（通用原语），不懂「做什么」**。装 docker/mysql 的名字、服务名、镜像源、诊断规则——全是**数据**（task YAML），不在代码里。

> 加一个可部署对象 = 写一个 `tasks/<name>.yaml`（task），**绝不改 Rust 重编译**。这和 ansible-core 不知道 nginx 是什么是同一架构。

| 允许在代码 | 必须是数据 |
|---|---|
| place/run_cmd/file/copy/service/extract/template/module… 通用原语 | 产品名/服务名/镜像源/诊断规则/依赖 |

## 基本 demo

加一个全新可部署对象（yq），只写一个 task，零 Rust：

```bash
cat > tasks/yq.yaml <<'YAML'
name: yq
hosts: all
vars:
  version: "4.53.2"
materials:
  - name: yq-bin
    kind: file
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64"
actions:
  - id: place
    action: place
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"
  - id: verify
    action: run_cmd
    phase: verify
    cmd: "/usr/local/bin/yq --version"
    needs: [place]
YAML

crater apply yq --host <host> --password <pw> --dry-run   # 立即可用，无需编译
```

## 验证

引擎源码内搜不到任何 docker/mysql 的**逻辑**（仅测试夹具用作示例名）；新增可部署对象只动 `tasks/`。

## 边界 / 后续

- 通用原语集（Rust enum）本身是"模块库"，扩充走 [modules.md](modules.md) 的四层模型，多数情况零 Rust。
- 镜像源默认值放 `crater-core/src/mirrors.default.yaml`（数据，`$CRATER_MIRRORS`/`./mirrors.yaml` 可覆盖）。
