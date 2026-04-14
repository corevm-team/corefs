// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

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
