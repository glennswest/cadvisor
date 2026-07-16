# Unit: cadvisor-rs runtime-test VM — cadv-rt1.g8.lo
#
# A single Fedora VM running BOTH containerd (+ nerdctl) and CRI-O (+ crictl)
# so the cadvisor-runtime factories (M7) can be conformance-tested against a
# real google/cadvisor on the same host. Throwaway: `terragrunt destroy` after
# sign-off.
#
# Uses the shared, versioned proxmox-fedora-vm module (pinned ?ref). vm_id
# allocated live via ../free-vmid.sh (range 2000-2100) — NEVER reuse an id
# without checking (terraform-modules CLAUDE.md "Incident: 2026-07-08").

include "root" {
  path = find_in_parent_folders("root.hcl")
}

terraform {
  source = "git::ssh://git@github.com/glennswest/terraform-modules.git//modules/proxmox-fedora-vm?ref=v0.3.0"
}

locals {
  ssh_key = trimspace(file(pathexpand("~/.ssh/id_rsa.pub")))

  # Fixed MAC -> reserved IP (outside the g8 DHCP pool .100-.200).
  # vm_id 2004 allocated via free-vmid.sh on 2026-07-16.
  node = { vm_id = 2004, mac = "BC:24:11:08:00:61", ip = "192.168.8.61" }
}

inputs = {
  dns_zone_id        = "9bed60c8-1664-4183-88f9-a1a21b927edc" # g8.lo
  ci_ssh_public_keys = [local.ssh_key]
  tags               = ["terraform", "fedora", "cadvisor", "runtime-test"]

  vm_datastore      = "test-lvm-thin"
  snippet_datastore = "terraform-snippets"

  vms = {
    cadv-rt1 = {
      vm_id     = local.node.vm_id
      mac       = local.node.mac
      ip        = local.node.ip
      cores     = 4
      memory    = 4096
      disk_size = 30
      user_data = templatefile("${get_terragrunt_dir()}/templates/runtime-user-data.yaml.tftpl", {
        hostname = "cadv-rt1"
        fqdn     = "cadv-rt1.g8.lo"
        ci_user  = "fedora"
        ssh_keys = [local.ssh_key]
      })
    }
  }
}
