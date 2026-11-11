//! Linux block device scanner using `/sys/class/block` and `/proc/mounts`.

use super::{BusType, DriveInfo, DriveScanner};
use crate::drive::guard::is_system_mount;
use crate::error::InscribeError;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Linux block device scanner that inspects sysfs, `/proc/mounts`, and `/proc/swaps`.
#[derive(Debug, Clone)]
pub struct LinuxDriveScanner {
    sysfs_block_dir: PathBuf,
    proc_mounts_file: PathBuf,
    proc_swaps_file: PathBuf,
}

impl Default for LinuxDriveScanner {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxDriveScanner {
    /// Creates a new `LinuxDriveScanner` targeting system standard `/sys/class/block`, `/proc/mounts`, and `/proc/swaps`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sysfs_block_dir: PathBuf::from("/sys/class/block"),
            proc_mounts_file: PathBuf::from("/proc/mounts"),
            proc_swaps_file: PathBuf::from("/proc/swaps"),
        }
    }

    /// Creates a `LinuxDriveScanner` with custom paths for deterministic testing and isolation.
    #[must_use]
    pub fn with_paths(
        sysfs_block_dir: impl Into<PathBuf>,
        proc_mounts_file: impl Into<PathBuf>,
    ) -> Self {
        let proc_mounts = proc_mounts_file.into();
        let default_swaps = proc_mounts
            .parent()
            .map(|p| p.join("swaps"))
            .unwrap_or_else(|| PathBuf::from("/proc/swaps"));
        Self {
            sysfs_block_dir: sysfs_block_dir.into(),
            proc_mounts_file: proc_mounts,
            proc_swaps_file: default_swaps,
        }
    }

    /// Configures custom `/proc/swaps` path for deterministic testing.
    #[must_use]
    pub fn with_swaps_path(mut self, proc_swaps_file: impl Into<PathBuf>) -> Self {
        self.proc_swaps_file = proc_swaps_file.into();
        self
    }

    /// Parses `/proc/mounts` and maps device node paths to their active mount points,
    /// canonicalizing symlinks to resolve paths like `/dev/disk/by-uuid/<UUID>` and `/dev/mapper/...`.
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
            let mut parts = line.split_whitespace();
            if let (Some(spec), Some(mount)) = (parts.next(), parts.next()) {
                if spec.starts_with('/') {
                    let decoded_mount = unescape_octal(mount);
                    let resolved_spec =
                        fs::canonicalize(spec).unwrap_or_else(|_| PathBuf::from(spec));
                    map.entry(resolved_spec)
                        .or_default()
                        .push(PathBuf::from(decoded_mount));
                }
            }
        }
        Ok(map)
    }

    /// Parses `/proc/swaps` and extracts active swap device paths.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::Io` if `/proc/swaps` exists but cannot be read.
    pub fn parse_swaps(swaps_file: &Path) -> Result<Vec<PathBuf>, InscribeError> {
        let mut swaps = Vec::new();
        if !swaps_file.exists() {
            return Ok(swaps);
        }

        let content = fs::read_to_string(swaps_file)?;
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("Filename") {
                continue;
            }
            if let Some(dev_spec) = trimmed.split_whitespace().next() {
                if dev_spec.starts_with('/') {
                    let path = PathBuf::from(dev_spec);
                    let resolved = fs::canonicalize(&path).unwrap_or(path);
                    if !swaps.contains(&resolved) {
                        swaps.push(resolved);
                    }
                }
            }
        }
        Ok(swaps)
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

/// Determines if `candidate` represents a valid partition path of `disk`.
///
/// Linux kernel naming conventions:
/// - If base device name ends in an ASCII digit (e.g. `nvme0n1`, `mmcblk0`, `loop0`),
///   partitions must start with 'p' followed by ASCII digits (`p1`, `p2`).
/// - If base device name ends in an ASCII non-digit (e.g. `sda`, `sdb`, `vdc`),
///   partitions must be ASCII digits directly (`1`, `2`).
#[must_use]
pub fn is_partition_of(disk: &Path, candidate: &Path) -> bool {
    // If parents exist, they should match (e.g. both in /dev)
    if disk.parent() != candidate.parent() {
        return false;
    }

    let Some(disk_name) = disk.file_name().and_then(|f| f.to_str()) else {
        return false;
    };
    let Some(cand_name) = candidate.file_name().and_then(|f| f.to_str()) else {
        return false;
    };

    let Some(suffix) = cand_name.strip_prefix(disk_name) else {
        return false;
    };
    if suffix.is_empty() {
        return false;
    }

    let disk_ends_with_digit = disk_name.chars().last().is_some_and(|c| c.is_ascii_digit());
    if disk_ends_with_digit {
        suffix
            .strip_prefix('p')
            .is_some_and(|rest| !rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
    } else {
        suffix.chars().all(|c| c.is_ascii_digit())
    }
}

/// Decodes octal escape sequences in `/proc/mounts` (e.g. `\040` -> `' '`, `\303\251` -> `é`).
#[must_use]
pub fn unescape_octal(input: &str) -> String {
    if !input.contains('\\') {
        return input.to_string();
    }
    let bytes = input.as_bytes();
    let mut raw_bytes = Vec::with_capacity(bytes.len());
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && (b'0'..=b'7').contains(&bytes[i + 1])
            && (b'0'..=b'7').contains(&bytes[i + 2])
            && (b'0'..=b'7').contains(&bytes[i + 3])
        {
            let val =
                ((bytes[i + 1] - b'0') << 6) | ((bytes[i + 2] - b'0') << 3) | (bytes[i + 3] - b'0');
            raw_bytes.push(val);
            i += 4;
        } else {
            raw_bytes.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&raw_bytes).into_owned()
}

impl DriveScanner for LinuxDriveScanner {
    fn scan_drives(&self) -> Result<Vec<DriveInfo>, InscribeError> {
        if !self.sysfs_block_dir.exists() {
            return Ok(Vec::new());
        }

        let mut mounts_map = Self::parse_mounts(&self.proc_mounts_file)?;

        // Correlate Device Mapper slaves (/sys/class/block/dm-*/slaves/) back to mapped volumes
        if let Ok(entries) = fs::read_dir(&self.sysfs_block_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.starts_with("dm-") {
                    let dm_dev_path = PathBuf::from(format!("/dev/{name}"));
                    let mut dm_mounts = Vec::new();
                    if let Some(mounts) = mounts_map.get(&dm_dev_path) {
                        dm_mounts.extend(mounts.clone());
                    }
                    if let Ok(canonical_dm) = fs::canonicalize(&dm_dev_path) {
                        if let Some(mounts) = mounts_map.get(&canonical_dm) {
                            for m in mounts {
                                if !dm_mounts.contains(m) {
                                    dm_mounts.push(m.clone());
                                }
                            }
                        }
                    }

                    if !dm_mounts.is_empty() {
                        let slaves_dir = entry.path().join("slaves");
                        if let Ok(slave_entries) = fs::read_dir(slaves_dir) {
                            for slave_entry in slave_entries.flatten() {
                                let slave_name =
                                    slave_entry.file_name().to_string_lossy().into_owned();
                                let slave_path = PathBuf::from(format!("/dev/{slave_name}"));
                                let list = mounts_map.entry(slave_path).or_default();
                                for m in &dm_mounts {
                                    if !list.contains(m) {
                                        list.push(m.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let swap_devices = Self::parse_swaps(&self.proc_swaps_file).unwrap_or_default();
        let mut drives = Vec::new();

        let entries = fs::read_dir(&self.sysfs_block_dir)?;

        for entry in entries {
            let entry = entry?;
            let file_name = entry.file_name();
            let dev_name = file_name.to_string_lossy();

            // Skip virtual / loop / ram / dm devices
            if dev_name.starts_with("loop")
                || dev_name.starts_with("ram")
                || dev_name.starts_with("zram")
                || dev_name.starts_with("dm-")
            {
                continue;
            }

            let block_path = entry.path();

            // Skip partitions: /sys/class/block/<dev>/partition exists for partitions
            if block_path.join("partition").exists() {
                continue;
            }

            let read_trimmed = |sub: &str| {
                fs::read_to_string(block_path.join(sub))
                    .ok()
                    .map(|s| s.trim().to_string())
            };
            let read_flag = |sub: &str| read_trimmed(sub).is_some_and(|s| s == "1");

            // Read sector count (512-byte units in Linux sysfs)
            let sectors: u64 = read_trimmed("size")
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let capacity_bytes = sectors.saturating_mul(512);

            // Read logical sector size
            let sector_size: u32 = read_trimmed("queue/logical_block_size")
                .or_else(|| read_trimmed("queue/hw_sector_size"))
                .and_then(|s| s.parse().ok())
                .unwrap_or(512);

            let is_removable = read_flag("removable");
            let is_read_only = read_flag("ro");

            // Device details
            let device_dir = block_path.join("device");
            let vendor = read_trimmed("device/vendor").unwrap_or_default();
            let model = read_trimmed("device/model")
                .or_else(|| read_trimmed("device/name"))
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
                // Match exact disk (/dev/sdb) or partition (/dev/sdb1, /dev/nvme0n1p2)
                let is_match =
                    mounted_dev == &dev_node_path || is_partition_of(&dev_node_path, mounted_dev);

                if is_match {
                    for mount in mount_list {
                        if !drive_mount_points.contains(mount) {
                            drive_mount_points.push(mount.clone());
                        }
                    }
                }
            }

            // Cross-reference active swap partitions
            for swap_dev in &swap_devices {
                if swap_dev == &dev_node_path || is_partition_of(&dev_node_path, swap_dev) {
                    let swap_label = PathBuf::from("[SWAP]");
                    if !drive_mount_points.contains(&swap_label) {
                        drive_mount_points.push(swap_label);
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
        proc_swaps: PathBuf,
    }

    impl TempMockSysfs {
        fn new(test_name: &str) -> Self {
            let temp_dir = std::env::temp_dir()
                .join(format!("inscribe_test_{test_name}_{}", std::process::id()));
            let _ = fs::remove_dir_all(&temp_dir);
            let sysfs_block = temp_dir.join("sys/class/block");
            let proc_mounts = temp_dir.join("proc/mounts");
            let proc_swaps = temp_dir.join("proc/swaps");
            fs::create_dir_all(&sysfs_block).unwrap();
            fs::create_dir_all(proc_mounts.parent().unwrap()).unwrap();
            File::create(&proc_mounts).unwrap();
            File::create(&proc_swaps).unwrap();

            Self {
                temp_dir,
                sysfs_block,
                proc_mounts,
                proc_swaps,
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

        fn add_dm_device(&self, dm_name: &str, slave_devs: &[&str]) {
            let dm_dir = self.sysfs_block.join(dm_name);
            let slaves_dir = dm_dir.join("slaves");
            fs::create_dir_all(&slaves_dir).unwrap();
            for slave in slave_devs {
                let slave_path = self.sysfs_block.join(slave);
                #[cfg(unix)]
                let _ = std::os::unix::fs::symlink(&slave_path, slaves_dir.join(slave));
            }
        }

        fn set_mounts(&self, content: &str) {
            fs::write(&self.proc_mounts, content).unwrap();
        }

        fn set_swaps(&self, content: &str) {
            fs::write(&self.proc_swaps, content).unwrap();
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

    #[test]
    fn test_unescape_octal_multibyte_utf8() {
        assert_eq!(
            unescape_octal(r"/media/user/Caf\303\251\040Drive"),
            "/media/user/Café Drive"
        );
        assert_eq!(unescape_octal(r"/media/user/T\303\274r"), "/media/user/Tür");
        assert_eq!(unescape_octal(r"/simple/path"), "/simple/path");
    }

    #[test]
    fn test_parse_mounts_symlink_canonicalization() {
        let temp_dir =
            std::env::temp_dir().join(format!("inscribe_test_sym_mount_{}", std::process::id()));
        let _ = fs::remove_dir_all(&temp_dir);
        fs::create_dir_all(&temp_dir).unwrap();

        let real_dev = temp_dir.join("sdb1");
        fs::write(&real_dev, b"").unwrap();
        let sym_dir = temp_dir.join("disk/by-uuid");
        fs::create_dir_all(&sym_dir).unwrap();
        let sym_dev = sym_dir.join("1234-5678");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&real_dev, &sym_dev).unwrap();

        let mounts_file = temp_dir.join("mounts");
        fs::write(
            &mounts_file,
            format!("{} /mnt/data ext4 rw 0 0\n", sym_dev.display()),
        )
        .unwrap();

        let parsed = LinuxDriveScanner::parse_mounts(&mounts_file).unwrap();
        let canonical_real = fs::canonicalize(&real_dev).unwrap();
        assert!(
            parsed.contains_key(&canonical_real),
            "Canonicalized target must be key in parsed mounts map"
        );
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_device_mapper_slaves_mark_parent_disk_as_system() {
        let fixture = TempMockSysfs::new("dm_test");
        fixture.add_disk(
            "sdb",
            "pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/host2/target2:0:0/2:0:0:0",
            "SanDisk",
            "Extreme",
            31266816,
            true,
        );
        fixture.add_partition("sdb2", 2);
        // dm-0 is a mapped device (e.g. LUKS/LVM root) whose slave is sdb2
        fixture.add_dm_device("dm-0", &["sdb2"]);
        fixture.set_mounts("/dev/dm-0 / ext4 rw,relatime 0 0\n");

        let scanner = LinuxDriveScanner::with_paths(&fixture.sysfs_block, &fixture.proc_mounts);
        let drives = scanner.scan_drives().expect("Scanning must succeed");
        assert_eq!(drives.len(), 1);
        let drive = &drives[0];
        assert_eq!(drive.path, PathBuf::from("/dev/sdb"));
        assert!(
            drive.is_system,
            "Disk backing DM device mounted at / must be marked as system"
        );
        assert!(drive.mount_points.contains(&PathBuf::from("/")));
    }

    #[test]
    fn test_proc_swaps_marks_backing_disk_as_system() {
        let fixture = TempMockSysfs::new("swap_test");
        fixture.add_disk(
            "sdb",
            "pci0000:00/0000:00:14.0/usb1/1-2/1-2:1.0/host2/target2:0:0/2:0:0:0",
            "Kingston",
            "DataTraveler",
            31266816,
            true,
        );
        fixture.add_partition("sdb1", 1);
        fixture
            .set_swaps("Filename\tType\tSize\tUsed\tPriority\n/dev/sdb1 partition 4194300 0 -2\n");

        let scanner = LinuxDriveScanner::with_paths(&fixture.sysfs_block, &fixture.proc_mounts)
            .with_swaps_path(&fixture.proc_swaps);
        let drives = scanner.scan_drives().expect("Scanning must succeed");
        assert_eq!(drives.len(), 1);
        let drive = &drives[0];
        assert_eq!(drive.path, PathBuf::from("/dev/sdb"));
        assert!(
            drive.is_system,
            "Disk with active swap partition in /proc/swaps must be marked as system"
        );
    }
}
