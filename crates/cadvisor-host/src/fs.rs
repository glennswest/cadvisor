//! Filesystem stats service: mountinfo partitions + statvfs + /proc/diskstats.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use cadvisor_model::v1;

use crate::HostError;
use crate::parse::{self, DiskStatsLine, MountInfoLine};

/// Filesystem types cadvisor collects capacity/usage for.
fn supported_fs(fs_type: &str) -> bool {
    matches!(fs_type, "btrfs" | "ext2" | "ext3" | "ext4" | "xfs" | "zfs" | "overlay" | "tmpfs")
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub device: String,
    pub mountpoint: String,
    pub fs_type: String,
    pub major: u64,
    pub minor: u64,
}

#[derive(Debug, Default)]
pub struct FsService {
    partitions: Vec<Partition>,
}

/// Directory usage (bytes and inodes) by walking, like upstream's GetDirUsage:
/// blocks x 512, hardlinks deduped, no crossing device boundaries.
pub fn dir_usage(path: &Path) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    let root_dev = match std::fs::symlink_metadata(path) {
        Ok(m) => m.dev(),
        Err(_) => return (0, 0),
    };
    let mut bytes = 0u64;
    let mut inodes = 0u64;
    let mut seen_hardlinks = BTreeSet::new();
    let mut stack = vec![path.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(meta) = std::fs::symlink_metadata(&dir) else { continue };
        if meta.dev() != root_dev {
            continue;
        }
        bytes += meta.blocks() * 512;
        inodes += 1;
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
            if m.is_dir() {
                stack.push(p);
            } else {
                if m.nlink() > 1 && !seen_hardlinks.insert(m.ino()) {
                    continue;
                }
                if m.dev() == root_dev {
                    bytes += m.blocks() * 512;
                    inodes += 1;
                }
            }
        }
    }
    (bytes, inodes)
}

/// statvfs-derived numbers for one filesystem.
#[derive(Debug, Default, Clone, Copy)]
pub struct FsUsage {
    pub capacity: u64,
    pub free: u64,
    pub available: u64,
    pub inodes: u64,
    pub inodes_free: u64,
}

impl FsService {
    pub fn new() -> Result<Self, HostError> {
        let content = std::fs::read_to_string("/proc/self/mountinfo")
            .map_err(|e| HostError::io("/proc/self/mountinfo", e))?;
        Ok(Self::from_mountinfo(&content))
    }

    pub fn from_mountinfo(content: &str) -> Self {
        let mut seen_sources = std::collections::BTreeSet::new();
        let mut partitions = Vec::new();
        for m in parse::parse_mountinfo(content) {
            if !supported_fs(&m.fs_type) {
                continue;
            }
            // Dedupe bind mounts by source device, except tmpfs (every tmpfs
            // mount reports "tmpfs" as source but is a distinct filesystem).
            let key = if m.fs_type == "tmpfs" { m.mountpoint.clone() } else { m.source.clone() };
            if !seen_sources.insert(key) {
                continue;
            }
            let MountInfoLine { major, minor, mountpoint, fs_type, source } = m;
            // tmpfs partitions are keyed/reported by mountpoint (there is no
            // meaningful device), matching cadvisor's output.
            let device = if fs_type == "tmpfs" { mountpoint.clone() } else { source };
            partitions.push(Partition { device, mountpoint, fs_type, major, minor });
        }
        FsService { partitions }
    }

    pub fn partitions(&self) -> &[Partition] {
        &self.partitions
    }

    pub fn usage(&self, mountpoint: &str) -> Result<FsUsage, HostError> {
        let st = rustix::fs::statvfs(Path::new(mountpoint))
            .map_err(|e| HostError::io(mountpoint, std::io::Error::from_raw_os_error(e.raw_os_error())))?;
        Ok(FsUsage {
            capacity: st.f_blocks * st.f_frsize,
            free: st.f_bfree * st.f_frsize,
            available: st.f_bavail * st.f_frsize,
            inodes: st.f_files,
            inodes_free: st.f_ffree,
        })
    }

    /// Machine-level filesystem list for MachineInfo.
    pub fn machine_filesystems(&self) -> Vec<v1::FilesystemInfo> {
        let mut out = Vec::new();
        for p in &self.partitions {
            let Ok(u) = self.usage(&p.mountpoint) else { continue };
            out.push(v1::FilesystemInfo {
                device: p.device.clone(),
                device_major: p.major,
                device_minor: p.minor,
                capacity: u.capacity,
                fs_type: "vfs".to_string(),
                inodes: u.inodes,
                has_inodes: true,
            });
        }
        out
    }

    /// The partition whose mountpoint is the longest prefix of `path`.
    pub fn partition_for_path(&self, path: &str) -> Option<&Partition> {
        self.partitions
            .iter()
            .filter(|p| {
                path == p.mountpoint
                    || path.starts_with(&format!("{}/", p.mountpoint.trim_end_matches('/')))
            })
            .max_by_key(|p| p.mountpoint.len())
    }

    /// Current per-disk IO counters keyed by (major, minor).
    pub fn disk_stats(&self) -> Result<BTreeMap<(u64, u64), DiskStatsLine>, HostError> {
        let content = std::fs::read_to_string("/proc/diskstats")
            .map_err(|e| HostError::io("/proc/diskstats", e))?;
        Ok(parse::parse_diskstats(&content)
            .into_iter()
            .map(|d| ((d.major, d.minor), d))
            .collect())
    }
}
