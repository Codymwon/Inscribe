<div align="center">

# ⚡ Inscribe

**Blazing-fast, memory-safe native USB & SD card image flasher in Rust.**  
*Sub-10MB single binary, sub-20MB idle RAM, zero disk buffering, and ironclad system drive protection.*

[![CI Status](https://img.shields.io/badge/CI-passing-brightgreen.svg)](https://github.com/inscribe/inscribe/actions)
[![Rust Version](https://img.shields.io/badge/rust-2024%20(1.85%2B)-orange.svg)](https://www.rust-lang.org)
[![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE)
[![Binary Size](https://img.shields.io/badge/binary%20size-%3C%2010%20MB-purple.svg)](#-why-inscribe-comparison)
[![Memory RSS](https://img.shields.io/badge/idle%20RAM-%3C%2020%20MB-success.svg)](#-why-inscribe-comparison)
[![Platforms](https://img.shields.io/badge/platform-Linux%20%7C%20Windows%20%7C%20macOS-lightgrey.svg)](#-installation)

[Features](#-key-features) • [Comparison](#-why-inscribe-comparison) • [Architecture](#-architecture) • [Installation](#-installation) • [Quick Start](#-quick-start) • [CLI Reference](#-headless-cli-reference) • [Safety Engine](#-drive-guard-safety-engine) • [Development](#-development--testing) • [License](#-license)

</div>

---

## 💡 About The Project

Flashing operating system disk images (`.iso`, `.img`) to USB thumb drives and SD cards is an essential task, but existing tools leave much to be desired:

- **400MB+ Electron flashers** consume hundreds of megabytes of RAM, start sluggishly, and bundle complete web browsers just to write bytes to a disk.
- **Host SSD wear**: Many flashers decompress multi-gigabyte compressed images (`.zip`, `.xz`, `.zst`) into temporary directories on your host drive before writing, burning flash cycles on your main SSD.
- **Inaccurate progress indicators**: Standard file I/O fills the OS kernel page cache instantly, causing progress bars to jump to 99% in seconds while freezing for minutes as dirty cache pages flush to physical storage.
- **Catastrophic overwrite risks**: Traditional terminal tools (`dd`) offer zero protection if you mistype a drive letter, instantly destroying primary boot drives or backup storage.

**Inscribe** was built from the ground up in Rust (2024 Edition) to solve every single one of these problems:

1. **Ultra-Lightweight**: A single standalone binary under **10 MB** with an idle resident memory footprint under **20 MB** and startup times under **100 ms**.
2. **Zero Disk Buffering**: Direct transparent streaming decompression directly from archives to block devices—temporary files never touch your host disk.
3. **True Hardware Direct I/O**: Direct block writes (`O_DIRECT` on Linux, `FILE_FLAG_NO_BUFFERING` on Windows, `/dev/rdisk*` + `F_NOCACHE` on macOS) bypass the OS buffer cache so speeds and percentages reflect real physical media writes.
4. **Drive Guard Safety Engine**: Multi-tiered protection blacklists system root drives, internal NVMe/SATA/SAS buses, and read-only devices, preventing accidental data loss.
5. **Modern Native GUI & Headless CLI**: A responsive 3-step native GUI built with Slint 1.18 alongside a scriptable, automation-ready CLI.

---

## 📊 Why Inscribe? Comparison

| Feature | Inscribe | Balena Etcher | Rufus | Raspberry Pi Imager | `dd` |
|:---|:---:|:---:|:---:|:---:|:---:|
| **Language & Engine** | **Rust 2024 + Slint** | Electron + Node.js | C / Win32 | C++ / Qt | C |
| **Binary Footprint** | **< 10 MB** | ~400 MB+ | ~1.5 MB | ~30 MB | ~100 KB |
| **Idle Memory (RSS)** | **< 20 MB** | 300 MB – 1 GB | ~25 MB | ~60 MB | ~5 MB |
| **Cold Startup Time** | **< 100 ms** | 4 – 10 s | < 200 ms | ~1 s | < 10 ms |
| **Zero Disk Buffering** | **Yes (Direct Stream)** | No (buffers to disk) | Limited | Partial | No (requires pipe) |
| **Direct Hardware I/O** | **Yes (`O_DIRECT`)** | No (Page cache) | Partial | No | User flag required |
| **Drive Guard Protection** | **Ironclad (Automated)** | Basic warning | Windows only | Basic filter | **None (Destructive!)** |
| **Two-Pass Verification** | **BLAKE3 + SHA-256** | MD5 / SHA-1 | Checksum check | SHA-256 | None |
| **Wayland Native Elevation** | **Yes (Polkit/UDisks2)** | Often requires root | N/A | Prompt | Requires root (`sudo`) |
| **Cross-Platform (GUI & CLI)** | **Linux / Windows / macOS** | Linux / Win / macOS (GUI) | Windows only | Linux / Win / macOS | Unix CLI only |

---

## ✨ Key Features

### ⚡ Zero-Decompression Direct Streaming
Inscribe wraps decompression algorithms inside streaming pipelines (`std::io::Read`) that decompress directly into 4MB sector-aligned ring buffers in RAM. Whether flashing `.zip`, `.tar.gz`, `.img.xz`, `.zst`, or `.bz2` images, zero uncompressed bytes are written to host SSDs.

### 🛡️ Ironclad Drive Guard Safety
Never worry about overwriting your OS drive:
- **System Mount Blacklist**: Cross-references physical disks backing `/`, `/boot`, `/home`, `C:\`, and macOS `/System/Volumes/Data`.
- **Bus Type Filtering**: Rejects internal NVMe, SATA, SAS, and VirtIO buses. Only removable USB and SD/MMC devices are flashable by default.
- **External USB SSD Safe Mode**: Explicit opt-in filter (`--allow-usb-ssd`) allowing high-speed external USB SSD enclosures with explicit confirmation warnings.
- **TOCTOU Attestation**: Validates device size, serial, and bus type immediately before writing to prevent drive swapping attacks.

### 🚀 Direct Hardware I/O & Dual-Pass Verification
- **True Direct I/O**: Sector-aligned 4096-byte buffers bypass the operating system's page cache (`O_DIRECT` / `FILE_FLAG_NO_BUFFERING` / `F_NOCACHE`), ensuring accurate write speeds (MB/s) and estimated time remaining (ETA).
- **Pass 1 (Write & In-Flight Hash)**: Streams blocks to the physical device while calculating in-flight cryptographic hashes (BLAKE3 and optional source SHA-256).
- **Pass 2 (Verification)**: Evicts the OS read cache (`posix_fadvise(POSIX_FADV_DONTNEED)`) and reads back raw blocks from hardware storage to verify bit-for-bit integrity against the source hash.

### 🎨 Native Slint 1.18 GUI
- **Sub-10MB Native Application**: Compiled using Slint with native software / FemtoVG renderers and Swash text rasterization (strictly avoiding 30MB+ Skia bloat).
- **Intuitive 3-Step Workflow**: Select Image $\rightarrow$ Select Target Drive $\rightarrow$ Flash & Verify.
- **Drag & Drop**: Drop disk images directly into the application window with path validation.
- **Full Keyboard Accessibility**: Complete keyboard navigation and shortcuts (`Ctrl+O`, `Ctrl+D`, `Ctrl+F`, `Esc`).
- **Live Metrics**: Real-time throughput graph, transfer progress, and elapsed/remaining timers.

### 🔐 Secure Privilege Elevation
- **Linux**: Integrates with `udisks2` and `zbus` over D-Bus to unmount target partitions and acquire authenticated raw block file descriptors via Polkit. **Never requires running the GUI as root under Wayland.**
- **Windows**: Automatic UAC elevation (`requireAdministrator` manifest) and volume locking (`FSCTL_LOCK_VOLUME`, `FSCTL_DISMOUNT_VOLUME`).
- **macOS**: Native exclusive volume claims via DiskArbitration and `authopen`.

---

## 📁 Supported Image Formats

Inscribe automatically detects compression types from file magic headers as well as file extensions:

| Format | Extension | Streaming Decompressor | Notes |
|:---|:---|:---|:---|
| **Raw Disk Image** | `.img`, `.iso`, `.raw`, `.bin` | Direct Zero-Copy Pass-Through | Line-speed direct block streaming |
| **Gzip** | `.gz`, `.img.gz`, `.iso.gz` | `flate2` (Streaming Deflate) | Common Linux & SBC distributions |
| **XZ / LZMA** | `.xz`, `.img.xz`, `.iso.xz` | `lzma-rust2` (Streaming XZ) | High-compression distributions |
| **Zstandard** | `.zst`, `.img.zst`, `.iso.zst` | `zstd` (Streaming Zstandard) | Fast high-ratio modern compression |
| **Bzip2** | `.bz2`, `.img.bz2` | `bzip2` (Streaming Bzip2) | Legacy archive compression |
| **ZIP** | `.zip` | `zip` (Single-file archive reader) | Raspberry Pi & vendor archives |

---

## 🏗️ Architecture

Inscribe is structured as an idiomatic, modular Rust workspace:

```
inscribe/
├── crates/
│   ├── inscribe-core/    # Pure headless domain logic (Drive Guard, streaming, writer)
│   ├── inscribe-cli/     # Headless terminal CLI (clap, indicatif, automation)
│   └── inscribe-gui/     # Slint 1.18 native GUI application (Fluent theme, event bridge)
├── dist/                 # Packaging manifests (AppImage, Flatpak, Polkit, macOS, WiX)
├── Cargo.toml            # Workspace manifest with fat LTO and release profiles
├── CONSTRAINTS.md        # Enforced engineering constraints and quality bar
└── deny.toml             # Supply chain security, banned crates, and CVE mitigation
```

### Component Flow

```
┌────────────────────────┐      ┌────────────────────────┐
│   inscribe-gui (Slint) │      │   inscribe-cli (Clap)  │
└───────────┬────────────┘      └───────────┬────────────┘
            │                               │
            └───────────────┬───────────────┘
                            ▼
        ┌───────────────────────────────────────┐
        │            inscribe-core              │
        │  ┌─────────────────────────────────┐  │
        │  │       Drive Guard Engine        │  │
        │  │  (Mount Blacklist, Bus Filters) │  │
        │  └─────────────────────────────────┘  │
        │  ┌─────────────────────────────────┐  │
        │  │  Streaming Decompressor (Read)  │  │
        │  │   (XZ, GZ, ZST, BZ2, ZIP, RAW)  │  │
        │  └─────────────────────────────────┘  │
        │  ┌─────────────────────────────────┐  │
        │  │  Direct I/O BlockDeviceWriter   │  │
        │  │ (4MB Aligned Buffer, O_DIRECT)  │  │
        │  └─────────────────────────────────┘  │
        │  ┌─────────────────────────────────┐  │
        │  │    Dual Hasher (BLAKE3 / SHA)   │  │
        │  └─────────────────────────────────┘  │
        └───────────────────┬───────────────────┘
                            ▼
                [ Physical Block Device ]
```

---

## 🚀 Installation

### Prerequisites

- **Rust**: Version **1.85.0 (2024 Edition)** or later. Install via [rustup](https://rustup.rs):
  ```bash
  rustup update stable
  ```
- **Linux Build Dependencies**:
  ```bash
  # Debian / Ubuntu / Pop!_OS
  sudo apt install -y build-essential pkg-config libdbus-1-dev libfontconfig1-dev libx11-dev libxcb1-dev

  # Fedora / RHEL
  sudo dnf install -y gcc dbus-devel fontconfig-devel libX11-devel libxcb-devel

  # Arch Linux
  sudo pacman -S --needed base-devel dbus fontconfig libx11 libxcb
  ```

### Build from Source

1. **Clone the repository:**
   ```bash
   git clone https://github.com/inscribe/inscribe.git
   cd inscribe
   ```

2. **Build optimized release binaries:**
   ```bash
   cargo build --release
   ```
   The compiled binaries will be placed in `target/release/`:
   - `inscribe-gui`: Native graphical application.
   - `inscribe`: Scriptable terminal CLI.

---

## 🖥️ Quick Start

### 1. Launching the GUI

Launch the native graphical interface directly:

```bash
cargo run --release -p inscribe-gui
# Or run the built binary
./target/release/inscribe-gui
```

#### 3-Step Graphical Flow
1. **Select Image**: Click **Select Image** or press `Ctrl+O`. You can also drag and drop any supported image file directly into the window.
2. **Select Target Drive**: Click **Select Drive** or press `Ctrl+D`. Inscribe lists detected removable drives with their vendor, model, capacity, and bus type. Non-removable internal drives are automatically hidden and protected by Drive Guard.
3. **Flash**: Click **Flash** or press `Ctrl+F`. Inscribe unmounts existing partitions, writes the image with direct hardware I/O, verifies the data bit-for-bit with BLAKE3, and reports completion with a celebratory confirmation!

#### Keyboard Shortcuts
| Shortcut | Action |
|:---|:---|
| `Ctrl + O` | Open file chooser to select disk image |
| `Ctrl + D` | Open drive selection dialog |
| `Ctrl + F` | Start flashing selected image to target drive |
| `Escape` | Dismiss modal / prompt cancellation |

---

### 2. Headless CLI Reference

Inscribe provides a dedicated CLI binary (`inscribe`) for automated terminal workflows, headless servers, and CI environments.

```bash
cargo run --release -p inscribe-cli -- --help
```

#### List Available Drives
Scan and list connected storage devices alongside their Drive Guard safety status:

```bash
inscribe list
```

*Options:*
- `--all`: Show all drives, including protected internal and system drives.
- `--allow-usb-ssd`: Include external USB SSD enclosures as flashable targets.

```text
Target Drive Selection:
--------------------------------------------------------------------------------
[0] Flashable: SanDisk Ultra USB 3.0 (/dev/sdb) - 31.0 GB [Bus: Usb]
[1] BLOCKED:   Samsung SSD 980 1TB (/dev/nvme0n1) - 1.0 TB [Bus: Nvme] (System Mount / Internal Bus)
```

#### Inspect an Image
Check the compression format, uncompressed payload size, and integrity of an image before flashing:

```bash
inscribe inspect debian-12.8.0-amd64-netinst.iso.xz
```

```text
Inspecting disk image: debian-12.8.0-amd64-netinst.iso.xz
Format:            XZ (Streaming Decompression)
Compressed Size:   482.3 MB
Uncompressed Size: 654.0 MB (detected from header)
```

#### Flash a Drive
Flash an image directly to a block device with two-pass verification:

```bash
# Standard write with BLAKE3 verification
inscribe flash debian-live.iso /dev/sdb

# Verify source against an expected SHA-256 checksum during Pass 1
inscribe flash ubuntu-24.04.iso /dev/sdb --verify-sha256 2a1f...9b4c

# Flash to an external USB SSD enclosure
inscribe flash fedora.iso /dev/sdc --allow-usb-ssd

# Skip verification pass (fastest write)
inscribe flash archlinux.iso /dev/sdb --no-verify
```

---

## 🛡️ Drive Guard Safety Engine

Drive Guard is Inscribe's safety mechanism against catastrophic storage overwrites. It operates fail-closed across all platforms:

```
Target Drive Candidate
         │
         ▼
  [ Zero Capacity? ] ───────► REJECT: Invalid capacity
         │ No
         ▼
  [ Read-Only Media? ] ─────► REJECT: Write-protected
         │ No
         ▼
  [ System / Root Disk? ] ──► REJECT: Contains OS root, /boot, C:\, or system partitions
         │ No
         ▼
  [ Internal Bus? ] ────────► REJECT: NVMe, SATA, SAS, VirtIO internal bus
         │ No (USB or SD/MMC)
         ▼
  [ Removable Media? ] ─────► PASS: Approved for Flashing
         │ No
         ▼
  [ External USB SSD? ] ────► SAFE OPT-IN ONLY (--allow-usb-ssd with confirmation)
```

- **Root Mount Protection**: Scans active system mount tables (`/proc/mounts` on Linux, volume paths on Windows, DiskArbitration on macOS) and identifies all partitions and physical parent devices backing the OS.
- **Fail-Closed Guarantee**: If bus type or mount information cannot be determined with complete confidence, the device is marked blocked.
- **Strict Separation in GUI**: The GUI completely disallows flashing to internal system drives under all circumstances.

---

## 📦 Packaging & Distribution

Inscribe includes complete distribution manifests in the `dist/` directory:

- **Linux AppImage**: Standalone, portable distribution with bundled libraries and desktop integration (`dist/appimage/`).
- **Flatpak**: Sandboxed desktop packaging with D-Bus portal access (`dist/flatpak/`).
- **Desktop & AppStream**: FreeDesktop compliant `.desktop` file (`dist/inscribe.desktop`) and AppStream metadata (`dist/org.inscribe.Inscribe.metainfo.xml`).
- **Polkit Security Policy**: Declares `org.inscribe.flash` action for secure root privilege escalation (`dist/org.inscribe.policy`).
- **macOS Bundle**: Application bundle configuration and `Info.plist` (`dist/macos/`).
- **Windows WiX Installer**: WiX toolset manifests for MSI package generation (`dist/windows/`).

---

## 🧪 Development & Testing

Inscribe enforces a strict engineering quality bar defined in [CONSTRAINTS.md](CONSTRAINTS.md):
- **Zero compiler warnings** (`-D warnings`).
- **Zero suppression attributes** (`no #[allow(...)]` without documented human sign-off).
- **No unimplemented stubs** in production paths.
- **High test coverage** across domain logic.
- **Hardened supply chain** via `cargo-deny` and `cargo-audit`.

### Running Tests

Run the full workspace test suite (unit tests, integration tests, and mock drive tests):

```bash
cargo test --workspace
```

### Static Analysis & Lints

Run clippy with warnings treated as errors:

```bash
cargo clippy --workspace --all-targets -- -D warnings
```

Verify code formatting:

```bash
cargo fmt --all -- --check
```

### Supply Chain Security & Dependency Audit

Audit dependencies against security advisories and banned licenses:

```bash
cargo deny check
```

---

## 🤝 Contributing

Contributions are what make the open-source community such an amazing place to build and collaborate. Any contributions you make are **greatly appreciated**!

1. Fork the Project
2. Create your Feature Branch (`git checkout -b feature/AmazingFeature`)
3. Verify your changes pass all constraints:
   ```bash
   cargo check --workspace --all-targets
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   cargo test --workspace
   ```
4. Commit your Changes (`git commit -m 'feat: Add some AmazingFeature'`)
5. Push to the Branch (`git push origin feature/AmazingFeature`)
6. Open a Pull Request

---

## 📄 License

Inscribe is distributed under the dual **MIT** or **Apache-2.0** license.

See [LICENSE](LICENSE) for more details.

---

<div align="center">
Built with ❤️ by the Inscribe Contributors
</div>
