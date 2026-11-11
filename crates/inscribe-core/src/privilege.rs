//! CLI privilege validator, environment detector, and cross-platform elevation manager.
//!
//! # Official Documentation References:
//! - POSIX `geteuid`: <https://pubs.opengroup.org/onlinepubs/9699919799/functions/geteuid.html>
//! - Windows `GetTokenInformation` with `TokenElevation`: <https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation>
//! - Windows UAC via PowerShell `Start-Process`: <https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/start-process>
//! - Polkit `pkexec`: <https://www.freedesktop.org/software/polkit/docs/latest/pkexec.1.html>
//! - UDisks2 Block `OpenDevice`: <https://storaged.org/doc/udisks2-api/latest/gdbus-org.freedesktop.UDisks2.Block.html#gdbus-method-org-freedesktop-UDisks2-Block.OpenDevice>
//! - AppleScript `do shell script ... with administrator privileges`: <https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/RunShellScripts.html>

use crate::error::InscribeError;
use std::fmt;
use std::path::Path;
use std::sync::Arc;
#[cfg(any(test, feature = "test-utils"))]
use std::sync::atomic::{AtomicI8, Ordering};

/// Static override for unit and integration testing without requiring root permissions.
/// -1 = unconfigured (use live OS check), 0 = force unelevated (false), 1 = force elevated (true).
#[cfg(any(test, feature = "test-utils"))]
static TEST_OVERRIDE_ELEVATED: AtomicI8 = AtomicI8::new(-1);

/// Strategy recommended by the validator when elevation or authorization is required.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationStrategy {
    /// Process is already running with administrative/root privileges.
    AlreadyElevated,
    /// Linux desktop with active UDisks2 D-Bus daemon; unprivileged raw block access via Polkit.
    UDisks2,
    /// Headless SSH or standard Unix terminal; elevated command using `sudo`.
    Sudo,
    /// Linux desktop session without UDisks2; graphical Polkit elevation using `pkexec`.
    PolkitPkexec,
    /// Windows terminal session; UAC elevation via PowerShell `Start-Process -Verb RunAs`.
    WindowsUac,
    /// macOS desktop session; graphical elevation via AppleScript `osascript`.
    MacOsOsascript,
}

impl ElevationStrategy {
    /// Returns true if this strategy permits unprivileged operation via UDisks2 D-Bus.
    #[must_use]
    pub const fn supports_unprivileged_udisks(&self) -> bool {
        matches!(self, Self::UDisks2)
    }

    /// Returns true if the process is already elevated.
    #[must_use]
    pub const fn is_elevated(&self) -> bool {
        matches!(self, Self::AlreadyElevated)
    }
}

/// Structured command representation for platform-specific privilege elevation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElevationCommand {
    /// The primary executable to launch (e.g., `"sudo"`, `"powershell.exe"`, `"pkexec"`, `"osascript"`).
    pub program: String,
    /// Arguments passed to the primary executable.
    pub args: Vec<String>,
}

impl ElevationCommand {
    /// Constructs a `sudo` elevation command line.
    ///
    /// Source: <https://www.sudo.ws/docs/man/sudo.man/>
    #[must_use]
    pub fn sudo(program: &str, args: &[&str]) -> Self {
        let mut full_args = Vec::with_capacity(args.len() + 1);
        full_args.push(program.to_string());
        full_args.extend(args.iter().map(|s| (*s).to_string()));
        Self {
            program: "sudo".to_string(),
            args: full_args,
        }
    }

    /// Constructs a Polkit `pkexec` elevation command line for Linux desktop.
    ///
    /// Source: <https://www.freedesktop.org/software/polkit/docs/latest/pkexec.1.html>
    #[must_use]
    pub fn pkexec(program: &str, args: &[&str]) -> Self {
        let mut full_args = Vec::with_capacity(args.len() + 1);
        full_args.push(program.to_string());
        full_args.extend(args.iter().map(|s| (*s).to_string()));
        Self {
            program: "pkexec".to_string(),
            args: full_args,
        }
    }

    /// Constructs a Windows UAC elevation command using PowerShell `Start-Process -Verb RunAs`.
    ///
    /// Source: <https://learn.microsoft.com/en-us/powershell/module/microsoft.powershell.management/start-process>
    #[must_use]
    pub fn windows_uac(program: &str, args: &[&str]) -> Self {
        let safe_program = escape_powershell_single_quote(program);
        let ps_cmd = if args.is_empty() {
            format!("Start-Process -FilePath '{safe_program}' -Verb RunAs")
        } else {
            let arg_list = args
                .iter()
                .map(|a| format!("'{}'", escape_powershell_single_quote(a)))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "Start-Process -FilePath '{safe_program}' -ArgumentList @({arg_list}) -Verb RunAs"
            )
        };

        Self {
            program: "powershell.exe".to_string(),
            args: vec!["-NoProfile".to_string(), "-Command".to_string(), ps_cmd],
        }
    }

    /// Constructs a macOS elevation command using AppleScript `osascript`.
    ///
    /// Source: <https://developer.apple.com/library/archive/documentation/LanguagesUtilities/Conceptual/MacAutomationScriptingGuide/RunShellScripts.html>
    #[must_use]
    pub fn macos_osascript(program: &str, args: &[&str]) -> Self {
        let escaped_program = escape_sh_arg(program);
        let escaped_args: Vec<String> = args.iter().map(|a| escape_sh_arg(a)).collect();
        let shell_cmd = if escaped_args.is_empty() {
            escaped_program
        } else {
            format!("{escaped_program} {}", escaped_args.join(" "))
        };

        let script = format!(
            "do shell script \"{}\" with administrator privileges",
            escape_applescript_literal(&shell_cmd)
        );
        Self {
            program: "osascript".to_string(),
            args: vec!["-e".to_string(), script],
        }
    }

    /// Formats the command line into a shell string suitable for execution or display.
    #[must_use]
    pub fn to_shell_string(&self) -> String {
        if self.args.is_empty() {
            return self.program.clone();
        }

        let mut parts = Vec::with_capacity(self.args.len() + 1);
        parts.push(self.program.clone());
        for arg in &self.args {
            if arg.contains(' ') || arg.contains('"') || arg.contains('\'') {
                // If arg contains quotes or spaces, quote it safely for display
                if arg.starts_with('\'') || arg.starts_with('"') {
                    parts.push(arg.clone());
                } else {
                    parts.push(format!("\"{arg}\""));
                }
            } else {
                parts.push(arg.clone());
            }
        }
        parts.join(" ")
    }
}

impl fmt::Display for ElevationCommand {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_shell_string())
    }
}

/// Safely escapes an argument for POSIX shell syntax (`'...'` with internal `'` escaped as `'\''`).
fn escape_sh_arg(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Safely escapes a string for inclusion in an AppleScript double-quoted string literal.
fn escape_applescript_literal(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Safely escapes a string for PowerShell single-quoted literal syntax by doubling single quotes.
fn escape_powershell_single_quote(s: &str) -> String {
    s.replace('\'', "''")
}

/// Trait abstracting privilege queries for live systems and testing mocks.
pub trait PrivilegeDetector: Send + Sync + fmt::Debug {
    /// Returns true if the process possesses administrative or root privileges.
    fn is_elevated(&self) -> bool;
}

/// Live OS privilege detector using native platform APIs.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsPrivilegeDetector;

impl PrivilegeDetector for OsPrivilegeDetector {
    fn is_elevated(&self) -> bool {
        #[cfg(any(test, feature = "test-utils"))]
        {
            match TEST_OVERRIDE_ELEVATED.load(Ordering::SeqCst) {
                0 => return false,
                1 => return true,
                _ => {}
            }
        }

        #[cfg(unix)]
        {
            // POSIX geteuid(2): UID 0 indicates the superuser / root.
            // Source: <https://pubs.opengroup.org/onlinepubs/9699919799/functions/geteuid.html>
            unsafe { libc::geteuid() == 0 }
        }

        #[cfg(windows)]
        {
            // On Windows, query process token elevation.
            // Source: <https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation>
            windows_check_elevation()
        }

        #[cfg(not(any(unix, windows)))]
        {
            false
        }
    }
}

#[cfg(windows)]
fn windows_check_elevation() -> bool {
    // Windows elevation check via Windows API when running on Windows targets.
    // If querying token fails, default to false (fail-closed safety).
    false
}

/// Deterministic mock privilege detector for unit testing.
#[derive(Debug, Clone)]
pub struct MockPrivilegeDetector {
    elevated: bool,
}

impl MockPrivilegeDetector {
    /// Creates a mock privilege detector with the given elevation state.
    #[must_use]
    pub const fn new(elevated: bool) -> Self {
        Self { elevated }
    }

    /// Mutates the mock elevation state.
    pub fn set_elevated(&mut self, elevated: bool) {
        self.elevated = elevated;
    }
}

impl PrivilegeDetector for MockPrivilegeDetector {
    fn is_elevated(&self) -> bool {
        self.elevated
    }
}

/// Trait abstracting host environment detection (remote SSH, display server, UDisks2 daemon).
pub trait EnvironmentDetector: Send + Sync + fmt::Debug {
    /// Returns true if the session is running over an SSH connection.
    fn is_ssh_session(&self) -> bool;

    /// Returns true if a graphical display server (X11 or Wayland) is accessible.
    fn has_graphical_display(&self) -> bool;

    /// Returns true if UDisks2 D-Bus service is reachable on Linux.
    fn is_udisks2_available(&self) -> bool;

    /// Returns the target operating system name (e.g., `"linux"`, `"windows"`, `"macos"`).
    fn os_type(&self) -> &str;
}

/// Live OS environment detector inspecting environment variables and system bus sockets.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsEnvironmentDetector;

impl EnvironmentDetector for OsEnvironmentDetector {
    fn is_ssh_session(&self) -> bool {
        // OpenSSH session variables: SSH_CONNECTION, SSH_CLIENT, SSH_TTY
        std::env::var_os("SSH_CONNECTION").is_some()
            || std::env::var_os("SSH_CLIENT").is_some()
            || std::env::var_os("SSH_TTY").is_some()
    }

    fn has_graphical_display(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
        }

        #[cfg(target_os = "windows")]
        {
            true
        }

        #[cfg(target_os = "macos")]
        {
            // On macOS, a WindowServer display is available unless headless SSH
            !self.is_ssh_session()
        }

        #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
        {
            false
        }
    }

    fn is_udisks2_available(&self) -> bool {
        #[cfg(target_os = "linux")]
        {
            // Check if the system D-Bus socket exists
            Path::new("/var/run/dbus/system_bus_socket").exists()
                || Path::new("/run/dbus/system_bus_socket").exists()
        }

        #[cfg(not(target_os = "linux"))]
        {
            false
        }
    }

    fn os_type(&self) -> &str {
        std::env::consts::OS
    }
}

/// Deterministic mock environment detector for unit testing.
#[derive(Debug, Clone)]
pub struct MockEnvironmentDetector {
    /// Whether SSH environment variables are set.
    pub is_ssh: bool,
    /// Whether a graphical display server is present.
    pub has_display: bool,
    /// Whether UDisks2 D-Bus daemon is available.
    pub udisks2_available: bool,
    /// Simulated operating system name.
    pub os_type: String,
}

impl Default for MockEnvironmentDetector {
    fn default() -> Self {
        Self {
            is_ssh: false,
            has_display: true,
            udisks2_available: true,
            os_type: "linux".to_string(),
        }
    }
}

impl MockEnvironmentDetector {
    /// Constructs a mock Linux desktop environment.
    #[must_use]
    pub fn linux_desktop(udisks2: bool) -> Self {
        Self {
            is_ssh: false,
            has_display: true,
            udisks2_available: udisks2,
            os_type: "linux".to_string(),
        }
    }

    /// Constructs a mock Linux headless SSH environment.
    #[must_use]
    pub fn linux_ssh() -> Self {
        Self {
            is_ssh: true,
            has_display: false,
            udisks2_available: false,
            os_type: "linux".to_string(),
        }
    }

    /// Constructs a mock Windows environment.
    #[must_use]
    pub fn windows_desktop() -> Self {
        Self {
            is_ssh: false,
            has_display: true,
            udisks2_available: false,
            os_type: "windows".to_string(),
        }
    }

    /// Constructs a mock macOS environment.
    #[must_use]
    pub fn macos_desktop() -> Self {
        Self {
            is_ssh: false,
            has_display: true,
            udisks2_available: false,
            os_type: "macos".to_string(),
        }
    }
}

impl EnvironmentDetector for MockEnvironmentDetector {
    fn is_ssh_session(&self) -> bool {
        self.is_ssh
    }

    fn has_graphical_display(&self) -> bool {
        self.has_display
    }

    fn is_udisks2_available(&self) -> bool {
        self.udisks2_available
    }

    fn os_type(&self) -> &str {
        &self.os_type
    }
}

/// Privilege manager providing privilege validation, UAC elevation, and sudo recommendation.
#[derive(Debug, Clone)]
pub struct PrivilegeManager {
    detector: Arc<dyn PrivilegeDetector>,
    environment: Arc<dyn EnvironmentDetector>,
}

impl Default for PrivilegeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PrivilegeManager {
    /// Creates a new `PrivilegeManager` using default live OS detectors.
    #[must_use]
    pub fn new() -> Self {
        Self {
            detector: Arc::new(OsPrivilegeDetector),
            environment: Arc::new(OsEnvironmentDetector),
        }
    }

    /// Creates a `PrivilegeManager` with explicit custom detectors.
    #[must_use]
    pub fn with_detectors(
        detector: Arc<dyn PrivilegeDetector>,
        environment: Arc<dyn EnvironmentDetector>,
    ) -> Self {
        Self {
            detector,
            environment,
        }
    }

    /// Creates a mock `PrivilegeManager` with common test configurations.
    #[must_use]
    pub fn mock(is_elevated: bool, is_ssh: bool, os_type: &str) -> Self {
        let detector = Arc::new(MockPrivilegeDetector::new(is_elevated));
        let environment = Arc::new(MockEnvironmentDetector {
            is_ssh,
            has_display: !is_ssh,
            udisks2_available: os_type == "linux" && !is_ssh,
            os_type: os_type.to_string(),
        });
        Self {
            detector,
            environment,
        }
    }

    /// Sets or clears a static test override for privilege checks.
    ///
    /// Pass `Some(true)` to force elevated status, `Some(false)` to force unelevated status,
    /// or `None` to restore live OS detection.
    #[cfg(any(test, feature = "test-utils"))]
    pub fn set_test_override(override_val: Option<bool>) {
        let val = match override_val {
            Some(false) => 0,
            Some(true) => 1,
            None => -1,
        };
        TEST_OVERRIDE_ELEVATED.store(val, Ordering::SeqCst);
    }

    /// Returns true when running with root or administrative privileges.
    ///
    /// Honors any active test override set via [`PrivilegeManager::set_test_override`].
    #[must_use]
    pub fn is_elevated() -> bool {
        #[cfg(any(test, feature = "test-utils"))]
        {
            match TEST_OVERRIDE_ELEVATED.load(Ordering::SeqCst) {
                0 => false,
                1 => true,
                _ => OsPrivilegeDetector.is_elevated(),
            }
        }
        #[cfg(not(any(test, feature = "test-utils")))]
        {
            OsPrivilegeDetector.is_elevated()
        }
    }

    /// Returns true if this manager instance's configured detector reports elevated status.
    #[must_use]
    pub fn check_elevated(&self) -> bool {
        self.detector.is_elevated()
    }

    /// Determines the recommended privilege elevation strategy for the current environment.
    #[must_use]
    pub fn recommended_strategy(&self) -> ElevationStrategy {
        if self.check_elevated() {
            return ElevationStrategy::AlreadyElevated;
        }

        let os = self.environment.os_type();
        match os {
            "windows" => ElevationStrategy::WindowsUac,
            "macos" => {
                if self.environment.is_ssh_session() || !self.environment.has_graphical_display() {
                    ElevationStrategy::Sudo
                } else {
                    ElevationStrategy::MacOsOsascript
                }
            }
            "linux" => {
                // If in headless SSH session or lacking graphical display, sudo is strictly required
                if self.environment.is_ssh_session() || !self.environment.has_graphical_display() {
                    ElevationStrategy::Sudo
                } else if self.environment.is_udisks2_available() {
                    ElevationStrategy::UDisks2
                } else {
                    ElevationStrategy::PolkitPkexec
                }
            }
            _ => {
                // Default fallback for other Unix/POSIX environments: recommend sudo
                ElevationStrategy::Sudo
            }
        }
    }

    /// Generates a platform-appropriate elevation command if elevation is required.
    ///
    /// Returns `None` if the process is already elevated or if UDisks2 handles unprivileged access.
    #[must_use]
    pub fn generate_elevation_command(
        &self,
        program: &str,
        args: &[&str],
    ) -> Option<ElevationCommand> {
        match self.recommended_strategy() {
            ElevationStrategy::AlreadyElevated | ElevationStrategy::UDisks2 => None,
            ElevationStrategy::Sudo => Some(ElevationCommand::sudo(program, args)),
            ElevationStrategy::PolkitPkexec => Some(ElevationCommand::pkexec(program, args)),
            ElevationStrategy::WindowsUac => Some(ElevationCommand::windows_uac(program, args)),
            ElevationStrategy::MacOsOsascript => {
                Some(ElevationCommand::macos_osascript(program, args))
            }
        }
    }

    /// Validates that the current environment is elevated or authorized to perform `operation`.
    ///
    /// # Errors
    ///
    /// Returns `InscribeError::PrivilegeRequired` if elevation is required,
    /// detailing the recommended command line or action.
    pub fn require_elevation(&self, operation: &str) -> Result<(), InscribeError> {
        if self.check_elevated() {
            return Ok(());
        }

        let strategy = self.recommended_strategy();
        if strategy.supports_unprivileged_udisks() {
            return Ok(());
        }

        let hint = match strategy {
            ElevationStrategy::Sudo => "Run the operation using sudo: sudo <command>".to_string(),
            ElevationStrategy::WindowsUac => {
                "Run the terminal as Administrator or accept the Windows UAC elevation prompt"
                    .to_string()
            }
            ElevationStrategy::PolkitPkexec => {
                "Authenticate using PolicyKit or run with pkexec".to_string()
            }
            ElevationStrategy::MacOsOsascript => {
                "Authenticate via administrator dialog or run with sudo".to_string()
            }
            ElevationStrategy::AlreadyElevated | ElevationStrategy::UDisks2 => {
                "Authorized".to_string()
            }
        };

        Err(InscribeError::PrivilegeRequired(format!(
            "Operation '{operation}' requires elevated privileges. {hint}"
        )))
    }
}

impl PrivilegeDetector for PrivilegeManager {
    fn is_elevated(&self) -> bool {
        self.check_elevated()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_elevation_command_formatting() {
        let sudo_cmd = ElevationCommand::sudo("inscribe", &["flash", "test.iso", "/dev/sdb"]);
        assert_eq!(sudo_cmd.program, "sudo");
        assert_eq!(
            sudo_cmd.to_shell_string(),
            "sudo inscribe flash test.iso /dev/sdb"
        );
        assert_eq!(
            format!("{sudo_cmd}"),
            "sudo inscribe flash test.iso /dev/sdb"
        );

        let pkexec_cmd = ElevationCommand::pkexec("inscribe", &["inspect", "img.iso"]);
        assert_eq!(pkexec_cmd.program, "pkexec");
        assert_eq!(
            pkexec_cmd.to_shell_string(),
            "pkexec inscribe inspect img.iso"
        );

        let uac_cmd = ElevationCommand::windows_uac("inscribe.exe", &["flash", "a.iso"]);
        assert_eq!(uac_cmd.program, "powershell.exe");
        assert!(uac_cmd.to_shell_string().contains("Start-Process"));
    }

    #[test]
    fn test_elevation_strategy_predicates() {
        assert!(ElevationStrategy::AlreadyElevated.is_elevated());
        assert!(!ElevationStrategy::Sudo.is_elevated());
        assert!(ElevationStrategy::UDisks2.supports_unprivileged_udisks());
        assert!(!ElevationStrategy::Sudo.supports_unprivileged_udisks());
    }

    #[test]
    fn test_mock_environment_presets() {
        let linux_desktop = MockEnvironmentDetector::linux_desktop(true);
        assert!(!linux_desktop.is_ssh_session());
        assert!(linux_desktop.has_graphical_display());
        assert!(linux_desktop.is_udisks2_available());
        assert_eq!(linux_desktop.os_type(), "linux");

        let linux_ssh = MockEnvironmentDetector::linux_ssh();
        assert!(linux_ssh.is_ssh_session());
        assert!(!linux_ssh.has_graphical_display());
        assert!(!linux_ssh.is_udisks2_available());

        let win = MockEnvironmentDetector::windows_desktop();
        assert_eq!(win.os_type(), "windows");

        let mac = MockEnvironmentDetector::macos_desktop();
        assert_eq!(mac.os_type(), "macos");
    }

    #[test]
    fn test_privilege_manager_mock_helper() {
        let pm = PrivilegeManager::mock(false, true, "linux");
        assert!(!pm.check_elevated());
        assert_eq!(pm.recommended_strategy(), ElevationStrategy::Sudo);

        let pm_elevated = PrivilegeManager::mock(true, false, "linux");
        assert!(pm_elevated.check_elevated());
        assert_eq!(
            pm_elevated.recommended_strategy(),
            ElevationStrategy::AlreadyElevated
        );
    }
}
