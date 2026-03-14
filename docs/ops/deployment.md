# Operations Guide: Deployment

TEMM1E supports three deployment methods: Docker, Fly.io, and Terraform (AWS). All methods use the same static binary and configuration format.

## Docker Deployment

### Quick Start

```bash
docker pull ghcr.io/temm1e/temm1e:latest

docker run -d \
  --name temm1e \
  --restart unless-stopped \
  -p 8080:8080 \
  -v temm1e-data:/var/lib/temm1e \
  -e TEMM1E_MODE=cloud \
  -e ANTHROPIC_API_KEY=sk-ant-... \
  -e TELEGRAM_BOT_TOKEN=123456:ABC-... \
  -e RUST_LOG=info \
  ghcr.io/temm1e/temm1e:latest
```

### Building the Image

The Dockerfile uses a multi-stage build with `cargo-chef` for dependency caching:

1. **Chef planner** -- installs `cargo-chef` and musl targets
2. **Dependency planner** -- generates a `recipe.json` of all dependencies
3. **Builder** -- cooks dependencies (cached), then builds the binary
4. **Runtime** -- Alpine 3.19 with curl and ca-certificates; copies the static binary

```bash
# Build for the current platform
docker build -t temm1e:latest .

# Build for a specific platform
docker buildx build --platform linux/amd64 -t temm1e:latest .
docker buildx build --platform linux/arm64 -t temm1e:latest .
```

**Source**: `/Dockerfile`

### Image Details

| Property | Value |
|----------|-------|
| Base image | Alpine 3.19 |
| Binary | Static musl-linked, stripped |
| User | `temm1e` (non-root) |
| Data directory | `/var/lib/temm1e` |
| Config directory | `/etc/temm1e` |
| Exposed port | `8080` |
| Health check | `GET http://localhost:8080/health` (30s interval) |
| Entry point | `temm1e start` |

### Docker Compose

```yaml
version: "3.8"

services:
  temm1e:
    image: ghcr.io/temm1e/temm1e:latest
    container_name: temm1e
    restart: unless-stopped
    ports:
      - "8080:8080"
    volumes:
      - temm1e-data:/var/lib/temm1e
      - ./config.toml:/etc/temm1e/config.toml:ro
    environment:
      TEMM1E_MODE: cloud
      ANTHROPIC_API_KEY: ${ANTHROPIC_API_KEY}
      TELEGRAM_BOT_TOKEN: ${TELEGRAM_BOT_TOKEN}
      RUST_LOG: info
    healthcheck:
      test: ["CMD", "curl", "-f", "http://localhost:8080/health"]
      interval: 30s
      timeout: 3s
      retries: 3
      start_period: 5s

volumes:
  temm1e-data:
```

### Persistent Storage

The `/var/lib/temm1e` directory contains:

- `memory.db` -- SQLite database (conversations, long-term memory)
- `vault.enc` -- encrypted vault file
- `vault.key` -- vault encryption key (protect this)
- `files/` -- locally stored files

Mount a Docker volume or host directory to persist this data across container restarts.

### Custom Configuration

Mount a config file to `/etc/temm1e/config.toml`:

```bash
docker run -d \
  -v ./my-config.toml:/etc/temm1e/config.toml:ro \
  -v temm1e-data:/var/lib/temm1e \
  ghcr.io/temm1e/temm1e:latest
```

---

## Fly.io Deployment

Fly.io provides a managed container platform with global edge deployment, automatic TLS, and persistent volumes.

### Prerequisites

```bash
# Install the Fly CLI
curl -L https://fly.io/install.sh | sh

# Authenticate
fly auth login
```

### First Deployment

```bash
cd infrastructure/terraform   # fly.toml is here

# Launch the app (first time)
fly launch

# Set secrets
fly secrets set ANTHROPIC_API_KEY=sk-ant-...
fly secrets set TELEGRAM_BOT_TOKEN=123456:ABC-...

# Deploy
fly deploy
```

**Source**: `/infrastructure/terraform/fly.toml`

### Configuration

The `fly.toml` configures:

| Setting | Value |
|---------|-------|
| Primary region | `iad` (US East) |
| VM size | `shared-cpu-1x`, 512 MB RAM |
| Internal port | `8080` |
| HTTPS | Force-enabled |
| Auto-stop | Stops idle machines |
| Auto-start | Starts on incoming request |
| Health check | `GET /health` every 30s |
| Persistent volume | `temm1e_data` mounted at `/var/lib/temm1e` (1 GB initial) |

### Scaling

```bash
# Scale to more machines
fly scale count 2

# Upgrade VM size
fly scale vm shared-cpu-2x --memory 1024

# Add a region
fly regions add fra  # Frankfurt
```

### Monitoring on Fly.io

```bash
# View logs
fly logs

# Check app status
fly status

# Open monitoring dashboard
fly dashboard
```

---

## Terraform Deployment (AWS)

The Terraform configuration deploys TEMM1E on a single EC2 instance with persistent EBS storage.

### Prerequisites

```bash
# Install Terraform
brew install terraform   # macOS
# or download from https://terraform.io/downloads

# Configure AWS credentials
aws configure
```

### Deployment

```bash
cd infrastructure/terraform

# Initialize Terraform
terraform init

# Preview the plan
terraform plan \
  -var="anthropic_api_key=sk-ant-..." \
  -var="telegram_bot_token=123456:ABC-..."

# Apply
terraform apply \
  -var="anthropic_api_key=sk-ant-..." \
  -var="telegram_bot_token=123456:ABC-..."
```

**Source**: `/infrastructure/terraform/main.tf`, `variables.tf`, `outputs.tf`

### Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `aws_region` | `us-east-1` | AWS region |
| `environment` | `dev` | Environment name (`dev`, `staging`, `prod`) |
| `instance_type` | `t3.small` | EC2 instance type |
| `volume_size_gb` | `10` | Persistent data volume size in GB |
| `docker_image` | `ghcr.io/temm1e/temm1e` | Docker image |
| `docker_tag` | `latest` | Docker image tag |
| `temm1e_mode` | `auto` | TEMM1E operating mode |
| `log_level` | `info` | Rust log level |
| `enable_ssh` | `false` | Enable SSH access |
| `ssh_key_name` | `""` | SSH key pair name |
| `enable_eip` | `true` | Allocate Elastic IP |
| `allowed_cidrs` | `["0.0.0.0/0"]` | CIDR blocks allowed to reach the gateway |
| `anthropic_api_key` | `""` | Anthropic API key (sensitive) |
| `telegram_bot_token` | `""` | Telegram bot token (sensitive) |

Pass sensitive variables via environment:

```bash
export TF_VAR_anthropic_api_key="sk-ant-..."
export TF_VAR_telegram_bot_token="123456:ABC-..."
terraform apply
```

### Outputs

After deployment, Terraform outputs:

- `instance_id` -- EC2 instance ID
- `public_ip` -- public IP address
- `gateway_url` -- TEMM1E gateway URL (http://IP:8080)
- `health_check_url` -- health check endpoint
- `ssh_command` -- SSH command (if SSH enabled)

### Infrastructure Details

The Terraform config creates:

- **Security group** -- allows inbound on port 8080 (and optionally 22 for SSH)
- **EBS volume** -- encrypted gp3 volume for persistent data
- **EC2 instance** -- Amazon Linux 2023, Docker installed via user-data script
- **Elastic IP** -- stable public address (optional)

The EC2 user-data script:
1. Installs and starts Docker
2. Formats and mounts the EBS data volume to `/var/lib/temm1e`
3. Pulls the TEMM1E Docker image
4. Runs the container with secrets from Terraform variables

### Updating

```bash
# Update the Docker image tag
terraform apply -var="docker_tag=v0.2.0"

# Change instance size
terraform apply -var="instance_type=t3.medium"
```

### Destroying

```bash
terraform destroy
```

This removes all AWS resources. The EBS volume is destroyed -- back up `/var/lib/temm1e` first if needed.

---

## Deployment Checklist

- [ ] AI provider API key configured
- [ ] At least one channel enabled with a valid token
- [ ] Channel allowlists configured (empty = deny all)
- [ ] TLS enabled for cloud deployments
- [ ] Persistent storage mounted for vault and memory data
- [ ] Health check endpoint verified: `GET /health` returns 200
- [ ] Log level set appropriately (`info` for production)
- [ ] Secrets passed via environment variables or vault, never in config files
