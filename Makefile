# ================================================
# TEMM1E Makefile
# Cloud-native Rust AI agent runtime
# ================================================

CARGO      := cargo
DOCKER     := docker
BINARY     := temm1e
VERSION    := $(shell grep '^version' Cargo.toml | head -1 | sed 's/.*"\(.*\)"/\1/')
GIT_SHA    := $(shell git rev-parse --short HEAD 2>/dev/null || echo "unknown")
IMAGE_TAG  := $(VERSION)-$(GIT_SHA)
IMAGE_NAME := temm1e

# Cross-compilation targets
TARGETS := x86_64-unknown-linux-musl \
           aarch64-unknown-linux-musl \
           x86_64-apple-darwin \
           aarch64-apple-darwin

# ------------------------------------------------
# Development
# ------------------------------------------------

.PHONY: build
build: ## Build debug binary
	$(CARGO) build --workspace

.PHONY: build-release
build-release: ## Build optimised release binary
	$(CARGO) build --release --bin $(BINARY)

.PHONY: run
run: ## Run the binary in debug mode
	$(CARGO) run -- start

.PHONY: run-release
run-release: ## Run the release binary
	$(CARGO) run --release -- start

.PHONY: test
test: ## Run all workspace tests
	$(CARGO) test --workspace

.PHONY: test-verbose
test-verbose: ## Run tests with output
	$(CARGO) test --workspace -- --nocapture

.PHONY: check
check: fmt-check clippy ## Run fmt check + clippy (CI gate)

.PHONY: fmt
fmt: ## Format all code
	$(CARGO) fmt --all

.PHONY: fmt-check
fmt-check: ## Check formatting (no changes)
	$(CARGO) fmt --all -- --check

.PHONY: clippy
clippy: ## Run clippy lints
	$(CARGO) clippy --workspace --all-targets --all-features -- -D warnings

.PHONY: audit
audit: ## Run security audit
	$(CARGO) audit

.PHONY: outdated
outdated: ## Check for outdated dependencies
	$(CARGO) outdated --workspace --root-deps-only

.PHONY: doc
doc: ## Build documentation
	$(CARGO) doc --workspace --no-deps --open

.PHONY: bench
bench: ## Run benchmarks
	$(CARGO) bench --workspace

# ------------------------------------------------
# Docker
# ------------------------------------------------

.PHONY: docker-build
docker-build: ## Build Docker image
	$(DOCKER) build -t $(IMAGE_NAME):$(IMAGE_TAG) -t $(IMAGE_NAME):latest .

.PHONY: docker-run
docker-run: ## Run Docker container
	$(DOCKER) run --rm -it \
		-p 8080:8080 \
		-e TEMM1E_MODE=auto \
		-e RUST_LOG=info \
		$(IMAGE_NAME):latest

.PHONY: docker-compose-up
docker-compose-up: ## Start services with docker-compose
	$(DOCKER) compose up -d

.PHONY: docker-compose-down
docker-compose-down: ## Stop docker-compose services
	$(DOCKER) compose down

.PHONY: docker-push
docker-push: docker-build ## Push Docker image to GHCR
	$(DOCKER) tag $(IMAGE_NAME):$(IMAGE_TAG) ghcr.io/temm1e/$(IMAGE_NAME):$(IMAGE_TAG)
	$(DOCKER) push ghcr.io/temm1e/$(IMAGE_NAME):$(IMAGE_TAG)

# ------------------------------------------------
# Cross-compilation / Release
# ------------------------------------------------

.PHONY: release
release: ## Cross-compile release binaries for all targets
	@echo "Building release binaries for all targets..."
	@mkdir -p dist
	@for target in $(TARGETS); do \
		echo ""; \
		echo "=== Building $$target ==="; \
		if $(CARGO) build --release --target $$target --bin $(BINARY) 2>/dev/null; then \
			cp target/$$target/release/$(BINARY) dist/$(BINARY)-$$target; \
			echo "OK: dist/$(BINARY)-$$target"; \
		else \
			echo "SKIP: $$target (missing toolchain or linker)"; \
		fi; \
	done
	@echo ""
	@echo "=== Release binaries ==="
	@ls -lh dist/ 2>/dev/null || echo "No binaries built."

.PHONY: release-checksums
release-checksums: release ## Generate SHA256 checksums for release binaries
	@cd dist && shasum -a 256 $(BINARY)-* > checksums-sha256.txt
	@cat dist/checksums-sha256.txt

# ------------------------------------------------
# Infrastructure
# ------------------------------------------------

.PHONY: tf-init
tf-init: ## Initialise Terraform
	cd infrastructure/terraform && terraform init

.PHONY: tf-plan
tf-plan: ## Plan Terraform changes
	cd infrastructure/terraform && terraform plan

.PHONY: tf-apply
tf-apply: ## Apply Terraform changes
	cd infrastructure/terraform && terraform apply

.PHONY: fly-deploy
fly-deploy: ## Deploy to Fly.io
	fly deploy -c infrastructure/terraform/fly.toml

# ------------------------------------------------
# Utilities
# ------------------------------------------------

.PHONY: clean
clean: ## Remove build artifacts
	$(CARGO) clean
	rm -rf dist/

.PHONY: clean-docker
clean-docker: ## Remove Docker images
	$(DOCKER) rmi $(IMAGE_NAME):latest $(IMAGE_NAME):$(IMAGE_TAG) 2>/dev/null || true

.PHONY: install
install: build-release ## Install binary to ~/.cargo/bin
	cp target/release/$(BINARY) $(HOME)/.cargo/bin/$(BINARY)

.PHONY: loc
loc: ## Count lines of code
	@find crates src -name '*.rs' | xargs wc -l | tail -1

.PHONY: help
help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | sort | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "\033[36m%-22s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
