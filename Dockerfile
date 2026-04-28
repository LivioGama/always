# Multi-stage build for optimized production image
FROM rust:1.70-slim as builder

# Install system dependencies for building
RUN apt-get update && apt-get install -y \
    pkg-config \
    libssl-dev \
    libsqlite3-dev \
    sox \
    libnotify-bin \
    xclip \
    xdotool \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy source code
COPY . .

# Build release binary
RUN cargo build --release

# Production stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    sox \
    libnotify-bin \
    xclip \
    xdotool \
    libssl3 \
    libsqlite3-0 \
    ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Create app user
RUN useradd -r -s /bin/false always

# Create data directory
RUN mkdir -p /app/data && chown always:always /app/data

# Copy binary from builder
COPY --from=builder /app/target/release/always /usr/local/bin/always

# Set permissions
RUN chmod +x /usr/local/bin/always

# Switch to app user
USER always

WORKDIR /app

# Expose any necessary ports (if needed for future features)
# EXPOSE 8080

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD always status || exit 1

# Default command
CMD ["always", "run"]