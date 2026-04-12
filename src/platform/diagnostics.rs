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
    checks.push(diagnose_userspace_tooling());

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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "corefs-diagnostics-{name}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system time should be after unix epoch")
                .as_nanos()
        ));
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }

    #[test]
    fn report_summary_reflects_failures_and_warnings() {
        let mut report = MountDiagnosisReport {
            image_path: PathBuf::from("a.img"),
            mountpoint: PathBuf::from("/mnt/corefs"),
            checks: vec![DiagnosticCheck {
                name: "image".to_string(),
                status: DiagnosticStatus::Pass,
                detail: "ok".to_string(),
            }],
        };

        assert_eq!(report.summary(), "mount-ready");

        report.checks.push(DiagnosticCheck {
            name: "mountpoint".to_string(),
            status: DiagnosticStatus::Warn,
            detail: "warn".to_string(),
        });
        assert_eq!(report.summary(), "mount-ready-with-warnings");

        report.checks.push(DiagnosticCheck {
            name: "fuse-device".to_string(),
            status: DiagnosticStatus::Fail,
            detail: "fail".to_string(),
        });
        assert_eq!(report.summary(), "mount-not-ready");
    }

    #[test]
    fn writable_probe_accepts_real_directory() {
        let path = temp_dir("writable");
        probe_directory_writable(&path).expect("directory should be writable");
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn missing_image_without_create_is_failure() {
        let options = LinuxMountOptions::default();
        let check = diagnose_image_path(Path::new("/definitely/not/there.img"), &options);
        assert_eq!(check.status, DiagnosticStatus::Fail);
    }
}
