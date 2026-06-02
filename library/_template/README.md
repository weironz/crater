# <交付名> —— 标准 crater 交付包(对齐 Ansible)

一句话说明。复制本目录起新交付,删掉用不到的部分。

## 部署
```bash
crater inspect <交付名>                         # 看参数/角色/materials
crater inspect <交付名> --gen-inventory > inv.yaml
crater apply  <交付名> -i inv.yaml              # 在线
crater build  -f library/<交付名>/<交付名>.yaml  # 打离线 OCI
```
