# k8s —— Kubernetes 交付(完整示例)

本交付自闭环:多入口 + 共享 files/templates + 私有 role + 示范 inventory。

```
k8s/
├── k8s-ha.yaml         # 3-master HA 部署(VIP + keepalived/haproxy)  ← 主入口
├── k8s-offline.yaml    # 纯离线部署(OCI 物料)
├── k8s-online.yaml     # 在线部署
├── k8s-upgrade.yaml    # 滚动升级 project(cp play → worker play)
├── upgrade-run.yaml    # 升级 task(被 k8s-upgrade 的 play 引用)
├── files/ templates/   # 共享静态文件 / .j2
├── inventory.example.yaml
└── roles/kube-upgrade/ # 私有 role:逐台 drain→升级→uncordon(throttle:1 + run_once)
```

```bash
crater inspect k8s-ha --gen-inventory > inv.yaml   # 据 params 生成清单骨架
crater apply  k8s-ha -i inv.yaml                    # 部署
crater apply  k8s-upgrade -i inv.yaml               # 滚动升级(交付内 roles/kube-upgrade)
crater build  -f library/k8s/k8s-offline.yaml -t myreg/k8s:1.37  # 离线 OCI
```

滚动升级体现"无需改引擎也能做 day-2":`throttle:1`(逐台)+ `run_once`(首个 cp 执行 `kubeadm upgrade apply`)+ project 两 play(先 control-plane 后 worker)。
