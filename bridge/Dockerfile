FROM rust:slim-bookworm

# mingw provides the linker for the x86_64-pc-windows-gnu target the dll is built for.
RUN apt-get update && apt-get install -y --no-install-recommends \
        gcc-mingw-w64-x86-64 \
    && rm -rf /var/lib/apt/lists/*

# Matches rust-toolchain.toml. Installed under the `stable` name that file asks for, so rustup
# finds it rather than re-downloading into every (ephemeral) container.
RUN rustup toolchain install stable --profile minimal \
        --component clippy,rustfmt --target x86_64-pc-windows-gnu \
    && rustup default stable

# Pre-create cache dirs world-writable so they're usable when mounted as
# named volumes under an arbitrary non-root uid (see docker-compose.yml).
RUN mkdir -p /usr/local/cargo/registry /usr/local/cargo/git \
    && chmod -R a+rwX /usr/local/cargo

WORKDIR /workspace

CMD ["cargo", "build", "--release"]
