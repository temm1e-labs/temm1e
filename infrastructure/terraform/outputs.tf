# ================================================
# TEMM1E Terraform Outputs
# ================================================

output "instance_id" {
  description = "EC2 instance ID"
  value       = aws_instance.temm1e.id
}

output "public_ip" {
  description = "Public IP address of the TEMM1E instance"
  value       = var.enable_eip ? aws_eip.temm1e[0].public_ip : aws_instance.temm1e.public_ip
}

output "gateway_url" {
  description = "TEMM1E gateway URL"
  value       = "http://${var.enable_eip ? aws_eip.temm1e[0].public_ip : aws_instance.temm1e.public_ip}:8080"
}

output "health_check_url" {
  description = "Health check endpoint"
  value       = "http://${var.enable_eip ? aws_eip.temm1e[0].public_ip : aws_instance.temm1e.public_ip}:8080/health"
}

output "security_group_id" {
  description = "Security group ID"
  value       = aws_security_group.temm1e.id
}

output "data_volume_id" {
  description = "Persistent EBS volume ID"
  value       = aws_ebs_volume.temm1e_data.id
}

output "ssh_command" {
  description = "SSH command (if SSH enabled)"
  value       = var.enable_ssh && var.ssh_key_name != "" ? "ssh -i ~/.ssh/${var.ssh_key_name}.pem ec2-user@${var.enable_eip ? aws_eip.temm1e[0].public_ip : aws_instance.temm1e.public_ip}" : "SSH not enabled"
}
