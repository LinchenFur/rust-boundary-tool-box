//! 显卡驱动和 Windows 防火墙状态检查。
//!
//! 这些检查只读取系统信息，不修改系统设置。UI 根据返回结果提示用户更新
//! NVIDIA 驱动，或在防火墙开启时自行关闭/放行相关程序与端口。

use super::*;

/// 推荐的 NVIDIA 公版驱动主版本。531+ 对 Boundary 社区服更稳。
const RECOMMENDED_NVIDIA_MAJOR: u16 = 531;

/// 一次系统环境检查的完整结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemCheckReport {
    pub nvidia: NvidiaDriverCheck,
    pub firewall: FirewallCheck,
}

impl SystemCheckReport {
    /// 是否存在需要用户处理的环境风险。
    pub fn has_warning(&self) -> bool {
        matches!(self.nvidia, NvidiaDriverCheck::Outdated { .. })
            || matches!(self.firewall, FirewallCheck::Enabled(_))
    }
}

/// NVIDIA 驱动检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NvidiaDriverCheck {
    /// 没有检测到 NVIDIA 显卡。
    NotDetected,
    /// 检测失败或驱动版本不可解析。
    Unknown(String),
    /// 驱动版本达到建议值。
    Ok {
        raw_version: String,
        public_version: String,
    },
    /// 驱动版本低于建议值。
    Outdated {
        raw_version: String,
        public_version: String,
    },
}

/// Windows 防火墙检查结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirewallCheck {
    /// 没能读取防火墙配置。
    Unknown(String),
    /// 三类网络配置文件都关闭了防火墙。
    AllDisabled,
    /// 至少一个网络配置文件开启了防火墙。
    Enabled(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct NvidiaPublicVersion {
    major: u16,
    minor: u16,
}

impl NvidiaPublicVersion {
    fn display(self) -> String {
        format!("{}.{:02}", self.major, self.minor)
    }

    fn is_recommended(self) -> bool {
        self.major >= RECOMMENDED_NVIDIA_MAJOR
    }
}

impl InstallerCore {
    /// 检查 NVIDIA 驱动版本和 Windows 防火墙状态。
    pub fn check_system_status(&self) -> SystemCheckReport {
        SystemCheckReport {
            nvidia: check_nvidia_driver(),
            firewall: check_firewall(),
        }
    }
}

#[cfg(windows)]
fn check_nvidia_driver() -> NvidiaDriverCheck {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let video =
        match hklm.open_subkey_with_flags("SYSTEM\\CurrentControlSet\\Control\\Video", KEY_READ) {
            Ok(key) => key,
            Err(error) => {
                return NvidiaDriverCheck::Unknown(format!("无法读取显卡注册表：{error}"));
            }
        };

    let mut best: Option<(NvidiaPublicVersion, String)> = None;
    let mut found_nvidia = false;
    let mut parse_errors = Vec::new();

    for video_key in video.enum_keys().flatten() {
        for device_index in 0..=3 {
            let device_path = format!("{video_key}\\{device_index:04}");
            let Ok(device) = video.open_subkey_with_flags(&device_path, KEY_READ) else {
                continue;
            };

            let descriptor = registry_string(&device, "DriverDesc");
            let provider = registry_string(&device, "ProviderName");
            if !contains_nvidia(&descriptor) && !contains_nvidia(&provider) {
                continue;
            }

            found_nvidia = true;
            let raw_version = match registry_string(&device, "DriverVersion") {
                Some(version) => version,
                None => {
                    parse_errors.push("检测到 N 卡，但注册表没有 DriverVersion".to_string());
                    continue;
                }
            };

            match parse_nvidia_public_version(&raw_version) {
                Some(version) => {
                    if best.as_ref().is_none_or(|(current, _)| version > *current) {
                        best = Some((version, raw_version));
                    }
                }
                None => parse_errors.push(format!("无法解析驱动版本 {raw_version}")),
            }
        }
    }

    match best {
        Some((version, raw_version)) if version.is_recommended() => NvidiaDriverCheck::Ok {
            raw_version,
            public_version: version.display(),
        },
        Some((version, raw_version)) => NvidiaDriverCheck::Outdated {
            raw_version,
            public_version: version.display(),
        },
        None if found_nvidia => NvidiaDriverCheck::Unknown(parse_errors.join("；")),
        None => NvidiaDriverCheck::NotDetected,
    }
}

#[cfg(not(windows))]
fn check_nvidia_driver() -> NvidiaDriverCheck {
    NvidiaDriverCheck::Unknown("当前平台不支持 NVIDIA 驱动检查".to_string())
}

#[cfg(windows)]
fn check_firewall() -> FirewallCheck {
    use winreg::RegKey;
    use winreg::enums::{HKEY_LOCAL_MACHINE, KEY_READ};

    let hklm = RegKey::predef(HKEY_LOCAL_MACHINE);
    let base = "SYSTEM\\CurrentControlSet\\Services\\SharedAccess\\Parameters\\FirewallPolicy";
    let profiles = [
        ("DomainProfile", "域网络"),
        ("StandardProfile", "专用网络"),
        ("PublicProfile", "公用网络"),
    ];

    let mut read_any = false;
    let mut enabled_profiles = Vec::new();
    let mut errors = Vec::new();

    for (profile_key, display_name) in profiles {
        let path = format!("{base}\\{profile_key}");
        match hklm.open_subkey_with_flags(&path, KEY_READ) {
            Ok(profile) => match profile.get_value::<u32, _>("EnableFirewall") {
                Ok(value) => {
                    read_any = true;
                    if value != 0 {
                        enabled_profiles.push(display_name.to_string());
                    }
                }
                Err(error) => errors.push(format!("{display_name}: {error}")),
            },
            Err(error) => errors.push(format!("{display_name}: {error}")),
        }
    }

    if !read_any {
        FirewallCheck::Unknown(errors.join("；"))
    } else if enabled_profiles.is_empty() {
        FirewallCheck::AllDisabled
    } else {
        FirewallCheck::Enabled(enabled_profiles)
    }
}

#[cfg(not(windows))]
fn check_firewall() -> FirewallCheck {
    FirewallCheck::Unknown("当前平台不支持 Windows 防火墙检查".to_string())
}

#[cfg(windows)]
fn registry_string(key: &winreg::RegKey, name: &str) -> Option<String> {
    key.get_value::<String, _>(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn contains_nvidia(value: &Option<String>) -> bool {
    value
        .as_deref()
        .map(|text| text.to_ascii_lowercase().contains("nvidia"))
        .unwrap_or(false)
}

fn parse_nvidia_public_version(raw: &str) -> Option<NvidiaPublicVersion> {
    let parts = raw
        .split('.')
        .map(str::parse::<u16>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;

    match parts.as_slice() {
        [major, minor] if *major >= 100 => Some(NvidiaPublicVersion {
            major: *major,
            minor: *minor,
        }),
        [_, _, branch, build, ..] if *branch >= 10 => Some(NvidiaPublicVersion {
            major: (*branch - 10) * 100 + (*build / 100),
            minor: *build % 100,
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_windows_nvidia_driver_version() {
        assert_eq!(
            parse_nvidia_public_version("31.0.15.3161").map(NvidiaPublicVersion::display),
            Some("531.61".to_string())
        );
        assert_eq!(
            parse_nvidia_public_version("31.0.15.6603").map(NvidiaPublicVersion::display),
            Some("566.03".to_string())
        );
        assert_eq!(
            parse_nvidia_public_version("27.21.14.5671").map(NvidiaPublicVersion::display),
            Some("456.71".to_string())
        );
    }

    #[test]
    fn parses_public_nvidia_driver_version() {
        assert_eq!(
            parse_nvidia_public_version("551.86").map(NvidiaPublicVersion::display),
            Some("551.86".to_string())
        );
    }

    #[test]
    fn classifies_recommended_nvidia_version() {
        assert!(
            parse_nvidia_public_version("31.0.15.3100")
                .unwrap()
                .is_recommended()
        );
        assert!(
            !parse_nvidia_public_version("31.0.15.2849")
                .unwrap()
                .is_recommended()
        );
    }

    #[test]
    fn warning_tracks_outdated_driver_and_firewall() {
        assert!(
            SystemCheckReport {
                nvidia: NvidiaDriverCheck::Outdated {
                    raw_version: "31.0.15.2849".to_string(),
                    public_version: "528.49".to_string(),
                },
                firewall: FirewallCheck::AllDisabled,
            }
            .has_warning()
        );
        assert!(
            SystemCheckReport {
                nvidia: NvidiaDriverCheck::NotDetected,
                firewall: FirewallCheck::Enabled(vec!["公用网络".to_string()]),
            }
            .has_warning()
        );
    }
}
