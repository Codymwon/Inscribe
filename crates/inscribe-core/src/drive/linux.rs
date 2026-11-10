//! Linux block device scanner using `/sys/class/block` and `/proc/mounts`.

use super::{BusType, DriveInfo, DriveScanner};
use crate::drive::guard::is_system_mount;
use crate::error::InscribeError;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Linux block device scanner that inspects sysfs and `/proc/mounts`.
#[derive(Debug, Clone)]
pub struct LinuxDriveScanner {
    sysfs_block_dir: PathBuf,
    proc_mounts_file: PathBuf,
}

impl Default for LinuxDriveScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxDriveScanner {
    /// Creates a new `LinuxDriveScanner` targeting system standard `/sys/class/block` and `/proc/mounts`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sysfs_block_dir: PathBuf::from("/sys/class/block"),
            proc_mounts_file: PathBuf::from("/proc/mounts"),
        }
    }

    /// Creates a `LinuxDriveScanner` with custom paths for deterministic testing and isolation.
    #[must_use]
    pub fn with_paths(
        sysfs_block_dir: impl Into<PathBuf>,
        proc_mounts_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            sysfs_block_dir: sysfs_block_dir.into(),
            proc_mounts_file: proc_mounts_file.into(),
        }
    }

    /// Parses `/proc/mounts` and maps device node paths to their active mount points.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::Io` if `/proc/mounts` cannot be read.
    pub fn parse_mounts(
        mounts_file: &Path,
    ) -> Result<HashMap<PathBuf, Vec<PathBuf>>, InscribeError> {
        let mut map: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
        if !mounts_file.exists() {
            return Ok(map);
        }

        let content = fs::read_to_string(mounts_file)?;
        for line in content.lines() {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let spec = parts[0];
                let mount = parts[1];
                if spec.starts_with('/') {
                    let decoded_mount = unescape_octal(mount);
                    map.entry(PathBuf::from(spec))
                        .or_default()
                        .push(PathBuf::from(decoded_mount));
                }
            }
        }
        Ok(map)
    }

    /// Determines the hardware bus interconnect type from a device's sysfs path.
    #[must_use]
    pub fn detect_bus_type(device_canonical_path: &Path) -> BusType {
        let path_str = device_canonical_path.to_string_lossy().to_ascii_lowercase();

        if path_str.contains("/usb") || path_str.contains(":usb") {
            BusType::Usb
        } else if path_str.contains("/nvme") {
            BusType::Nvme
        } else if path_str.contains("/ata")
            || path_str.contains("/sata")
            || path_str.contains("/ahci")
        {
            BusType::Sata
        } else if path_str.contains("/mmc") || path_str.contains("/sdhci") {
            BusType::SdMmc
        } else if path_str.contains("/virtio") {
            BusType::VirtIo
        } else if path_str.contains("/sas") {
            BusType::Sas
        } else {
            BusType::Unknown
        }
    }
}

/// Decodes octal escape sequences in `/proc/mounts` (e.g. `\040` -> `' '`).
fn unescape_octal(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            let mut octal = String::new();
            for _ in 0..3 {
                if let Some(&digit) = chars.peek() {
                    if ('0'..='7').contains(&digit) {
                        octal.push(digit);
                        chars.next();
                    } else {
                        break;
                    }
                }
            }
            if octal.len() == 3 {
                if let Ok(byte) = u8::from_str_radix(&octal, 8) {
                    result.push(byte as char);
                    continue;
                }
            }
            result.push('\\');
            result.push_str(&octal);
        } else {
            result.push(c);
        }
    }
    result
}

impl DriveScanner for LinuxDriveScanner {
    fn scan_drives(&self) -> Result<Vec<DriveInfo>, InscribeError> {
        if !self.sysfs_block_dir.exists() {
            return Ok(Vec::new());
        }

        let mounts_map = Self::parse_mounts(&self.proc_mounts_file)?;
        let mut drives = Vec::new();

        let entries = fs::read_dir(&self.sysfs_block_dir)?;

        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let dev_name = file_name.to_string_lossy();

            // Skip virtual / loop / ram devices
            if dev_name.starts_with("loop")
                || dev_name.starts_with("ram")
                || dev_name.starts_with("zram")
            {
                continue;
            }

            let block_path = entry.path();

            // Skip partitions: /sys/class/block/<dev>/partition exists for partitions
            if block_path.join("partition").exists() {
                continue;
            }

            // Read sector count (512-byte units in Linux sysfs)
            let size_str = fs::read_to_string(block_path.join("size")).unwrap_or_default();
            let sectors: u64 = size_str.trim().parse().unwrap_or(0);
            let capacity_bytes = sectors.saturating_mul(512);

            // Read logical sector size
            let sector_size: u32 = fs::read_to_string(block_path.join("queue/logical_block_size"))
                .or_else(|_| fs::read_to_string(block_path.join("queue/hw_sector_size")))
                .ok()
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(512);

            // Read removable flag
            let is_removable = fs::read_to_string(block_path.join("removable"))
                .ok()
                .map(|s| s.trim() == "1")
                .unwrap_or(false);

            // Read read-only flag
            let is_read_only = fs::read_to_string(block_path.join("ro"))
                .ok()
                .map(|s| s.trim() == "1")
                .unwrap_or(false);

            // Device details
            let device_dir = block_path.join("device");
            let vendor = fs::read_to_string(device_dir.join("vendor"))
                .ok()
                .map(|s| s.trim().to_string())
                .unwrap_or_default();

            let model = fs::read_to_string(device_dir.join("model"))
                .or_else(|_| fs::read_to_string(device_dir.join("name")))
                .ok()
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| dev_name.to_string());

            // Determine bus type using canonicalized path of device or block node
            let canonical_target = fs::canonicalize(&device_dir).unwrap_or_else(|_| {
                fs::canonicalize(&block_path).unwrap_or_else(|_| block_path.clone())
            });
            let bus_type = Self::detect_bus_type(&canonical_target);

            // Associate mount points: disk itself or any of its partitions
            let dev_node_path = PathBuf::from(format!("/dev/{dev_name}"));
            let mut drive_mount_points = Vec::new();

            for (mounted_dev, mount_list) in &mounts_map {
                let mounted_str = mounted_dev.to_string_lossy();
                let dev_node_str = dev_node_path.to_string_lossy();

                // Match exact disk (/dev/sdb) or partition (/dev/sdb1, /dev/nvme0n1p2)
                let is_match = mounted_str == dev_node_str
                    || (mounted_str.starts_with(&*dev_node_str)
                        && mounted_str[dev_node_str.len()..]
                            .chars()
                            .next()
                            .is_some_and(|c| c.is_ascii_digit() || c == 'p'));

                if is_match {
                    for mount in mount_list {
                        if !drive_mount_points.contains(mount) {
                            drive_mount_points.push(mount.clone());
                        }
                    }
                }
            }

            // Flag as system disk if any active mount point is a critical OS mount
            let is_system = drive_mount_points.iter().any(|m| is_system_mount(m));

            drives.push(DriveInfo {
                path: dev_node_path,
                model,
                vendor,
                size: capacity_bytes,
                sector_size,
                bus_type,
                is_removable,
                is_read_only,
                is_system,
                mount_points: drive_mount_points,
            });
        }

        drives.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(drives)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;

    struct TempMockSysfs {
        temp_dir: PathBuf,
        sysfs_block: PathBuf,
        proc_mounts: PathBuf,
    }

    impl TempMockSysfs {
        fn new(test_name: &str) -> Self {
            let temp_dir = std::env::temp_dir()
                .join(format!("inscribe_test_{test_name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&temp_dir);
            let sysfs_block = temp_dir.join("sys/class/block");
            let proc_mounts = temp_dir.join("proc/mounts");
            fs::create_dir_all(&sysfs_block).unwrap();
            fs::create_dir_all(proc_mounts.parent().unwrap()).unwrap();
            File::create(&proc_mounts).unwrap();

            Self {
                temp_dir,
                sysfs_block,
                proc_mounts,
            }
        }

        fn add_disk(
            &self,
            name: &str,
            bus_rel_path: &str,
            vendor: &str,
            model: &str,
            sectors: u64,
            removable: bool,
        ) -> PathBuf {
            let disk_dir = self.sysfs_block.join(name);
            fs::create_dir_all(&disk_dir).unwrap();

            // Create fake hierarchy to simulate bus detection
            let bus_target = self
                .temp_dir
                .join("sys/devices")
                .join(bus_rel_path)
                .join(name);
            fs::create_dir_all(&bus_target).unwrap();

            // Create device symlink pointing to the bus target
            #[cfg(unix)]
            std::os::unix::fs::symlink(&bus_target, disk_dir.join("device")).unwrap();

            let dev_dir = disk_dir.join("device");
            fs::write(disk_dir.join("size"), format!("{sectors}\n")).unwrap();
            fs::write(
                disk_dir.join("removable"),
                if removable { "1\n" } else { "0\n" },
            )
            .unwrap();
            fs::write(disk_dir.join("ro"), "0\n").unwrap();

            let queue_dir = disk_dir.join("queue");
            fs::create_dir_all(&queue_dir).unwrap();
            fs::write(queue_dir.join("logical_block_size"), "512\n").unwrap();

            fs::write(dev_dir.join("vendor"), format!("{vendor}\n")).unwrap();
            fs::write(dev_dir.join("model"), format!("{model}\n")).unwrap();

            disk_dir
        }

        fn add_partition(&self, part_name: &str, part_num: u32) {
            let part_dir = self.sysfs_block.join(part_name);
            fs::create_dir_all(&part_dir).unwrap();
            fs::write(part_dir.join("partition"), format!("{part_num}\n")).unwrap();
        }

        fn set_mounts(&self, content: &str) {
            fs::write(&self.proc_mounts, content).unwrap();
        }
    }

    impl Drop for TempMockSysfs {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.temp_dir);
        }
    }

    #[test]
    fn test_sysfs_parsing_mock_usb_drive() {
        let fixture = TempMockSysfs::new("usb_test");
        fixture.add_disk(
            "sdb",
            "pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/host2/target2:0:0/2:0:0:0",
            "SanDisk",
            "Ultra Flair",
            31266816, // ~16 GB in 512-byte sectors
            true,
        );
        fixture.add_partition("sdb1", 1);
        fixture.set_mounts("/dev/sdb1 /media/user/USB_STICK vfat rw 0 0\n");

        let scanner = LinuxDriveScanner::with_paths(&fixture.sysfs_block, &fixture.proc_mounts);
        let drives = scanner.scan_drives().expect("Scanning must succeed");

        assert_eq!(drives.len(), 1);
        let drive = &drives[0];
        assert_eq!(drive.path, PathBuf::from("/dev/sdb"));
        assert_eq!(drive.vendor, "SanDisk");
        assert_eq!(drive.model, "Ultra Flair");
        assert_eq!(drive.size, 31266816 * 512);
        assert_eq!(drive.sector_size, 512);
        assert_eq!(drive.bus_type, BusType::Usb);
        assert!(drive.is_removable);
        assert!(!drive.is_read_only);
        assert!(!drive.is_system);
        assert_eq!(
            drive.mount_points,
            vec![PathBuf::from("/media/user/USB_STICK")]
        );
    }

    #[test]
    fn test_root_mount_marks_backing_disk_as_system() {
        let fixture = TempMockSysfs::new("root_test");
        fixture.add_disk(
            "nvme0n1",
            "pci0000:00/0000:00:02.4/0000:05:00.0/nvme/nvme0",
            "",
            "Samsung SSD 970 EVO Plus 250GB",
            488397168,
            false,
        );
        fixture.add_partition("nvme0n1p1", 1);
        fixture.add_partition("nvme0n1p2", 2);
        fixture.set_mounts(
            "/dev/nvme0n1p2 / btrfs rw,noatime 0 0\n\
             /dev/nvme0n1p1 /boot/efi vfat rw 0 0\n",
        );

        let scanner = LinuxDriveScanner::with_paths(&fixture.sysfs_block, &fixture.proc_mounts);
        let drives = scanner.scan_drives().expect("Scanning must succeed");

        assert_eq!(drives.len(), 1);
        let drive = &drives[0];
        assert_eq!(drive.path, PathBuf::from("/dev/nvme0n1"));
        assert_eq!(drive.bus_type, BusType::Nvme);
        assert!(!drive.is_removable);
        assert!(drive.is_system);
        assert!(drive.mount_points.contains(&PathBuf::from("/")));
        assert!(drive.mount_points.contains(&PathBuf::from("/boot/efi")));
    }

    #[test]
    fn test_bus_type_detection() {
        assert_eq!(
            LinuxDriveScanner::detect_bus_type(Path::new("/sys/devices/pci0000:00/usb1/1-2")),
            BusType::Usb
        );
        assert_eq!(
            LinuxDriveScanner::detect_bus_type(Path::new(
                "/sys/devices/pci0000:00/0000:02:00.0/nvme/nvme0"
            )),
            BusType::Nvme
        );
        assert_eq!(
            LinuxDriveScanner::detect_bus_type(Path::new(
                "/sys/devices/pci0000:00/ata1/host0/target0:0:0"
            )),
            BusType::Sata
        );
        assert_eq!(
            LinuxDriveScanner::detect_bus_type(Path::new(
                "/sys/devices/platform/sdhci-pci.0/mmc_host/mmc0"
            )),
            BusType::SdMmc
        );
        assert_eq!(
            LinuxDriveScanner::detect_bus_type(Path::new(
                "/sys/devices/pci0000:00/0000:00:04.0/virtio1"
            )),
            BusType::VirtIo
        );
    }

    #[test]
    fn test_host_system_scan_on_linux() {
        let scanner = LinuxDriveScanner::new();
        let drives = scanner
            .scan_drives()
            .expect("Host scan must succeed on Linux");
        // A standard Linux workstation will have at least 1 whole disk
        assert!(
            !drives.is_empty(),
            "Expected at least one storage drive on host"
        );

        // There must be at least one drive marked as system disk (hosting /)
        let system_drives: Vec<&DriveInfo> = drives.iter().filter(|d| d.is_system).collect();
        assert!(
            !system_drives.is_empty(),
            "Host scan must mark root backing disk as system disk"
        );
    }
}
