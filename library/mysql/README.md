# mysql —— MySQL Server(blueprint)

发行版包安装;服务名按 family 分叉(debian=mysql / rhel=mysqld),条件写在
条目的 `when:` 上。

```bash
crater apply library/mysql/mysql.blueprint.yaml -i inventory.yaml
```
