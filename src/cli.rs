use crate::app::CoreFsService;
use crate::config::CoreFsConfig;
use crate::error::{CoreFsError, CoreFsResult};

pub fn run<I>(args: I) -> CoreFsResult<()>
where
    I: IntoIterator<Item = String>,
{
    let args: Vec<String> = args.into_iter().collect();
    if args.len() <= 1 {
        print_usage();
        return Ok(());
    }

    let mut fs = bootstrap_demo_fs()?;

    match args[1].as_str() {
        "mkfs" => {
            println!("formatted volume {}", fs.volume_name());
        }
        "status" => {
            let report = fs.admin_report();
            println!("volume: {}", report.volume.name);
            println!("block_size: {}", report.volume.block_size);
            println!("features: {}", report.volume.feature_flags.join(", "));
            println!("files: {}", report.stats.files);
            println!("snapshots: {}", report.stats.snapshots);
            println!("journal_entries: {}", report.stats.journal_entries);
        }
        "ls" => {
            for path in fs.list_paths() {
                println!("{path}");
            }
        }
        "snapshot" => {
            let name = args.get(2).cloned().unwrap_or_else(|| "manual".to_string());
            let snapshot = fs.create_snapshot(&name);
            println!(
                "snapshot {} created with {} paths",
                snapshot.name,
                snapshot.paths.len()
            );
        }
        "scrub" => {
            let report = fs.scrub();
            println!(
                "scrub checked={} valid={} invalid={}",
                report.checked_paths, report.valid_blocks, report.invalid_blocks
            );
        }
        "delete" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for delete".to_string())
            })?;
            let secure = args.iter().any(|arg| arg == "--secure");
            fs.delete_file(path, secure)?;
            println!("deleted {path}");
        }
        "restore" => {
            let path = args.get(2).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing path for restore".to_string())
            })?;
            fs.restore_file(path)?;
            println!("restored {path}");
        }
        "write" => {
            let path = args
                .get(2)
                .ok_or_else(|| CoreFsError::InvalidCommand("missing path for write".to_string()))?;
            let payload = args.get(3).ok_or_else(|| {
                CoreFsError::InvalidCommand("missing payload for write".to_string())
            })?;
            fs.write_file(path, payload.as_bytes())?;
            println!("written {path}");
        }
        "read" => {
            let path = args
                .get(2)
                .ok_or_else(|| CoreFsError::InvalidCommand("missing path for read".to_string()))?;
            let bytes = fs.read_file(path)?;
            println!("{}", String::from_utf8_lossy(&bytes));
        }
        command => {
            return Err(CoreFsError::InvalidCommand(format!(
                "unknown command: {command}"
            )));
        }
    }

    Ok(())
}

fn bootstrap_demo_fs() -> CoreFsResult<CoreFsService> {
    let mut fs = CoreFsService::format(CoreFsConfig::default());
    fs.create_directory("/etc")?;
    fs.create_directory("/var")?;
    fs.create_file(
        "/etc/corefs.conf",
        b"volume=corefs\ncompression=on\nencryption=on\n",
        &["config".to_string(), "system".to_string()],
    )?;
    fs.create_file(
        "/var/readme.txt",
        b"CoreFS enterprise bootstrap",
        &["docs".to_string()],
    )?;
    fs.create_symlink("/etc/corefs-current", "/etc/corefs.conf")?;
    Ok(fs)
}

fn print_usage() {
    println!("corefs commands:");
    println!("  mkfs");
    println!("  status");
    println!("  ls");
    println!("  snapshot [name]");
    println!("  scrub");
    println!("  delete <path> [--secure]");
    println!("  restore <path>");
    println!("  write <path> <payload>");
    println!("  read <path>");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_without_command_returns_ok() {
        let result = run(vec!["corefs".to_string()]);
        assert!(result.is_ok());
    }

    #[test]
    fn cli_supports_successful_commands() {
        let successful = [
            vec!["corefs".to_string(), "mkfs".to_string()],
            vec!["corefs".to_string(), "status".to_string()],
            vec!["corefs".to_string(), "ls".to_string()],
            vec![
                "corefs".to_string(),
                "snapshot".to_string(),
                "nightly".to_string(),
            ],
            vec!["corefs".to_string(), "scrub".to_string()],
            vec![
                "corefs".to_string(),
                "write".to_string(),
                "/etc/corefs.conf".to_string(),
                "updated".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "read".to_string(),
                "/etc/corefs.conf".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "delete".to_string(),
                "/var/readme.txt".to_string(),
            ],
            vec![
                "corefs".to_string(),
                "delete".to_string(),
                "/var/readme.txt".to_string(),
                "--secure".to_string(),
            ],
        ];

        for args in successful {
            assert!(run(args).is_ok());
        }
    }

    #[test]
    fn cli_returns_errors_for_invalid_commands_and_missing_arguments() {
        let invalid = run(vec!["corefs".to_string(), "nope".to_string()]);
        assert!(matches!(invalid, Err(CoreFsError::InvalidCommand(_))));

        let delete = run(vec!["corefs".to_string(), "delete".to_string()]);
        assert!(matches!(delete, Err(CoreFsError::InvalidCommand(_))));

        let restore = run(vec!["corefs".to_string(), "restore".to_string()]);
        assert!(matches!(restore, Err(CoreFsError::InvalidCommand(_))));

        let write_path = run(vec!["corefs".to_string(), "write".to_string()]);
        assert!(matches!(write_path, Err(CoreFsError::InvalidCommand(_))));

        let write_payload = run(vec![
            "corefs".to_string(),
            "write".to_string(),
            "/etc/corefs.conf".to_string(),
        ]);
        assert!(matches!(write_payload, Err(CoreFsError::InvalidCommand(_))));

        let read = run(vec!["corefs".to_string(), "read".to_string()]);
        assert!(matches!(read, Err(CoreFsError::InvalidCommand(_))));
    }

    #[test]
    fn bootstrap_demo_fs_creates_expected_layout() {
        let fs = bootstrap_demo_fs().expect("bootstrap should succeed");
        let paths = fs.list_paths();

        assert!(paths.iter().any(|path| path == "/etc"));
        assert!(paths.iter().any(|path| path == "/var"));
        assert!(paths.iter().any(|path| path == "/etc/corefs.conf"));
        assert!(paths.iter().any(|path| path == "/var/readme.txt"));
        assert!(paths.iter().any(|path| path == "/etc/corefs-current"));
    }
}
