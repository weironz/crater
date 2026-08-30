# crater 运行镜像。
#
# **它不在镜像里编译 Rust** —— 二进制由 CI 的构建矩阵在各自架构的原生
# runner 上产出,这里只负责放进去。理由:aws-lc-sys 要 C 工具链,在 QEMU
# 里跨架构编译 Rust 是几十分钟起步,而原生构建 + COPY 是几秒。
#
# 因此 build 前必须先备好:
#   dist/crater-amd64   dist/crater-arm64
# 然后 `docker buildx build --platform linux/amd64,linux/arm64 -f docker/Dockerfile .`
# 二进制是 **musl 静态链接**(static-pie),所以底可以是 alpine ——
# 也因此彻底没有 glibc 版本耦合:此前用 debian:bookworm(glibc 2.36)装
# 本机编的二进制,镜像能 build 成功、容器一起就是 `GLIBC_2.39 not found`。
# 静态之后那类问题从根上消失,镜像也从 159MB 降到 ~44MB。
ARG RUNTIME_BASE=alpine:3.21
FROM ${RUNTIME_BASE}

# ca-certificates 是**必需**的:物料常来自 https,没有根证书下载一律失败。
# alpine 自带 busybox sh —— crater 的本机执行器要 `sh -c`,不能用 scratch。
RUN apk add --no-cache ca-certificates

# buildx 自动注入,值为 amd64 / arm64 —— 一份 Dockerfile 出两个架构的镜像。
ARG TARGETARCH
COPY dist/crater-${TARGETARCH} /usr/local/bin/crater
RUN chmod +x /usr/local/bin/crater \
    # **构建期自检**:证明这个二进制在这个底上真的跑得起来。
    # 少了这一行,glibc 不匹配要等到容器启动才暴露 —— 那时镜像已经推上去了。
    && /usr/local/bin/crater --version

# 工作区 = 进程 CWD(蓝图 / inventory / app 文件都从这里找)。
WORKDIR /workspace
# 部署记录、作业日志、对账快照落在 $HOME/.crater —— 单独挂出来才能重启不丢。
ENV HOME=/data
VOLUME ["/workspace", "/data"]

EXPOSE 8080

# 默认起 UI;`docker run <image> plan -f x.yaml` 这类子命令同样直达。
#
# 容器里绑 0.0.0.0 是对的(端口由 -p 控制),但 UI **没有认证**:
# 除非用 --token 或前面挡一层反代,否则只应暴露在可信网段。
ENTRYPOINT ["/usr/local/bin/crater"]
CMD ["ui", "--bind", "0.0.0.0", "--port", "8080"]
