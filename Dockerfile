# Stage 1: Chef for dependency caching
FROM rust:1.89-slim AS chef
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

# Install Chrome and dependencies
RUN apt-get update && \
    apt-get install -y \
        wget \
        gnupg \
        ca-certificates \
        --no-install-recommends && \
    wget -q -O - https://dl-ssl.google.com/linux/linux_signing_key.pub | \
    gpg --dearmor -o /usr/share/keyrings/googlechrome-linux-keyring.gpg && \
    echo "deb [arch=amd64 signed-by=/usr/share/keyrings/googlechrome-linux-keyring.gpg] https://dl.google.com/linux/chrome/deb/ stable main" > /etc/apt/sources.list.d/google.list && \
    apt-get update && \
    apt-get install -y \
        google-chrome-stable \
        fonts-liberation \
        fonts-noto-cjk \
        --no-install-recommends && \
    apt-get clean && \
    rm -rf /var/lib/apt/lists/* && \
    # Verify Chrome is installed
    google-chrome-stable --version

# Set Chrome environment variable for the Rust app
ENV CHROME_PATH=/usr/bin/google-chrome-stable

# Create non-root user
RUN groupadd -r -g 1001 appuser && \
    useradd -r -u 1001 -g appuser -d /home/appuser -s /sbin/nologin appuser && \
    mkdir -p /home/appuser && \
    chown -R appuser:appuser /home/appuser && \
    # Ensure /tmp is writable for Chrome user data dirs
    chmod 1777 /tmp

# Copy binary
COPY --from=builder --chown=appuser:appuser /app/target/release/html-to-pdf-rust /usr/local/bin/html-to-pdf-rust

# Switch to non-root user
USER appuser
WORKDIR /home/appuser

EXPOSE 5000

ENTRYPOINT ["/usr/local/bin/html-to-pdf-rust"]