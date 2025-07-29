# Stage 1: install toolchain and LLVM-based cross-compiler for AArch64
FROM ubuntu:jammy AS toolchain
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
 && apt-get install -y --no-install-recommends \
    git \
    ca-certificates \
    clang \
    lld \
    cmake \
    build-essential \
    llvm \
    libc6-dev-arm64-cross \
    linux-libc-dev-arm64-cross \
    pkg-config \
    binutils-aarch64-linux-gnu \
    gcc-aarch64-linux-gnu \
    libstdc++-11-dev \
    libstdc++-11-dev-arm64-cross \
    wget \
    curl \
    gnupg

RUN curl -fsSL https://deb.nodesource.com/setup_23.x -o nodesource_setup.sh
RUN bash nodesource_setup.sh
RUN apt-get update \
     && apt-get install -y nodejs \
     && rm -rf /var/lib/apt/lists/*
# Install rustup and add the ARM64 target
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y

ENV PATH="/root/.cargo/bin:${PATH}"
RUN rustup target add aarch64-unknown-linux-gnu
RUN rustup target add x86_64-unknown-linux-gnu
RUN rustup component add rust-src

# Dummy sources for dependency caching
RUN echo "fn main() {}"  > /dummy_main.rs
RUN echo ""              > /dummy_lib.rs

# Stage 2: copy Cargo manifests and dummy sources, fetch deps
FROM toolchain AS sources
ENV PATH="/root/.cargo/bin:${PATH}"

# Copy workspace Cargo.toml and lock
COPY Cargo.toml Cargo.toml
COPY Cargo.lock Cargo.lock

# Inject dummy files so that `cargo fetch` caches everything
COPY --from=toolchain dummy_main.rs src/main.rs
COPY --from=toolchain dummy_main.rs dg_edge_updater/src/main.rs

# Fetch all dependencies for the ARM64 target
RUN cargo fetch --target aarch64-unknown-linux-gnu
RUN cargo fetch --target x86_64-unknown-linux-gnu

# Stage 3: actual build with real sources
FROM sources AS all_build
ENV PATH="/root/.cargo/bin:${PATH}"
# Copy in the full workspace
COPY druid-garden-os-ui/    druid-garden-os-ui/
COPY dg_edge_updater/    dg_edge_updater/
COPY src/ src/
COPY migrations/ migrations/
COPY .sqlx/ .sqlx/

# Build the Frontend UI
RUN cd druid-garden-os-ui && npm install && npm run build && cd ../

# Build for AMD64 and collect binaries
RUN mkdir -p /build/amd64
RUN cargo build --release --target x86_64-unknown-linux-gnu
RUN cd dg_edge_updater && cargo build --release --target x86_64-unknown-linux-gnu && cd ../
RUN mv target/x86_64-unknown-linux-gnu/release/druid-garden-os /build/amd64/druid-garden-os.app
RUN mv dg_edge_updater/target/x86_64-unknown-linux-gnu/release/dg_edge_updater /build/amd64/druid-garden-edge-updater.app

# Build for ARM64 and collect binaries
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig \
    PKG_CONFIG_ALLOW_CROSS=1

RUN mkdir -p /build/aarch64
RUN cargo build --release --target aarch64-unknown-linux-gnu
RUN cd dg_edge_updater && cargo build --release --target aarch64-unknown-linux-gnu && cd ../
RUN mv target/aarch64-unknown-linux-gnu/release/druid-garden-os /build/aarch64/druid-garden-os.app
RUN mv dg_edge_updater/target/aarch64-unknown-linux-gnu/release/dg_edge_updater /build/aarch64/druid-garden-edge-updater.app


FROM sources AS build
ENV PATH="/root/.cargo/bin:${PATH}"
# Copy in the full workspace
COPY druid-garden-os-ui/    druid-garden-os-ui/
COPY dg_edge_updater/    dg_edge_updater/
COPY src/ src/
COPY migrations/ migrations/
COPY .sqlx/ .sqlx/

# Build the Frontend UI
RUN cd druid-garden-os-ui && npm install && npm run build && cd ../

# Build for ARM64 and collect binaries
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++ \
    PKG_CONFIG_PATH=/usr/lib/aarch64-linux-gnu/pkgconfig \
    PKG_CONFIG_ALLOW_CROSS=1

RUN mkdir -p /build/aarch64
RUN cargo build --release --target aarch64-unknown-linux-gnu
RUN mv target/aarch64-unknown-linux-gnu/release/druid-garden-os /build/aarch64/druid-garden-os.app

FROM scratch AS bins
COPY --from=all_build /build/amd64/druid-garden-os.app amd64/druid-garden-os.app
COPY --from=all_build /build/amd64/druid-garden-edge-updater.app amd64/druid-garden-edge-updater.app
COPY --from=all_build /build/aarch64/druid-garden-os.app aarch64/druid-garden-os.app
COPY --from=all_build /build/aarch64/druid-garden-edge-updater.app aarch64/druid-garden-edge-updater.app

FROM scratch AS aarch_bin
COPY --from=build /build/aarch64/druid-garden-os.app aarch64/druid-garden-os.app