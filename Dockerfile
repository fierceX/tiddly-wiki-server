# ==============================================================================
# Stage 1: Chef - 用于缓存依赖，加速构建
# ==============================================================================
FROM rust:1.83-slim-bookworm AS chef
# 安装必要的系统构建依赖 (OpenSSL 是 AWS SDK 常需的)
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*
RUN cargo install cargo-chef --version '^0.1'
WORKDIR /app

# ==============================================================================
# Stage 2: Planner - 计算依赖配方
# ==============================================================================
FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

# ==============================================================================
# Stage 3: Builder - 实际编译
# ==============================================================================
FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
# 1. 先编译依赖（这一步会被 Docker 缓存，除非依赖变了）
RUN cargo chef cook --release --recipe-path recipe.json

# 2. 复制源代码
COPY . .

# 3. 编译二进制文件
# 注意：此时 empty.html 等文件会被 include_str! 宏打包进二进制文件
RUN cargo build --release

# 4. 准备运行时目录结构
# 创建空文件夹用于从 builder 阶段复制权限
RUN mkdir -p /app/data /app/files

# ==============================================================================
# Stage 4: Runtime - 最终的精简镜像
# ==============================================================================
# 使用 Google 的 Distroless 镜像，包含 glibc 和 libssl，非常适合 Rust
FROM gcr.io/distroless/cc-debian12

WORKDIR /app

# 1. 从 Builder 复制二进制文件
COPY --from=builder --chown=nonroot:nonroot /app/target/release/tiddly-wiki-server /app/tiddly-wiki-server

# 2. 准备数据目录（确保 nonroot 用户有写权限）
COPY --from=builder --chown=nonroot:nonroot /app/data /app/data
COPY --from=builder --chown=nonroot:nonroot /app/files /app/files

# 3. 复制配置文件 (假设你本地根目录有一个 config.toml 模板)
# 如果你希望完全通过挂载来配置，可以注释掉这一行，或者保留作为默认配置
COPY --chown=nonroot:nonroot config.toml /app/config.toml

# 4. 切换到非 root 用户提高安全性
USER nonroot

# 5. 暴露端口
EXPOSE 3032

# 6. 定义启动命令
# 指定配置文件路径
ENTRYPOINT ["/app/tiddly-wiki-server"]
CMD ["--config", "/app/config.toml"]
