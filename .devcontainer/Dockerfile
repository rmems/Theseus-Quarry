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
        cmake \
        curl \
        git \
        libssl-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

# Install latest stable Rust (edition 2024 workspace)
SHELL ["/bin/bash", "-o", "pipefail", "-c"]
# Match rust-toolchain.toml components so non-vscode UIDs are not forced to
# rustup-install rust-src/rust-analyzer into a root-owned /usr/local/rustup.
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y \
        --default-toolchain stable \
        --profile minimal \
        --component rustfmt \
        --component clippy \
        --component rust-src \
        --component rust-analyzer \
    && rustup default stable \
    && rustc --version \
    && cargo --version \
    && rustup component list --installed \
    && chown -R vscode:vscode "${CARGO_HOME}" "${RUSTUP_HOME}"

WORKDIR /workspace

# Default command for ad-hoc docker run
CMD ["bash"]
