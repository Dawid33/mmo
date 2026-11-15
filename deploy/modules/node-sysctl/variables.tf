variable "sysctls" {
  description = "Node sysctls to set (name -> value). fs.inotify.max_user_instances: the kernel default is 128 per user, and every container that watches a file (config reloaders, sidecars) plus every followed log stream takes one; a full node answers `failed to create fsnotify watcher: too many open files`, which cut the kubelet's log follows (so Alloy's tails) on orion at 44 containers."
  type        = map(string)
  default = {
    "fs.inotify.max_user_instances" = "8192"
  }
}

variable "images" {
  description = "Images (repository:tag): `init` needs a shell and `sysctl` (busybox), `pause` only has to stay alive."
  type        = map(string)
  default = {
    init  = "busybox:1.37.0"
    pause = "registry.k8s.io/pause:3.10"
  }
}
