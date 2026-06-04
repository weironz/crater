# rustfs —— S3 兼容对象存储(容器化交付)

用 **docker run** 跑 [RustFS](https://github.com/rustfs/rustfs):本任务**不安装 docker**,
preflight 检查 docker 就绪,缺了报错并提示先 `crater apply docker`。

```bash
crater inspect rustfs
crater apply  rustfs --host 10.0.0.5 --password <pw>     # 在线:docker pull + run
crater delete rustfs --host 10.0.0.5 --password <pw>     # 删容器 + 数据目录(镜像保留)
just push                                                 # 构建 OCI 制品并推 registry
```

要点:

- 镜像是 `kind: image` 物料:在线 `docker pull`;`crater build` 把镜像打成 oci-archive,
  离线 `docker import` —— 同一份 yaml,在线/离线通吃(D-061)。
- S3 端点宿主机 `:9000`,数据落 `/data/rustfs`(teardown 会删,镜像不删)。
- 凭据默认上游的 `rustfsadmin/rustfsadmin`,生产用 inventory `vars:` 覆盖
  (`access_key`/`secret_key`,D-082)。
- 容器不可变:改参数(端口/凭据)先 `crater delete rustfs` 再 apply。
- 版本:`vars.version` 默认 `latest`;`just version=1.x.y push` 出固定版本制品。
