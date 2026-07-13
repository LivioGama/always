.PHONY: build run start stop clean help

# Default target
all: run

# Build and run the complete Always system
build run start:
	@./build-and-run.sh

# Stop the Always system
stop:
	@echo "🛑 Stopping Always system..."
	@pkill -f "AlwaysApp" 2>/dev/null && echo "✓ Stopped AlwaysApp" || echo "  No AlwaysApp running"
	@pkill -f "always run" 2>/dev/null && echo "✓ Stopped voice daemon" || echo "  No voice daemon running"

# Clean build artifacts
clean:
	@echo "🧹 Cleaning build artifacts..."
	@cargo clean
	@cd AlwaysApp && rm -rf .build
	@echo "✓ Clean complete"

# Show help
help:
	@echo "Always Voice Detection - Build System"
	@echo "====================================="
	@echo ""
	@echo "Available commands:"
	@echo "  make           - Build and run Always (default)"
	@echo "  make build     - Build and run Always"
	@echo "  make run       - Build and run Always"
	@echo "  make start     - Build and run Always"
	@echo "  make stop      - Stop Always system"
	@echo "  make clean     - Clean build artifacts"
	@echo "  make help      - Show this help"
	@echo ""
	@echo "Quick usage:"
	@echo "  make           # Build everything and start"
	@echo "  make stop      # Stop the system"