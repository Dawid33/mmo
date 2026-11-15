variable "read_timeout" {
  description = "Entrypoint readTimeout: the longest Traefik lets a client take to send one whole request, body included. Traefik v3 defaults to 60s, which cuts any upload slower than a minute (PicoShare). \"0\" = no limit; a duration (\"6h\") caps slow-body connections again."
  type        = string
  default     = "0"
}

variable "entrypoints" {
  description = "Entrypoints the timeout applies to (the k3s chart's names)."
  type        = list(string)
  default     = ["web", "websecure"]
}
