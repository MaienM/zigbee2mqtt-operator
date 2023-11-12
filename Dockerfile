ARG RUST_VERSION=1.72.0

#
# Builder.
#

FROM rust:${RUST_VERSION} as builder
WORKDIR /source

# Setup rust toolchain for target environment.
RUN TARGET="$(uname -m)-unknown-linux-musl" \
 && rustup target add ${TARGET} \
 && rustup set default-host ${TARGET}

# Setup LLVM environment. (At least) ring needs this to build.
ARG LLVM_VERSION=16
RUN . /etc/os-release \
 && echo "Types: deb" >> /etc/apt/sources.list.d/llvm.sources \
 && echo "URIs: http://apt.llvm.org/${VERSION_CODENAME}/" >> /etc/apt/sources.list.d/llvm.sources \
 && echo "Suites: llvm-toolchain-${VERSION_CODENAME}-${LLVM_VERSION}" >> /etc/apt/sources.list.d/llvm.sources \
 && echo "Components: main" >> /etc/apt/sources.list.d/llvm.sources \
 && wget -qO- https://apt.llvm.org/llvm-snapshot.gpg.key > /etc/apt/trusted.gpg.d/apt.llvm.org.asc \
 && apt-get update \
 && apt-get install -y build-essential llvm-${LLVM_VERSION} clang-${LLVM_VERSION} \
 && rm -rf /var/lib/apt/lists/*
ENV CC=clang-${LLVM_VERSION}
ENV AR=llvm-ar-${LLVM_VERSION}
ENV CARGO_RUSTFLAGS="-Clink-self-contained=yes -Clinker=rust-lld"

# Fetch dependencies.
COPY Cargo.* .
RUN mkdir -p src \
 && touch src/lib.rs \
 && cargo fetch --locked \
 && rm -r src

# Build binary.
COPY src/ ./src
RUN cargo build --frozen --release

#
# Runtime.
#

FROM scratch

COPY --from=builder /source/target/release/zigbee2mqtt-operator /

CMD ["/zigbee2mqtt-operator", "run"]
