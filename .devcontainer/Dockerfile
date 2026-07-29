# Theseus-Quarry dev image — source-only (no binaries/, wallets, or .env).
# Latest stable Rust via rustup; matches rust-toolchain.toml channel.

FROM mcr.microsoft.com/devcontainers/base:bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    CARGO_HOME=/usr/local/cargo \
    RUSTUP_HOME=/usr/local/rustup \
    PATH=/usr/local/cargo/bin:$PATH

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        pkg-config \
        libssl-dev \
        cmake \
    && rm -rf /var/lib/apt/lists/*

# Install latest stable Rust (edition 2024 workspace)
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
        --default-toolchain stable \
        --profile minimal \
        --component rustfmt \
        --component clippy \
    && rustup default stable \
    && rustc --version \
    && cargo --version \
    && chown -R vscode:vscode $CARGO_HOME $RUSTUP_HOME

WORKDIR /workspace

# Default command for ad-hoc docker run
CMD ["bash"]
