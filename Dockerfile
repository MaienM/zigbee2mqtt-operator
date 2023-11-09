ARG RUST_VERSION=1.72.0
ARG ARCH=x86_64
ARG TARGET=${ARCH}-unknown-linux-musl

# Builder.

FROM rust:${RUST_VERSION} as builder
WORKDIR /source

ARG TARGET
RUN rustup target add ${TARGET}

COPY Cargo.* .
RUN mkdir -p src \
 && touch src/lib.rs \
 && cargo fetch --target=${TARGET} --locked \
 && rm -r src

# Ring doesn't build with the default build environment, and the environment that does work for it doesn't work to build the full project. To work around this we set up an environment that'll work for ring, build a dummy version of the project so that ring is built, and then clean up the environment for the regular build.
ARG LLVM_VERSION=16
RUN . /etc/os-release \
 && echo "Types: deb" >> /etc/apt/sources.list.d/llvm.sources \
 && echo "URIs: http://apt.llvm.org/$VERSION_CODENAME/" >> /etc/apt/sources.list.d/llvm.sources \
 && echo "Suites: llvm-toolchain-$VERSION_CODENAME-${LLVM_VERSION}" >> /etc/apt/sources.list.d/llvm.sources \
 && echo "Components: main" >> /etc/apt/sources.list.d/llvm.sources \
 && wget -qO- https://apt.llvm.org/llvm-snapshot.gpg.key > /etc/apt/trusted.gpg.d/apt.llvm.org.asc \
 && apt-get update \
 && apt-get install -y build-essential llvm-${LLVM_VERSION} clang-${LLVM_VERSION} \
 && rm -rf /var/lib/apt/lists/*
ENV CC_x86_64_unknown_linux_musl=clang-${LLVM_VERSION}
ENV AR_x86_64_unknown_linux_musl=llvm-ar-${LLVM_VERSION}
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-Clink-self-contained=yes -Clinker=rust-lld"
RUN mkdir -p src \
 && touch src/lib.rs \
 && cargo build --target=${TARGET} --frozen --release \
 && rm -r src target
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS=

COPY src/ ./src
RUN cargo build --target=${TARGET} --frozen --release

# Runtime.

FROM scratch

ARG TARGET
COPY --from=builder /source/target/${TARGET}/release/zigbee2mqtt-operator /

CMD ["/zigbee2mqtt-operator"]
