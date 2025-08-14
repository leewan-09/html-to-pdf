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
    apt-get install -y \
    pkg-config \
    libssl-dev \
    && rm -rf /var/lib/apt/lists/*

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

# Stage 4: Runtime with minimal attack surface
FROM debian:bookworm-slim AS runtime

# Install base dependencies first
RUN apt-get update && \
    apt-get install -y \
    wget \
    gnupg \
    ca-certificates \
    curl \
    apt-transport-https \
    --no-install-recommends && \
    # Add Chrome repository with HTTPS URL
    wget -q -O - https://dl-ssl.google.com/linux/linux_signing_key.pub | \
    gpg --dearmor -o /usr/share/keyrings/googlechrome-linux-keyring.gpg && \
    echo "deb [arch=amd64 signed-by=/usr/share/keyrings/googlechrome-linux-keyring.gpg] https://dl.google.com/linux/chrome/deb/ stable main" > /etc/apt/sources.list.d/google.list && \
    apt-get update

# Install Chrome and dependencies separately for better error handling
RUN apt-get install -y \
    google-chrome-stable \
    fonts-liberation \
    fonts-noto-cjk \
    fonts-noto-color-emoji \
    libnss3 \
    libxss1 \
    libasound2 \
    libxtst6 \
    libatspi2.0-0 \
    libgtk-3-0 \
    libgbm1 \
    libx11-xcb1 \
    libxcb-dri3-0 \
    --no-install-recommends && \
    # Verify Chrome installation explicitly
    test -f /usr/bin/google-chrome-stable || (echo "Chrome installation failed" && exit 1) && \
    /usr/bin/google-chrome-stable --version && \
    # Clean up
    apt-get purge -y wget gnupg apt-transport-https && \
    apt-get autoremove -y && \
    rm -rf /var/lib/apt/lists/*

# Create Chrome wrapper script after verifying Chrome exists
RUN test -f /usr/bin/google-chrome-stable && \
    echo '#!/bin/sh' > /usr/local/bin/chrome-wrapper && \
    echo 'exec /usr/bin/google-chrome-stable --no-sandbox --disable-setuid-sandbox --disable-dev-shm-usage --disable-gpu "$@"' >> /usr/local/bin/chrome-wrapper && \
    chmod 755 /usr/local/bin/chrome-wrapper && \
    # Create symlink for chrome
    ln -sf /usr/local/bin/chrome-wrapper /usr/local/bin/chrome && \
    # Verify wrapper works
    /usr/local/bin/chrome-wrapper --version || (echo "Chrome wrapper verification failed" && exit 1)

# Set Chrome path to use the wrapper
ENV CHROME_PATH=/usr/local/bin/chrome-wrapper

# Create non-root user with specific UID/GID
RUN groupadd -r -g 1001 appuser && \
    useradd -r -u 1001 -g appuser \
    -d /home/appuser \
    -s /sbin/nologin \
    -c "Application user" appuser && \
    mkdir -p /home/appuser && \
    chown -R appuser:appuser /home/appuser

# Copy binary
COPY --from=builder --chown=appuser:appuser /app/target/release/html-to-pdf-rust /usr/local/bin/html-to-pdf-rust

# Create necessary directories for Chrome with proper permissions
RUN mkdir -p /home/appuser/.cache/chromium && \
    mkdir -p /home/appuser/.local/share && \
    mkdir -p /home/appuser/.config && \
    mkdir -p /tmp/.X11-unix && \
    chmod 1777 /tmp/.X11-unix && \
    chown -R appuser:appuser /home/appuser/.cache && \
    chown -R appuser:appuser /home/appuser/.local && \
    chown -R appuser:appuser /home/appuser/.config

# Security: Drop capabilities
USER appuser
WORKDIR /home/appuser

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:5000/health || exit 1

EXPOSE 5000

ENTRYPOINT ["/usr/local/bin/html-to-pdf-rust"]