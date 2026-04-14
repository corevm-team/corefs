// Copyright (c) 2026 Christian Möller
// SPDX-License-Identifier: MIT

use crate::error::{CoreFsError, CoreFsResult};
use crate::platform::linux_fuse::LinuxMountOptions;
use crate::services::integrity::{ImageIntegrityReport, IntegrityService};
use std::fs::{self, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticStatus {
    Pass,
    Warn,
    Fail,
}

impl DiagnosticStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub name: String,
    pub status: DiagnosticStatus,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MountDiagnosisReport {
    pub image_path: PathBuf,
    pub mountpoint: PathBuf,
    pub checks: Vec<DiagnosticCheck>,
}

impl MountDiagnosisReport {
    pub fn has_failures(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DiagnosticStatus::Fail)
    }

    pub fn has_warnings(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.status == DiagnosticStatus::Warn)
    }

    pub fn summary(&self) -> &'static str {
        if self.has_failures() {
            "mount-not-ready"
        } else if self.has_warnings() {
            "mount-ready-with-warnings"
        } else {
            "mount-ready"
        }
    }
}

pub fn diagnose_mount(
    image_path: impl AsRef<Path>,
    mountpoint: impl AsRef<Path>,
    options: &LinuxMountOptions,
) -> MountDiagnosisReport {
    let image_path = image_path.as_ref().to_path_buf();
    let mountpoint = mountpoint.as_ref().to_path_buf();
    let mut checks = Vec::new();

    checks.push(diagnose_platform());
    checks.push(diagnose_image_path(&image_path, options));
    checks.push(diagnose_mountpoint(&mountpoint));
    checks.push(diagnose_fuse_kernel_support());
    checks.push(diagnose_fuse_device());
    checks.push(diagnose_current_identity());
    checks.push(diagnose_userspace_tooling());
    checks.push(diagnose_fusermount_permissions());
    checks.push(diagnose_fuse_configuration());
    checks.push(diagnose_namespace_context());
    checks.push(diagnose_lsm_context());
    checks.push(diagnose_recent_fuse_denials());

    MountDiagnosisReport {
        image_path,
        mountpoint,
        checks,
    }
}

pub fn render_mount_diagnosis(report: &MountDiagnosisReport) -> Vec<String> {
    let mut lines = vec![
        format!("diagnosis: {}", report.summary()),
        format!("image: {}", report.image_path.display()),
        format!("mountpoint: {}", report.mountpoint.display()),
    ];

    for check in &report.checks {
        lines.push(format!(
            "[{}] {}: {}",
            check.status.label(),
            check.name,
            check.detail
        ));
    }

    if report.has_failures() {
        lines.push(
            "hint: behebe zuerst die FAIL-Pruefungen, bevor du mount-image erneut startest."
                .to_string(),
        );
    } else if report.has_warnings() {
        lines.push(
            "hint: der Mount ist grundsaetzlich plausibel, aber die WARN-Hinweise koennen spaeter stoeren."
                .to_string(),
        );
    } else {
        lines.push(
            "hint: alle offensichtlichen FUSE-Voraussetzungen sind erfuellt; wenn mount-image trotzdem mit EPERM oder `file descriptor 3 is not a socket` scheitert, spricht das fuer eine Host-Policy, Namespace- oder Helper-Inkompatibilitaet ausserhalb des CoreFS-Images."
                .to_string(),
        );
    }

    lines
}

pub fn ensure_mount_ready(report: &MountDiagnosisReport) -> CoreFsResult<()> {
    if report.has_failures() {
        Err(CoreFsError::State(
            "mount diagnosis failed; run `corefs diagnose-mount <image> <mountpoint>` for details"
                .to_string(),
        ))
    } else {
        Ok(())
    }
}

fn diagnose_platform() -> DiagnosticCheck {
    if cfg!(target_os = "linux") {
        DiagnosticCheck {
            name: "platform".to_string(),
            status: DiagnosticStatus::Pass,
            detail: "Linux erkannt, FUSE-Adapter kann prinzipiell verwendet werden".to_string(),
        }
    } else {
        DiagnosticCheck {
            name: "platform".to_string(),
            status: DiagnosticStatus::Fail,
            detail: "der aktuelle Build laeuft nicht auf Linux; dieser Adapter ist nur fuer Linux-FUSE gedacht".to_string(),
        }
    }
}

fn diagnose_image_path(path: &Path, options: &LinuxMountOptions) -> DiagnosticCheck {
    if path.exists() {
        if !path.is_file() {
            return DiagnosticCheck {
                name: "image".to_string(),
                status: DiagnosticStatus::Fail,
                detail: format!(
                    "{} existiert, ist aber keine regulaere Datei",
                    path.display()
                ),
            };
        }

        return match IntegrityService.fsck_image(path) {
            Ok(report) => image_pass_check(path, report),
            Err(error) => DiagnosticCheck {
                name: "image".to_string(),
                status: DiagnosticStatus::Fail,
                detail: format!(
                    "{} ist nicht mit dem aktuellen CoreFS-Format lesbar: {}",
                    path.display(),
                    error
                ),
            },
        };
    }

    if !options.create_if_missing {
        return DiagnosticCheck {
            name: "image".to_string(),
            status: DiagnosticStatus::Fail,
            detail: format!(
                "{} fehlt und `--create` wurde nicht gesetzt",
                path.display()
            ),
        };
    }

    match writable_parent_for(path) {
        Ok(parent) => match probe_directory_writable(&parent) {
            Ok(()) => DiagnosticCheck {
                name: "image".to_string(),
                status: DiagnosticStatus::Warn,
                detail: format!(
                    "{} existiert noch nicht, kann aber in {} angelegt werden",
                    path.display(),
                    parent.display()
                ),
            },
            Err(error) => DiagnosticCheck {
                name: "image".to_string(),
                status: DiagnosticStatus::Fail,
                detail: format!(
                    "{} fehlt und der Zielordner {} ist nicht beschreibbar: {}",
                    path.display(),
                    parent.display(),
                    error
                ),
            },
        },
        Err(error) => DiagnosticCheck {
            name: "image".to_string(),
            status: DiagnosticStatus::Fail,
            detail: error.to_string(),
        },
    }
}

fn image_pass_check(path: &Path, report: ImageIntegrityReport) -> DiagnosticCheck {
    DiagnosticCheck {
        name: "image".to_string(),
        status: DiagnosticStatus::Pass,
        detail: format!(
            "{} ist konsistent (format={}, segmente={}, generation={}, superblocks={})",
            path.display(),
            report.format_version,
            report.segment_count,
            report.selected_generation,
            report.valid_superblocks
        ),
    }
}

fn diagnose_mountpoint(path: &Path) -> DiagnosticCheck {
    if path.exists() {
        if !path.is_dir() {
            return DiagnosticCheck {
                name: "mountpoint".to_string(),
                status: DiagnosticStatus::Fail,
                detail: format!("{} existiert, ist aber kein Verzeichnis", path.display()),
            };
        }

        let entry_count = fs::read_dir(path)
            .ok()
            .map(|entries| entries.filter_map(Result::ok).count())
            .unwrap_or(0);

        if entry_count == 0 {
            DiagnosticCheck {
                name: "mountpoint".to_string(),
                status: DiagnosticStatus::Pass,
                detail: format!("{} ist vorhanden und leer", path.display()),
            }
        } else {
            DiagnosticCheck {
                name: "mountpoint".to_string(),
                status: DiagnosticStatus::Warn,
                detail: format!(
                    "{} ist vorhanden, aber nicht leer ({} Eintraege)",
                    path.display(),
                    entry_count
                ),
            }
        }
    } else {
        match writable_parent_for(path) {
            Ok(parent) => match probe_directory_writable(&parent) {
                Ok(()) => DiagnosticCheck {
                    name: "mountpoint".to_string(),
                    status: DiagnosticStatus::Warn,
                    detail: format!(
                        "{} fehlt noch, der Parent {} ist aber beschreibbar",
                        path.display(),
                        parent.display()
                    ),
                },
                Err(error) => DiagnosticCheck {
                    name: "mountpoint".to_string(),
                    status: DiagnosticStatus::Fail,
                    detail: format!(
                        "{} fehlt und der Parent {} ist nicht beschreibbar: {}",
                        path.display(),
                        parent.display(),
                        error
                    ),
                },
            },
            Err(error) => DiagnosticCheck {
                name: "mountpoint".to_string(),
                status: DiagnosticStatus::Fail,
                detail: error.to_string(),
            },
        }
    }
}

fn diagnose_fuse_kernel_support() -> DiagnosticCheck {
    let filesystems = Path::new("/proc/filesystems");
    if !filesystems.exists() {
        return DiagnosticCheck {
            name: "kernel-fuse".to_string(),
            status: DiagnosticStatus::Warn,
            detail: "/proc/filesystems ist nicht verfuegbar; Kernel-Support konnte nicht sicher geprueft werden".to_string(),
        };
    }

    match fs::read_to_string(filesystems) {
        Ok(content) => {
            if content.lines().any(|line| line.contains("fuse")) {
                DiagnosticCheck {
                    name: "kernel-fuse".to_string(),
                    status: DiagnosticStatus::Pass,
                    detail: "Kernel meldet FUSE-Unterstuetzung ueber /proc/filesystems".to_string(),
                }
            } else {
                DiagnosticCheck {
                    name: "kernel-fuse".to_string(),
                    status: DiagnosticStatus::Fail,
                    detail: "in /proc/filesystems wurde kein FUSE-Dateisystem gefunden".to_string(),
                }
            }
        }
        Err(error) => DiagnosticCheck {
            name: "kernel-fuse".to_string(),
            status: DiagnosticStatus::Warn,
            detail: format!("/proc/filesystems konnte nicht gelesen werden: {}", error),
        },
    }
}

fn diagnose_fuse_device() -> DiagnosticCheck {
    let fuse_device = Path::new("/dev/fuse");
    if !fuse_device.exists() {
        return DiagnosticCheck {
            name: "fuse-device".to_string(),
            status: DiagnosticStatus::Fail,
            detail: "/dev/fuse fehlt; FUSE-Mounts sind in dieser Umgebung nicht verfuegbar"
                .to_string(),
        };
    }

    match OpenOptions::new().read(true).write(true).open(fuse_device) {
        Ok(_) => DiagnosticCheck {
            name: "fuse-device".to_string(),
            status: DiagnosticStatus::Pass,
            detail: "/dev/fuse ist vorhanden und fuer den aktuellen Prozess oeffenbar".to_string(),
        },
        Err(error) => DiagnosticCheck {
            name: "fuse-device".to_string(),
            status: DiagnosticStatus::Fail,
            detail: format!("/dev/fuse ist vorhanden, aber nicht nutzbar: {}", error),
        },
    }
}

fn diagnose_current_identity() -> DiagnosticCheck {
    let uid = unsafe { libc::geteuid() };
    let gid = unsafe { libc::getegid() };
    let groups = command_output("id", ["-nG"]).unwrap_or_else(|| "unbekannt".to_string());
    DiagnosticCheck {
        name: "identity".to_string(),
        status: DiagnosticStatus::Pass,
        detail: format!("euid={} egid={} gruppen={}", uid, gid, groups.trim()),
    }
}

fn diagnose_userspace_tooling() -> DiagnosticCheck {
    let mut found = Vec::new();
    for tool in ["fuser", "fusermount3", "fusermount"] {
        if let Some(path) = find_in_path(tool) {
            found.push(format!("{tool}={}", path.display()));
        }
    }

    if found.is_empty() {
        return DiagnosticCheck {
            name: "userspace-tooling".to_string(),
            status: DiagnosticStatus::Warn,
            detail: "kein FUSE-Helfer in PATH gefunden; Mounts koennen trotzdem funktionieren, aber Unmount/Debugging wird schwieriger".to_string(),
        };
    }

    let libfuse3_version = command_output("fuser", ["-V"]);
    let detail = match libfuse3_version {
        Some(version) => format!("verfuegbar ({})", version.trim()),
        None => format!("verfuegbar ({})", found.join(", ")),
    };

    DiagnosticCheck {
        name: "userspace-tooling".to_string(),
        status: DiagnosticStatus::Pass,
        detail,
    }
}

fn diagnose_fusermount_permissions() -> DiagnosticCheck {
    let fusermount = Path::new("/usr/bin/fusermount3");
    if !fusermount.exists() {
        return DiagnosticCheck {
            name: "fusermount3".to_string(),
            status: DiagnosticStatus::Warn,
            detail: "/usr/bin/fusermount3 wurde nicht gefunden".to_string(),
        };
    }

    let metadata = match fs::metadata(fusermount) {
        Ok(metadata) => metadata,
        Err(error) => {
            return DiagnosticCheck {
                name: "fusermount3".to_string(),
                status: DiagnosticStatus::Warn,
                detail: format!(
                    "Metadaten fuer /usr/bin/fusermount3 nicht lesbar: {}",
                    error
                ),
            };
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let mode = metadata.mode();
        let is_setuid = mode & 0o4000 != 0;
        let status = if is_setuid {
            DiagnosticStatus::Pass
        } else {
            DiagnosticStatus::Warn
        };
        let detail = if is_setuid {
            format!(
                "fusermount3 ist vorhanden und setuid-root (mode={:o})",
                mode & 0o7777
            )
        } else {
            format!(
                "fusermount3 ist vorhanden, aber ohne setuid-root-Bit (mode={:o})",
                mode & 0o7777
            )
        };
        return DiagnosticCheck {
            name: "fusermount3".to_string(),
            status,
            detail,
        };
    }

    #[allow(unreachable_code)]
    DiagnosticCheck {
        name: "fusermount3".to_string(),
        status: DiagnosticStatus::Pass,
        detail: "fusermount3 ist vorhanden".to_string(),
    }
}

fn diagnose_fuse_configuration() -> DiagnosticCheck {
    let config = Path::new("/etc/fuse.conf");
    if !config.exists() {
        return DiagnosticCheck {
            name: "fuse-conf".to_string(),
            status: DiagnosticStatus::Warn,
            detail: "/etc/fuse.conf fehlt; Standardverhalten wird angenommen".to_string(),
        };
    }

    match fs::read_to_string(config) {
        Ok(content) => {
            let user_allow_other = content
                .lines()
                .map(str::trim)
                .any(|line| !line.starts_with('#') && line == "user_allow_other");
            let detail = if user_allow_other {
                "/etc/fuse.conf erlaubt `user_allow_other`".to_string()
            } else {
                "/etc/fuse.conf setzt kein `user_allow_other`; fuer `allow_other`-Mounts waere das spaeter relevant".to_string()
            };
            DiagnosticCheck {
                name: "fuse-conf".to_string(),
                status: if user_allow_other {
                    DiagnosticStatus::Pass
                } else {
                    DiagnosticStatus::Warn
                },
                detail,
            }
        }
        Err(error) => DiagnosticCheck {
            name: "fuse-conf".to_string(),
            status: DiagnosticStatus::Warn,
            detail: format!("/etc/fuse.conf konnte nicht gelesen werden: {}", error),
        },
    }
}

fn diagnose_namespace_context() -> DiagnosticCheck {
    let uid_map = fs::read_to_string("/proc/self/uid_map").ok();
    let gid_map = fs::read_to_string("/proc/self/gid_map").ok();
    let cgroup = fs::read_to_string("/proc/1/cgroup").ok();
    let in_container =
        Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists();

    let userns_hint = uid_map
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unbekannt");
    let gidns_hint = gid_map
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unbekannt");

    let suspicious_cgroup = cgroup
        .as_deref()
        .map(|value| {
            let lowered = value.to_ascii_lowercase();
            lowered.contains("docker")
                || lowered.contains("podman")
                || lowered.contains("kubepods")
                || lowered.contains("lxc")
                || lowered.contains("container")
        })
        .unwrap_or(false);

    let status = if in_container || suspicious_cgroup {
        DiagnosticStatus::Warn
    } else {
        DiagnosticStatus::Pass
    };

    DiagnosticCheck {
        name: "namespace-context".to_string(),
        status,
        detail: format!(
            "uid_map=`{}` gid_map=`{}` container_hints={}",
            userns_hint,
            gidns_hint,
            if in_container || suspicious_cgroup {
                "ja"
            } else {
                "nein"
            }
        ),
    }
}

fn diagnose_lsm_context() -> DiagnosticCheck {
    let apparmor = fs::read_to_string("/sys/module/apparmor/parameters/enabled")
        .ok()
        .map(|value| value.trim().to_string());
    let selinux = fs::read_to_string("/sys/fs/selinux/enforce")
        .ok()
        .map(|value| value.trim().to_string());

    let mut parts = Vec::new();
    if let Some(value) = apparmor {
        parts.push(format!("apparmor={value}"));
    }
    if let Some(value) = selinux {
        parts.push(format!("selinux={value}"));
    }

    if parts.is_empty() {
        DiagnosticCheck {
            name: "security-context".to_string(),
            status: DiagnosticStatus::Pass,
            detail: "keine offensichtliche AppArmor-/SELinux-Info gefunden".to_string(),
        }
    } else {
        DiagnosticCheck {
            name: "security-context".to_string(),
            status: DiagnosticStatus::Warn,
            detail: format!(
                "{}; bei EPERM koennen Host-Sicherheitsrichtlinien hier hineinspielen",
                parts.join(" ")
            ),
        }
    }
}

fn diagnose_recent_fuse_denials() -> DiagnosticCheck {
    let recent_logs = recent_kernel_log_excerpt().or_else(recent_journal_excerpt);

    let Some(logs) = recent_logs else {
        return DiagnosticCheck {
            name: "recent-denials".to_string(),
            status: DiagnosticStatus::Warn,
            detail: "keine Kernel-/Journal-Logs lesbar; FUSE- oder AppArmor-Denials konnten nicht automatisch geprueft werden".to_string(),
        };
    };

    let mut matches = logs
        .lines()
        .filter(|line| {
            let lowered = line.to_ascii_lowercase();
            lowered.contains("fusermount3")
                || lowered.contains("fuse")
                || lowered.contains("apparmor=\"denied\"")
        })
        .collect::<Vec<_>>();

    if matches.is_empty() {
        return DiagnosticCheck {
            name: "recent-denials".to_string(),
            status: DiagnosticStatus::Pass,
            detail: "keine offensichtlichen juengsten FUSE-/AppArmor-Denials in den verfuegbaren Kernel-Logs gefunden".to_string(),
        };
    }

    matches.truncate(3);
    let excerpt = matches.join(" | ");
    let status = if excerpt.contains("apparmor=\"DENIED\"")
        || excerpt.contains("apparmor=\"denied\"")
        || excerpt.contains("Operation not permitted")
    {
        DiagnosticStatus::Warn
    } else {
        DiagnosticStatus::Pass
    };

    DiagnosticCheck {
        name: "recent-denials".to_string(),
        status,
        detail: excerpt,
    }
}

fn writable_parent_for(path: &Path) -> CoreFsResult<PathBuf> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    if !parent.exists() {
        return Err(CoreFsError::State(format!(
            "der Parent-Ordner {} existiert nicht",
            parent.display()
        )));
    }
    if !parent.is_dir() {
        return Err(CoreFsError::State(format!(
            "der Parent-Pfad {} ist kein Verzeichnis",
            parent.display()
        )));
    }
    Ok(parent.to_path_buf())
}

fn probe_directory_writable(path: &Path) -> CoreFsResult<()> {
    let probe_path = path.join(format!(
        ".corefs-write-probe-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after unix epoch")
            .as_nanos()
    ));

    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe_path)
        .map_err(|error| {
            CoreFsError::State(format!(
                "konnte keine Testdatei in {} anlegen: {}",
                path.display(),
                error
            ))
        })?;

    fs::remove_file(&probe_path).map_err(|error| {
        CoreFsError::State(format!(
            "konnte Testdatei {} nicht entfernen: {}",
            probe_path.display(),
            error
        ))
    })?;

    Ok(())
}

fn find_in_path(tool: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for entry in std::env::split_paths(&path_var) {
        let candidate = entry.join(tool);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn command_output<const N: usize>(program: &str, args: [&str; N]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !stdout.is_empty() {
        return Some(stdout);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        None
    } else {
        Some(stderr)
    }
}

fn recent_kernel_log_excerpt() -> Option<String> {
    command_output("dmesg", ["--color=never"])
}

fn recent_journal_excerpt() -> Option<String> {
    command_output("journalctl", ["-k", "-n", "200", "--no-pager"])
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
