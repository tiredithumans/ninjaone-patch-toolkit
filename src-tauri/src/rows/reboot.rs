//! Whether installing a patch is likely to need a reboot, estimated from its name.
//!
//! NinjaOne's patch records carry no reboot field at all: `DeviceOSPatch` and
//! `DeviceSoftwarePatch` say nothing about it (see `docs/api/ninjaone-surface.md`).
//! The only reboot signal the API has is the *device's* `os.needsReboot`, which turns
//! on after an install, so a pending kernel update read "Needs Reboot: No" right up
//! until it was installed. This is the per-patch half the vendor does not send.
//!
//! It answers "likely" or "no signal", never "no": a name this does not recognise
//! says nothing about whether the install will reboot, and printing "No" for it would
//! repeat the bug this exists to fix.

/// The OS families patch management covers, from a device's `nodeClass`. Each has
/// its own naming for the updates that need a restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsFamily {
    Linux,
    Windows,
    Mac,
}

impl OsFamily {
    /// `None` for a class patch management does not cover, or no class at all.
    pub fn of(node_class: Option<&str>) -> Option<Self> {
        let class = node_class?;
        if starts_with_ci(class, "LINUX") {
            Some(Self::Linux)
        } else if starts_with_ci(class, "WINDOWS") {
            Some(Self::Windows)
        } else if starts_with_ci(class, "MAC") {
            Some(Self::Mac)
        } else {
            None
        }
    }

    /// Slot in the join's per-family memo.
    pub(super) const fn index(self) -> usize {
        match self {
            Self::Linux => 0,
            Self::Windows => 1,
            Self::Mac => 2,
        }
    }
}

/// Whether an **OS** patch with this name is likely to need a reboot on a device of
/// this family. Third-party patches are never estimated: an application update
/// restarts the application, not the machine.
pub fn reboot_likely(family: OsFamily, name: &str) -> bool {
    match family {
        OsFamily::Linux => name.split_whitespace().any(linux_package_needs_reboot),
        OsFamily::Windows => windows_update_needs_reboot(name),
        OsFamily::Mac => {
            starts_with_ci(name.trim_start(), "macOS")
                || contains_ci(name, "Rapid Security Response")
        }
    }
}

/// Packages whose update needs a reboot to take effect, by exact base name. Drawn
/// from the lists the distributions themselves use to decide this: RHEL's
/// `needs-restarting -r` and Debian/Ubuntu's `reboot-required` triggers.
const LINUX_REBOOT_PACKAGES: [&str; 20] = [
    "kernel",
    "kernel-core",
    "kernel-default",
    "glibc",
    "libc6",
    "systemd",
    "systemd-udev",
    "udev",
    "dbus",
    "dbus-daemon",
    "dbus-broker",
    "linux-firmware",
    "intel-microcode",
    "amd64-microcode",
    "microcode_ctl",
    "openssl-libs",
    "gnutls",
    "linux-base",
    "linux-generic",
    "linux-virtual",
];

/// Package families matched as the name itself or the name plus a `-suffix`
/// (`kernel-modules-extra`, `linux-image-generic`, `linux-generic-hwe`), minus the
/// [`NOT_RUNTIME`] build and documentation packages.
const LINUX_REBOOT_FAMILIES: [&str; 8] = [
    "kernel-modules",
    "kernel-rt",
    "kernel-uek",
    "kernel-64k",
    "linux-image",
    "linux-modules",
    "linux-generic",
    "linux-virtual",
];

/// Prefixes matched with any continuation, for libraries whose package name carries
/// the soname (`libssl3`, `libssl1.1`, `libgnutls30t64`).
const LINUX_REBOOT_PREFIXES: [&str; 2] = ["libssl", "libgnutls"];

/// Name segments that mark a package as headers, a toolchain or documentation —
/// installed beside the kernel or a library, but never loaded by the running system.
const NOT_RUNTIME: [&str; 10] = [
    "headers",
    "devel",
    "dev",
    "tools",
    "doc",
    "docs",
    "debuginfo",
    "debugsource",
    "src",
    "source",
];

const LINUX_ARCH_SUFFIXES: [&str; 7] = [
    ".x86_64", ".aarch64", ".noarch", ".i686", ".i386", ".ppc64le", ".s390x",
];

fn linux_package_needs_reboot(word: &str) -> bool {
    let base = linux_package_base(word);
    if base.is_empty() {
        return false;
    }
    if LINUX_REBOOT_PACKAGES.contains(&base.as_str()) {
        return true;
    }
    if base.split(['-', '_']).any(|seg| NOT_RUNTIME.contains(&seg)) {
        return false;
    }
    LINUX_REBOOT_FAMILIES.iter().any(|family| {
        base == *family
            || base
                .strip_prefix(family)
                .is_some_and(|rest| rest.starts_with('-'))
    }) || LINUX_REBOOT_PREFIXES.iter().any(|p| base.starts_with(p))
}

/// A package's base name from however a feed spells it: a bare name (`kernel`), an
/// RPM NEVRA (`kernel-core-5.14.0-427.13.1.el9_4.x86_64`), a Debian versioned name
/// (`linux-image-6.8.0-45-generic`, `libc6_2.35-0ubuntu3.8`) or a multiarch name
/// (`libc6:amd64`). Lowercased; the version is everything from the first `-` or `_`
/// followed by a digit, which no package name in either family's list contains.
fn linux_package_base(word: &str) -> String {
    let word = word.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || "-_.+:".contains(c)));
    let mut base = word
        .split(':')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if let Some(len) = LINUX_ARCH_SUFFIXES
        .iter()
        .find_map(|arch| base.strip_suffix(arch).map(str::len))
    {
        base.truncate(len);
    }
    let bytes = base.as_bytes();
    if let Some(cut) = (0..bytes.len().saturating_sub(1))
        .find(|&i| matches!(bytes[i], b'-' | b'_') && bytes[i + 1].is_ascii_digit())
    {
        base.truncate(cut);
    }
    base
}

/// Windows update classes that restart the machine to finish installing. Checked
/// after [`WINDOWS_NO_REBOOT`], because a Defender definition update's title also
/// reads like a security update.
const WINDOWS_REBOOT: [&str; 5] = [
    "cumulative update",
    "monthly quality rollup",
    "security only quality update",
    "servicing stack update",
    "feature update",
];

const WINDOWS_NO_REBOOT: [&str; 3] = [
    "security intelligence update",
    "definition update",
    "malicious software removal tool",
];

fn windows_update_needs_reboot(name: &str) -> bool {
    !WINDOWS_NO_REBOOT.iter().any(|n| contains_ci(name, n))
        && WINDOWS_REBOOT.iter().any(|n| contains_ci(name, n))
}

fn starts_with_ci(haystack: &str, prefix: &str) -> bool {
    haystack
        .get(..prefix.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
}

/// Case-insensitive substring test without lowercasing either side into a new
/// `String`; the join calls this once per distinct patch title.
fn contains_ci(haystack: &str, needle: &str) -> bool {
    let (h, n) = (haystack.as_bytes(), needle.as_bytes());
    n.is_empty() || h.windows(n.len()).any(|w| w.eq_ignore_ascii_case(n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_packages_from_every_distribution_are_likely() {
        for name in [
            "kernel",
            "kernel-core",
            "kernel-5.14.0-427.13.1.el9_4.x86_64",
            "kernel-core-5.14.0-427.13.1.el9_4.x86_64",
            "kernel-modules-extra-5.14.0-427.13.1.el9_4",
            "kernel-default-5.14.21-150500.55.39.1",
            "kernel-uek-5.15.0-206.153.7.el8uek",
            "kernel-rt-core",
            "linux-image-6.8.0-45-generic",
            "linux-image-generic",
            "linux-image-amd64",
            "linux-modules-extra-6.8.0-45-generic",
            "linux-generic",
            "linux-generic-hwe-22.04",
            "Linux-Image-Generic 6.8.0.45.45",
            "Security update for kernel",
        ] {
            assert!(reboot_likely(OsFamily::Linux, name), "{name:?}");
        }
    }

    /// Spelled exactly as NinjaOne listed a RHEL 9 server's pending OS patches:
    /// package, a space, then the version-release. The kernel packages that load
    /// into the running system are likely; the userland built from the same source
    /// (perf bindings, kernel tools) and unrelated packages are not.
    #[test]
    fn a_real_fleets_rhel_9_patch_names_are_classified() {
        for name in [
            "kernel-core 5.14.0-687.60.1.el9_8",
            "kernel-modules-core 5.14.0-687.60.1.el9_8",
            "kernel-modules 5.14.0-687.60.1.el9_8",
            "kernel 5.14.0-687.60.1.el9_8",
        ] {
            assert!(reboot_likely(OsFamily::Linux, name), "{name:?}");
        }
        for name in [
            "python3-perf 5.14.0-687.60.1.el9_8",
            "kernel-tools-libs 5.14.0-687.60.1.el9_8",
            "kernel-tools 5.14.0-687.60.1.el9_8",
            "expat 2.5.0-6.el9_8.7",
            "ca-certificates 2026.2.92_v9.0.319-90.3.el9_8",
        ] {
            assert!(!reboot_likely(OsFamily::Linux, name), "{name:?}");
        }
    }

    #[test]
    fn core_runtime_libraries_are_likely() {
        for name in [
            "glibc-2.34-100.el9_4.2.x86_64",
            "libc6",
            "libc6:amd64",
            "libc6_2.35-0ubuntu3.8",
            "systemd",
            "dbus",
            "linux-firmware",
            "intel-microcode",
            "openssl-libs",
            "libssl3",
            "libssl1.1",
            "libgnutls30t64",
        ] {
            assert!(reboot_likely(OsFamily::Linux, name), "{name:?}");
        }
    }

    #[test]
    fn headers_toolchain_and_userland_packages_are_not() {
        for name in [
            "kernel-headers-5.14.0-427.el9",
            "kernel-devel",
            "kernel-tools",
            "kernel-rt-devel",
            "linux-headers-6.8.0-45-generic",
            "linux-libc-dev",
            "libssl-dev",
            "glibc-common",
            "glibc-langpack-en",
            "systemd-libs",
            "openssl",
            "nginx",
            "curl-8.5.0",
            "kernelshark",
        ] {
            assert!(!reboot_likely(OsFamily::Linux, name), "{name:?}");
        }
    }

    #[test]
    fn windows_cumulative_and_servicing_updates_are_likely() {
        for name in [
            "KB5044281 · 2024-10 Cumulative Update for Windows Server 2022 for x64-based Systems (KB5044281)",
            "2024-10 Cumulative Update for .NET Framework 3.5, 4.8 and 4.8.1 for Windows 10",
            "2024-10 Security Monthly Quality Rollup for Windows Server 2012 R2",
            "2024-10 Servicing Stack Update for Windows Server 2016",
            "Feature update to Windows 11, version 24H2",
        ] {
            assert!(reboot_likely(OsFamily::Windows, name), "{name:?}");
        }
    }

    #[test]
    fn windows_definition_and_tool_updates_are_not() {
        for name in [
            "Security Intelligence Update for Microsoft Defender Antivirus - KB2267602 (Version 1.419.1)",
            "Windows Malicious Software Removal Tool x64 - v5.129 (KB890830)",
            "Update for Microsoft Defender Antivirus antimalware platform - KB4052623",
        ] {
            assert!(!reboot_likely(OsFamily::Windows, name), "{name:?}");
        }
    }

    #[test]
    fn macos_system_updates_are_likely_and_app_updates_are_not() {
        assert!(reboot_likely(OsFamily::Mac, "macOS Sequoia 15.5"));
        assert!(reboot_likely(
            OsFamily::Mac,
            "Rapid Security Response macOS 13.4.1 (a)"
        ));
        assert!(!reboot_likely(OsFamily::Mac, "Safari 18.5"));
        assert!(!reboot_likely(
            OsFamily::Mac,
            "Command Line Tools for Xcode 16.4"
        ));
    }

    #[test]
    fn the_family_comes_from_the_node_class() {
        assert_eq!(OsFamily::of(Some("LINUX_SERVER")), Some(OsFamily::Linux));
        assert_eq!(
            OsFamily::of(Some("windows_workstation")),
            Some(OsFamily::Windows)
        );
        assert_eq!(OsFamily::of(Some("MAC_SERVER")), Some(OsFamily::Mac));
        assert_eq!(OsFamily::of(Some("NMS_SWITCH")), None);
        assert_eq!(OsFamily::of(None), None);
    }
}
