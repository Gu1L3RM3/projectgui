use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallMethod {
    /// Installed by the cargo-dist shell/PowerShell installer; can update itself.
    Installer,
    Homebrew,
    Cargo,
    /// Distro package or other system location.
    System,
    Unknown,
}

impl InstallMethod {
    pub fn name(self) -> &'static str {
        match self {
            Self::Installer => "installer",
            Self::Homebrew => "homebrew",
            Self::Cargo => "cargo",
            Self::System => "system package",
            Self::Unknown => "unknown",
        }
    }

    /// What the user should run when we must not update in place.
    pub fn upgrade_hint(self) -> &'static str {
        match self {
            Self::Installer => "pgui update",
            Self::Homebrew => "brew upgrade pgui",
            Self::Cargo => "cargo install pgui --locked --force",
            Self::System => {
                "update it with your package manager (apt, dnf, pacman, winget, ...) or reinstall from the releases page"
            }
            Self::Unknown => {
                "download the latest release from the releases page and replace this binary"
            }
        }
    }
}

/// `has_receipt` is true when the cargo-dist install receipt for this app exists.
pub fn classify(exe: &Path, has_receipt: bool) -> InstallMethod {
    let p = exe.to_string_lossy().replace('\\', "/");
    if p.contains("/Cellar/") || p.contains("/homebrew/") || p.contains("/linuxbrew/") {
        InstallMethod::Homebrew
    } else if has_receipt {
        InstallMethod::Installer
    } else if p.contains("/.cargo/bin/") {
        InstallMethod::Cargo
    } else if p.starts_with("/usr/") || p.starts_with("/opt/") || p.contains("/Program Files") {
        InstallMethod::System
    } else {
        InstallMethod::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_by_location_and_receipt() {
        let c = |p: &str, r| classify(Path::new(p), r);
        assert_eq!(c("/home/u/.local/bin/pgui", true), InstallMethod::Installer);
        assert_eq!(
            c("/opt/homebrew/Cellar/pgui/0.1/bin/pgui", true),
            InstallMethod::Homebrew
        );
        assert_eq!(c("/home/u/.cargo/bin/pgui", false), InstallMethod::Cargo);
        assert_eq!(c("/usr/bin/pgui", false), InstallMethod::System);
        assert_eq!(c("C:\\Users\\u\\pgui.exe", false), InstallMethod::Unknown);
        assert_eq!(
            c("/home/u/dev/target/debug/pgui", false),
            InstallMethod::Unknown
        );
    }
}
