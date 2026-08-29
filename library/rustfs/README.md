# rustfs —— S3 兼容对象存储(blueprint)

按官方 Linux 路线以 musl 静态二进制 + systemd 部署 [RustFS](https://github.com/rustfs/rustfs)。

**单机与分布式是同一份蓝图,拓扑由 inventory 决定**:storage 组 1 台 = 单机;
≥4 台 = 纠删码分布式(卷列表由机群视角渲染,每台自动列出全体节点,顺序一致)。
旧 task 版靠人手写 RUSTFS_VOLUMES 区间表达式、写错一台集群起不来 —— 那正是
这次改造删掉的东西。

```bash
crater inspect library/rustfs/rustfs.blueprint.yaml   # 参数契约
crater apply library/rustfs/rustfs.blueprint.yaml -i inventory.yaml \
  --set access_key=... --set secret_key=...
```

注意:
- 上游只发 zip(物料声明 `unzip: rustfs`),**离线部署必须先 `crater build` 烤闭包**
  (解包发生在控制端,目标机零依赖)。
- 测试环境数据目录在 loop 盘/同一块物理盘时,`--set bypass_disk_check=true`。
- 2~3 台的 inventory 两头不靠(单机不需要,纠删码不够),rustfs 首启会自己拒绝。
- 平台栈(`../middleware/platform.stack.yaml`)引用本蓝图做存储层。
