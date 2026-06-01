## inventory示例

```
root@node5:/data/codes/crater# cat inventory.yaml 
# crater inventory —— 测试机群 .11/.12/.13。
# 用法:crater apply <动作> -i inventory.yaml  /  crater task list -i inventory.yaml
# 含明文密码,已被 .gitignore(/inventory.yaml)忽略,不会提交。
#
# 角色(D-071)决定 k8s-ha 拓扑:
#   controlplane —— 跑 keepalived+haproxy(VIP 192.168.73.14:8443)
#   bootstrap    —— 第一个 master(kubeadm init --upload-certs)
#   master       —— 额外 master(control-plane join,serial 逐台)
#   worker       —— 工作节点(join)
# 当前:3-master HA(.11 bootstrap + .12/.13 master),etcd quorum=3。
inventory:
  hosts:
    - name: n11
      address: 192.168.73.11
      user: root
      port: 22
      password: "123456"
      roles: [controlplane, bootstrap]

    - name: n12
      address: 192.168.73.12
      user: root
      port: 22
      password: "123456"
      roles: [controlplane, master]

    - name: n13
      address: 192.168.73.13
      user: root
      port: 22
      password: "123456"
      roles: [controlplane, master]
root@node5:/data/codes/crater# 

```

