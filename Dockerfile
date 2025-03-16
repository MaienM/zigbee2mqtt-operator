ARG RUST_VERSION
ARG NODEJS_VERSION

#
# Build extension.
#

FROM --platform=${BUILDPLATFORM} node:${NODEJS_VERSION} as build-extension
WORKDIR /source

COPY extension/*.* /source
RUN npm install
RUN npm run build

#
# Build operator.
#

FROM --platform=${BUILDPLATFORM} rust:${RUST_VERSION} as build-operator
WORKDIR /source

# Setup rust toolchain for target environment.
ARG TARGETARCH
RUN case "${TARGETARCH}" in \
	amd64) ARCH="x86_64" ;; \
	arm64) ARCH="aarch64" ;; \
	*) \
		>&2 echo "Unsupported architecture ${TARGETARCH}." \
		exit 1 \
	;; \
 esac \
 && TARGET="${ARCH}-unknown-linux-musl" \
 && echo "${TARGET}" > .target \
 && rustup target add ${TARGET}

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

# Fetch dependencies.
COPY Cargo.* .
RUN mkdir -p src \
 && touch src/lib.rs \
 && cargo fetch --locked --target="$(cat .target)" \
 && rm -r src

# Copy extension.
COPY --from=build-extension /source/dist /source/extension/dist

# Build binary.
COPY src/ ./src
ENV RUSTFLAGS="-Clink-self-contained=yes -Clinker=rust-lld"
RUN cargo build --frozen --release --target="$(cat .target)"
RUN mv "target/$(cat .target)/release/zigbee2mqtt-operator" target/

#
# Runtime.
#

FROM scratch

COPY --from=build-operator /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=build-operator /source/target/zigbee2mqtt-operator /

ENTRYPOINT ["/zigbee2mqtt-operator"]
CMD ["run"]
