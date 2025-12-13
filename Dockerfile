# Stage 1: Chef for dependency caching
FROM rust:1.89-slim-bookworm AS chef
RUN cargo install cargo-chef
WORKDIR /app

# Stage 2: Planner (analyze dependencies)
FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo chef prepare --recipe-path recipe.json

# Stage 3: Builder with cached dependencies
FROM chef AS builder

# Install build dependencies
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev && \
    rm -rf /var/lib/apt/lists/*

# Build dependencies (cached unless Cargo.toml changes)
COPY --from=planner /app/recipe.json recipe.json
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY Cargo.toml Cargo.lock ./
COPY src ./src

# Enable optimizations
ENV CARGO_PROFILE_RELEASE_LTO=true
ENV CARGO_PROFILE_RELEASE_CODEGEN_UNITS=1
ENV CARGO_PROFILE_RELEASE_OPT_LEVEL=3

RUN cargo build --release && \
    strip /app/target/release/html-to-pdf-rust

# Stage 4: Runtime
FROM debian:bookworm-slim AS runtime

# Pin Chrome version for chromiumoxide compatibility
# Chrome 131 is stable and compatible with chromiumoxide 0.7
ARG CHROME_VERSION=131.0.6778.204-1

# Install Chrome (pinned version) and dependencies
RUN apt-get update && \
    apt-get install -y \
        wget \
        gnupg \
        ca-certificates \
        fonts-liberation \
        fonts-noto-cjk \
        fonts-noto \
        fonts-noto-extra \
        libasound2 \
        libatk-bridge2.0-0 \
        libatk1.0-0 \
        libatspi2.0-0 \
        libcups2 \
        libdbus-1-3 \
        libdrm2 \
        libgbm1 \
        libgtk-3-0 \
        libnspr4 \
        libnss3 \
        libxcomposite1 \
        libxdamage1 \
        libxfixes3 \
        libxkbcommon0 \
        libxrandr2 \
        xdg-utils \
        --no-install-recommends && \
    # Download and install pinned Chrome version
    wget -q "https://dl.google.com/linux/chrome/deb/pool/main/g/google-chrome-stable/google-chrome-stable_${CHROME_VERSION}_amd64.deb" -O /tmp/chrome.deb && \
    dpkg -i /tmp/chrome.deb || apt-get install -fy --no-install-recommends && \
    rm /tmp/chrome.deb && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/* && \
    # Verify Chrome version
    google-chrome-stable --version

# Set Chrome environment variable for the Rust app
ENV CHROME_PATH=/usr/bin/google-chrome-stable

# Create non-root user with increased limits
RUN groupadd -r -g 1001 appuser && \
    useradd -r -u 1001 -g appuser -d /home/appuser -s /sbin/nologin appuser && \
    mkdir -p /home/appuser && \
    chown -R appuser:appuser /home/appuser && \
    # Ensure /tmp is writable for Chrome user data dirs
    chmod 1777 /tmp && \
    # Configure system limits
    echo "appuser soft nofile 65536" >> /etc/security/limits.conf && \
    echo "appuser hard nofile 65536" >> /etc/security/limits.conf && \
    echo "appuser soft nproc 4096" >> /etc/security/limits.conf && \
    echo "appuser hard nproc 4096" >> /etc/security/limits.conf

# Copy binary
COPY --from=builder --chown=appuser:appuser /app/target/release/html-to-pdf-rust /usr/local/bin/html-to-pdf-rust

# Switch to non-root user
USER appuser
WORKDIR /home/appuser

# Environment variables for resource limits
ENV CHROME_DUMPDIR=/tmp
ENV MALLOC_ARENA_MAX=2

EXPOSE 5000

ENTRYPOINT ["/usr/local/bin/html-to-pdf-rust"]