## 创建component

```
cd /data/codes/crater
mkdir components/yq
```

创建文件

```
root@node5:/data/codes/crater# cat components/yq/component.yaml 
# yq — minimal online-deploy demo (D-017 dogfood).
# A single dependency-free static binary from GitHub (mikefarah/yq). Not a
# daemon — just a CLI tool dropped on PATH. The simplest possible "deploy
# anything": adding it is PURE DATA, zero Rust changes.
#
# Online path: the target machine curls the binary itself (agentless, D-007),
# with CN mirror fallback handled by fetch_best on the build side / mirrors data.
name: yq
version_default: "4.53.2"
supported_os: [ubuntu, debian, rhel, centos, rocky]

# Material closure (D-034): the one external thing yq needs, declared up front.
# `crater build` reads THIS to know what to pack — it never scrapes install.
materials:
  - name: yq-bin
    kind: file
    url_tmpl: "https://github.com/mikefarah/yq/releases/download/v{{version}}/yq_linux_amd64"

install:
  # Reference the material by name; the engine decides online-fetch vs
  # offline-push. `mode` folds the chmod in (no separate run_cmd). Idempotent:
  # offline-push skips when the remote sha256 already matches; online curl
  # skips when the file is already present.
  - action: place
    material: yq-bin
    dest: /usr/local/bin/yq
    mode: "0755"

verify:
  - action: run_cmd
    cmd: "/usr/local/bin/yq --version"
```

## 本机部署

```
crater apply yq
```

## 指定主机

```
crater apply yq --host 192.168.73.11 --user root --password 123456 或者 --key ~/.ssh/id_rsa 
```

## 指定inventory

创建inventory

```
root@node5:/data/codes/crater# cat inventory.yaml 
# crater inventory —— 部署目标主机清单。
# 用法:crater apply <动作> -i <此文件>(大量机器、每台各自凭据)。
#
# 每台主机至少 name + address;认证用 password 或 key(二选一,key 优先)。
# user 默认 root,port 默认 22。roles 可选(组件/task 按 role 选主机)。
inventory:
  hosts:
    # ① 密码认证
    - name: web1
      address: 192.168.73.11
      user: root
      port: 22
      password: "123456"
      # roles: [web]

    # ② SSH 私钥认证(适合禁用密码登录的机群;~ 会自动展开为 $HOME)
    - name: web2
      address: 192.168.73.12
      user: root
      port: 22
      password: "123456"
      # roles: [web]
```

运行

```
crater apply yq -i inventory.yaml
```

## 镜像操作

构建镜像

```
crater build -f examples/yq/yq.yaml -t 192.168.73.5:5000/yq:4.53.2
```

部署

```
crater apply 192.168.73.5:5000/yq:4.53.2
crater apply yq.tar
```

