# syntax=docker/dockerfile:1
FROM rust:1.99.1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS git-build
RUN git init /git \
    && git -C /git remote add origin https://github.com/git/git.git \
    && git -C /git fetch --depth 1 origin 16bd9f20a403117f2e0d9bcda6c6e621d3763e77 \
    && git -C /git checkout --detach FETCH_HEAD \
    && make -C /git -j 2 NO_GETTEXT=YesPlease NO_TCLTK=YesPlease NO_OPENSSL=YesPlease prefix=/opt/git install

FROM rust:1.98.1-bookworm@sha256:93ce27a88655056a51dbdd8f5f2d7ddc071c7b0070fb288a37b5a285fc83971e AS enroute-build
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY scripts/prepare_enroute.py scripts/prepare_enroute.py
COPY vendor/enroute vendor/enroute
RUN python3 scripts/prepare_enroute.py
WORKDIR /build/target/enroute
RUN --mount=type=cache,id=enroute-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=enroute-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=enroute-target,target=/build/target/enroute/target \
    cargo build --locked --release --bin enroute && cp target/release/enroute /enroute

FROM rust:1.99.1-bookworm@sha256:48b7f435b8fc6378ce43b44f15e3a47fd8919b2825f835c39bd4bd3da50af5b8 AS cambium-build
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY vendor/enroute vendor/enroute
RUN --mount=type=cache,id=cambium-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cambium-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=cambium-target,target=/build/target \
    cargo build --locked --release -p cambium-server --bin cambium && cp target/release/cambium /cambium

FROM debian:bookworm-slim@sha256:7ff6ba31527b9e336162fa061746c366c4b2032f4d2cff2078bea43f78efe3f8 AS runtime
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates libcurl4 libssl3 zlib1g python3 \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 cambium && useradd --uid 10001 --gid 10001 --no-create-home cambium \
    && mkdir -p /var/lib/cambium /var/lib/enroute /run/cambium-hook \
    && chown 10001:10001 /var/lib/cambium /var/lib/enroute /run/cambium-hook
COPY --from=git-build /opt/git /opt/git
COPY --from=git-build /git/COPYING /usr/share/licenses/git/COPYING
ENV PATH="/opt/git/bin:${PATH}"
USER 10001:10001

FROM runtime AS enroute
COPY vendor/enroute/LICENSE vendor/enroute/NOTICE /usr/share/licenses/enroute/
COPY --from=enroute-build /enroute /usr/local/bin/enroute
COPY deploy/enroute.toml /etc/enroute.toml
COPY deploy/enroute-entrypoint.py /usr/local/bin/enroute-entrypoint.py
ENTRYPOINT ["python3", "/usr/local/bin/enroute-entrypoint.py"]

FROM runtime AS cambium
COPY LICENSE-APACHE LICENSE-AGPL /usr/share/licenses/cambium/
COPY vendor/enroute/LICENSE vendor/enroute/NOTICE /usr/share/licenses/enroute/
COPY --from=cambium-build /cambium /usr/local/bin/cambium
COPY deploy/cambium.toml /etc/cambium.toml
COPY deploy/initialize.py /usr/local/bin/initialize.py
ENTRYPOINT ["cambium", "--data-dir", "/var/lib/cambium/state"]
CMD ["serve"]
