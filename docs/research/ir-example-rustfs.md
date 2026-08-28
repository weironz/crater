# 试金石 ①:rustfs 重写为 IR

> 承接 [ir-draft.md](ir-draft.md)。目标不是"写得好看",是**用真实交付撞 schema**,把别扭的
> 地方揪出来。对照物:[library/rustfs/rustfs.yaml](../../library/rustfs/rustfs.yaml)(现行,143 行)。

---

## 1. 重写结果(blueprint)

```yaml
# blueprints/rustfs/blueprint.yaml —— S3 兼容对象存储,二进制 + systemd
name: rustfs
version: 1.0.0-beta.7

params:
  version:     { default: "1.0.0-beta.7", stage: build, desc: RustFS 版本(release tag) }
  volumes:     { default: "/data/rustfs0", desc: "RUSTFS_VOLUMES 表达式(单盘/多盘/多机)" }
  data_dirs:   { type: [string], default: ["/data/rustfs0"], desc: 本机数据目录 }
  port:        { type: int, default: 9000 }
  access_key:  { default: crater-admin }
  secret_key:  { default: crater-changeme, secret: true }      # ← secret: 日志/孪生里自动打码
  bypass_disk_check: { type: bool, default: false, desc: "仅测试:跳过物理磁盘独立性校验" }

materials:
  - name: rustfs-bin                                            # 双 arch 变体,zip 内取二进制
    file: "https://github.com/rustfs/rustfs/releases/download/${params.version}/rustfs-linux-${substrate.arch == 'arm64' ? 'aarch64' : 'x86_64'}-musl-v${params.version}.zip"
    unzip: rustfs

preflight:                                                      # 只读准入断言,不是资源
  - assert: port_owner(9000) in ["", "rustfs.service"]          # 被别人占 → 拒绝(见 §3-A)
    msg: "9000 被非 rustfs 进程占用"
  - assert: port_owner(9001) in ["", "rustfs.service"]
    msg: "9001 被非 rustfs 进程占用"

resources:
  - file: { path: "${item}", state: directory, mode: "0750" }
    each: params.data_dirs

  - file: { path: /var/logs/rustfs, state: directory, mode: "0750" }

  - copy: { material: rustfs-bin, dest: /usr/local/bin/rustfs, mode: "0755" }

  - copy:
      dest: /etc/default/rustfs
      mode: "0600"
      content: |
        RUSTFS_ACCESS_KEY=${params.access_key}
        RUSTFS_SECRET_KEY=${params.secret_key}
        RUSTFS_VOLUMES="${params.volumes}"
        RUSTFS_ADDRESS=":${params.port}"
        RUSTFS_CONSOLE_ENABLE=true
        RUSTFS_UNSAFE_BYPASS_DISK_CHECK=${params.bypass_disk_check}
        RUST_LOG=error
        RUSTFS_OBS_LOG_DIRECTORY="/var/logs/rustfs/"

  - systemd_unit:                                               # 见 §3-C:unit 是类型,不是 copy
      name: rustfs
      description: RustFS S3-compatible object storage (crater)
      after: [network-online.target]
      environment_file: /etc/default/rustfs
      exec_start: /usr/local/bin/rustfs $RUSTFS_VOLUMES
      restart: on-failure
      limits: { NOFILE: 1048576 }

  - service: { name: rustfs, state: started, enabled: true }
    # 无 deps:声明顺序即依赖;重启由「上游资源 changed」自动触发(见 §3-B,取代 notify)

health:                                                         # verify/drift 的依据,只读
  - service_active: rustfs                                      # 先核身份,防冒名顶替
  - http: { url: "http://localhost:${params.port}/health", status: 200 }
    timeout: 30s                                                # 多机首启组建纠删码集需等
```

**43 行 vs 143 行(-70%)**,且没有一处 `id:`/`needs:`/`phase:`/`notify:`/`teardown:`。

## 2. 消失的东西(以及它们去哪了)

| 现行写法 | 新形态 | 为什么能消失 |
|---|---|---|
| `teardown:` 6 步(停服务/删 unit/删 config/删二进制/删日志/rm -rf 数据) | **全删** | destroy() 是五动词契约 → 引擎逆序调用每个资源的 destroy。数据目录本就是 `file` 资源,删它是推论 |
| `phase: preflight/verify` 三段 | `preflight:` 断言 + `health:` 探针 | 阶段不是资源的属性,是**动作的种类**:准入是断言、验收是健康探针,都只读 |
| `id:` × 12、`needs:` × 8 | 0 | 声明顺序即依赖;需要跨资源并发/乱序时才写 `deps:` |
| `notify: [restart_rustfs]` × 3 + handler | 0 | 二进制/config/unit 任一 changed → service 资源的 observe 发现指纹变了 → 自然 restart。**handler 是 push 模型的补丁,对账模型不需要**(§3-B) |
| `action: shell` mkdir + check(data_dirs) | `file` × each | 类型化后自带 observe/destroy;check 不用手写 |
| `wait_for` × 2(port_free / wait_ready) | preflight 断言 / health 探针 | 等待是探针的重试语义,不是一种资源 |
| `verify` 那条 60 字符 shell(systemctl + curl) | `health:` 两条声明 | 身份核验 + HTTP 探针都是内建健康类型 |
| `{{item}}` / `loop:` | `each:` + `item`(CEL 作用域) | 同义,但类型不再靠字符串替换糊(现行是"序列化→字符串替换→反序列化") |
| 双 material(amd64/arm64 各一条) | 单条 + CEL 三元 | 变体是表达式,不是重复声明 |

## 3. 撞出来的 schema 问题(本次演练的真正产出)

### A. `preflight` 断言需要"只读探针函数",且严格语义是个真 bug 源

现行 `port_free`(wait_for state: stopped)在**已装好并运行的机器上重跑会失败**——rustfs 自己
就占着 9000。严格语义下 apply 不幂等,这与幂等承诺冲突(现行靠"先 delete"绕开)。

对账模型天然给出正解:断言的不是"端口空闲",是"**端口没被别人占**"。因此 IR 需要一小组
**只读探针函数**供 CEL 调用(`port_owner(p)` / `path_exists(p)` / `cmd_ok(s)`),在 substrate 上求值。
**裁定**:v0 引入 `preflight: [{assert: <CEL>, msg}]`,函数集封闭(白名单,可扩展但不开放
任意求值),保持"非图灵完备 + 可静态检查"。

### B. handler/notify 可以彻底删掉——但要求"资源指纹传播"

对账模型里 handler 是多余的:service 的 observe 应包含**它所依赖的输入指纹**(二进制 sha256 +
config 内容 hash + unit 内容 hash),任一变化 ⇒ diff 非空 ⇒ restart。这正是现行
`docker_container` 已经在用的 `crater.spec` 标签指纹法(D-092),把它**从一个模块的技巧
提升为引擎的通用机制**。

**裁定**:引擎按 deps 图把上游资源的 outcome 指纹喂给下游 observe;`service` 类型声明
`fingerprint_inputs: [自动:所有上游 changed 的资源]`。v0 先做"上游任一 changed ⇒ 下游
service 需 restart"的保守规则,精确到字段的传播留 P1。
副作用:**handler 概念从 IR 中删除**(ansible 前端的 `notify:` 编译成一条 deps 边)。

### C. `copy` 一坨 systemd unit 文本 → 需要 `systemd_unit` 类型

现行用 `copy` + 内联 INI 文本写 unit(19 行占了全文 13%),缺点:不可校验(拼错字段要等
systemd 报)、不可 diff(整文件 hash)、不可跨发行版。**裁定**:v0 内建资源类型加
`systemd_unit`(字段即 unit 段落,observe = 读回并解析比对)。这是"类型化 > 文本化"的
典型收益,也提示 L1 内建模块清单要按**这个层次**(而非 ansible 模块表)重新拟。

### D. params 需要 `type` 与 `secret`

`data_dirs` 现行是"空格分隔字符串"(为了 shell 分词),本质是列表被压扁——类型化后应是
`[string]`,`each` 直接吃。`secret_key` 需要 `secret: true`:孪生视图/Run 日志/API 响应里
自动打码。**裁定**:params schema v0 支持 `type`(string/int/bool/[T]/enum)+ `secret` +
`stage`(build|deploy,承接 D-093)+ `desc` + `default` + `required`。

### E. `stage: apply` 应改名 `deploy`

七名词里没有 "apply" 这个名词(apply 是动词/动作),params 分期应说 `build` vs `deploy`。
纯命名,但趁 v0 改。

### F. 没撞到问题的部分(可以放心冻结)

`each`/`when`/CEL 插值/materials 变体/Selector(本例全 `all` 未考)/health 探针 —— 表达顺畅。

## 4. 操作者视角(同一 blueprint 的另一张脸)

```console
$ x install rustfs --env prod --set volumes='http://node{1...4}:9000/data/rustfs{0...3}' \
                              --set data_dirs='["/data/rustfs0","/data/rustfs1"]'
plan: 4 substrates × 6 resources → +22 create ~2 update
      物料 rustfs-bin@arm64 1 层 118MB(air-gap 完备 ✓)
      preflight ✓ 端口 9000/9001 无他占用
approve? y
$ x status rustfs --env prod
  service/rustfs        ✓ active   (4/4 substrate)
  http /health          ✓ 200      (4/4)
  drift                 none       (last verified 3m ago)
```

## 5. 结论

IR 形状**成立**:资源模型不仅表达得下,还删掉了 70% 的样板(teardown/handler/phase/id/needs
全部成为推论)。撞出 6 个 schema 修正(A–E 需改,F 冻结),已全部裁定,回填 ir-draft.md。
下一站:**k8s-ha**——考 Selector(多角色)、init/join 次序、以及最难的 procedure(升级 dance)。
