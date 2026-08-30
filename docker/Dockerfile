# crater 运行镜像。
#
# **它不在镜像里编译 Rust** —— 二进制由 CI 的构建矩阵在各自架构的原生
# runner 上产出,这里只负责放进去。理由:aws-lc-sys 要 C 工具链,在 QEMU
# 里跨架构编译 Rust 是几十分钟起步,而原生构建 + COPY 是几秒。
#
# 因此 build 前必须先备好:
#   dist/crater-amd64   dist/crater-arm64
# 然后 `docker buildx build --platform linux/amd64,linux/arm64 -f docker/Dockerfile .`
# 基础镜像的 glibc 必须 **>= 构建机的 glibc** —— 否则镜像能 build 成功,
# 容器一起就是 `version GLIBC_2.39 not found`。
# ubuntu:24.04 是 2.39:既容得下 CI(ubuntu-22.04,glibc 2.35)编出的二进制,
# 也容得下本机 dev 直接编的。换更老的底(如 bookworm 的 2.36)会砍掉后者。
ARG RUNTIME_BASE=ubuntu:24.04
FROM ${RUNTIME_BASE}

# ca-certificates 是**必需**的:物料常来自 https,没有根证书下载一律失败。
# 其余保持最小 —— crater 是 agentless 的,目标机什么都不用装,
# 控制端也就不需要一整套工具链。
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

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
