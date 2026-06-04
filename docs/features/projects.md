# project:有序 plays 编排多个 task(在线 + 离线一包)

> ADR: D-083(project/plays)、**D-098(离线 project)** ｜ 代码: `project.rs`、
> `build.rs build_project_to_store`、`store.rs export_oci_archive`、`apply.rs apply_oci_bundle`

## 这是什么

project 是 crater 的 **playbook**(ansible `site.yml`):有序 plays,每个 play 引用一个
task,可覆盖 `hosts`/`vars`。层级:project(编排)→ task(actions)→ role(复用子程序)。

```yaml
# demo-stack.yaml
name: demo-stack
plays:
  - name: 装 yq
    source: yq          # library/**/yq.yaml
    hosts: all
  - name: 部署 rustfs
    source: rustfs
    hosts: all
```

- **在线**:`crater apply -f demo-stack.yaml -i inv.yaml` —— plays 顺序执行(play 间屏障),
  delete 逆序;play 的 `hosts` 无匹配主机 → 跳过该 play 不整体中止。
- **离线(D-098)**:三步把整套环境装进**一个文件**:

```bash
crater build -f demo-stack.yaml        # ① 逐 play 构建 task 制品(D-096 缓存生效),
                                       #    再造项目制品:play source 锁定为 task ref
crater save crater/demo-stack:latest -o demo-stack.oci   # ② 项目 + 全部 task 闭包 → 一包
crater apply demo-stack.oci -i inv.yaml                   # ③ air-gap:按 play 顺序离线编排
```

## 关键机制

- **锁定(lock)**:build 把每个 play 的 `source: yq` 改写成 `source: crater/yq:4.44.3`
  烘焙进项目制品 recipe——离线 replay 按 ref 在包内精确找到对应 task 制品,不存在
  "线上又变了"的歧义。play `vars` 参与各 task 的 build 覆盖(可钉版本);两个 play 若
  构出**同 ref 但输入不同**会直接报错(锁定不允许静默指向后者)。
- **内容寻址去重**:bundle 是一个 OCI layout,所有 task 的 blob 共存于 `blobs/sha256/`
  ——跨 task 相同的物料(共享的镜像层/二进制)只存一份,零额外机制。
- **delete 逆序 + 优雅跳过**:没写 `teardown:` 的 play 跳过并提示(单 task delete 仍是
  硬错误,opt-in 语义不变)——多 play 卸载不会因一个可选 play 中断。
- 复用全管线:每个 play 的离线执行 = 标准 task 离线路径(blob 先推后 agent 跑 D-095、
  指纹收敛 D-092、register/hostvars/when_role 全部可用)。

## 验证(真机 192.168.73.11)

- build:yq **构建缓存命中**(瞬过)+ rustfs 打包,项目制品锁定 2 ref。
- save:115MB 单文件,index = 1 个 project artifact + 2 个 component artifact,11 blobs。
- apply .oci:`offline apply project 'demo-stack': 2 play(s)` → play1 yq changed →
  play2 rustfs 5 步(镜像离线导入、容器指纹收敛、api/console 双 200)→ 完成。
- 重跑:blob 全部 `cached, reusing`,changed=0(幂等贯穿)。
- delete .oci:逆序,rustfs teardown 干净(容器+数据目录无残留),yq `(跳过:未编写 teardown)`。

## 边界 / 后续

- **registry 分发未实现**:`crater push <project-ref>` 报错引导 save/.oci(push 是单 ref,
  task 制品不会跟着走);`apply <project-ref>`(store/registry 直连)同样引导。后续做
  多 ref push/pull 闭包。
- 同名 task 的两个不同版本进同一 project:bundle 内 recipe 目录按 task 名落盘会互覆,
  暂不支持(build 时同 ref 不同输入已报错兜住大部分)。
- 跨 play hostvars 传递、project 级 `crater inspect` 仍是 D-083 既有后续。
