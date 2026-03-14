# ================================================
# TEMM1E Terraform Variables
# ================================================

# ------------------------------------------------
# AWS Configuration
# ------------------------------------------------

variable "aws_region" {
  description = "AWS region to deploy into"
  type        = string
  default     = "us-east-1"
}

variable "environment" {
  description = "Deployment environment (dev, staging, prod)"
  type        = string
  default     = "dev"

  validation {
    condition     = contains(["dev", "staging", "prod"], var.environment)
    error_message = "Environment must be one of: dev, staging, prod."
  }
}

# ------------------------------------------------
# Compute
# ------------------------------------------------

variable "instance_type" {
  description = "EC2 instance type"
  type        = string
  default     = "t3.small"
}

variable "ssh_key_name" {
  description = "Name of the SSH key pair (leave empty to disable SSH)"
  type        = string
  default     = ""
}

variable "enable_ssh" {
  description = "Enable SSH ingress rule"
  type        = bool
  default     = false
}

variable "enable_eip" {
  description = "Allocate an Elastic IP for stable addressing"
  type        = bool
  default     = true
}

# ------------------------------------------------
# Storage
# ------------------------------------------------

variable "volume_size_gb" {
  description = "Size of the persistent data volume in GB"
  type        = number
  default     = 10
}

# ------------------------------------------------
# Networking
# ------------------------------------------------

variable "allowed_cidrs" {
  description = "CIDR blocks allowed to reach the gateway"
  type        = list(string)
  default     = ["0.0.0.0/0"]
}

# ------------------------------------------------
# Docker / Application
# ------------------------------------------------

variable "docker_image" {
  description = "Docker image name (without tag)"
  type        = string
  default     = "ghcr.io/temm1e/temm1e"
}

variable "docker_tag" {
  description = "Docker image tag to deploy"
  type        = string
  default     = "latest"
}

variable "temm1e_mode" {
  description = "TEMM1E operating mode (auto, cloud, local)"
  type        = string
  default     = "auto"

  validation {
    condition     = contains(["auto", "cloud", "local"], var.temm1e_mode)
    error_message = "Mode must be one of: auto, cloud, local."
  }
}

variable "log_level" {
  description = "Rust log level"
  type        = string
  default     = "info"
}

# ------------------------------------------------
# Secrets (pass via TF_VAR_ env vars or tfvars)
# ------------------------------------------------

variable "anthropic_api_key" {
  description = "Anthropic API key"
  type        = string
  sensitive   = true
  default     = ""
}

variable "telegram_bot_token" {
  description = "Telegram bot token"
  type        = string
  sensitive   = true
  default     = ""
}
