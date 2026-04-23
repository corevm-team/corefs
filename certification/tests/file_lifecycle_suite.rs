mod common;

use common::{CertTemp, assert_clean_image, maybe_write_evidence};
use corefs::config::CoreFsConfig;
use corefs::domain::inode::InodeKind;
use corefs::storage::ondisk::session::{OdfFileSession, OdfSessionOptions};

const FILE_CAPACITY: u64 = 64 * 1024 * 1024;

#[test]
fn cert_120_file_creation_exists_duplicate_rejected_and_reopen() {
    let tmp = CertTemp::new("file-create");
    let image = tmp.path("create.img");
    let mut session = format_session(&image);

    session
        .mutate(|fs| {
            assert!(!fs.list_paths().contains(&"/created.txt".to_string()));
            fs.create_file("/created.txt", b"created payload", &[String::from("file")])?;
            let inode = fs.get_inode("/created.txt").expect("created inode");
            assert_eq!(inode.kind, InodeKind::File);
            assert_eq!(inode.size, "created payload".len());
            assert_eq!(fs.read_file("/created.txt")?, b"created payload");
            assert!(
                fs.create_file("/created.txt", b"duplicate", &[]).is_err(),
                "duplicate file creation must be rejected"
            );
            Ok(())
        })
        .expect("create file lifecycle");
    drop(session);

    let session = OdfFileSession::open(&image).expect("reopen created file");
    assert_eq!(
        session.service().read_file("/created.txt").unwrap(),
        b"created payload"
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_120_file_creation_exists",
        "created_exists=true\nduplicate_rejected=true\nreopen_visible=true\nfsck_clean=true\n",
    );
}

#[test]
fn cert_121_file_deletion_deleted_but_recoverable_then_restored() {
    let tmp = CertTemp::new("file-soft-delete");
    let image = tmp.path("soft-delete.img");
    let mut session = format_session(&image);

    session
        .mutate(|fs| {
            fs.create_file("/recover-me.txt", b"recoverable", &[])?;
            fs.delete_file("/recover-me.txt", false)?;
            assert!(fs.read_file("/recover-me.txt").is_err());
            assert!(
                fs.recoverable_paths()
                    .contains(&"/recover-me.txt".to_string()),
                "soft-deleted file must be listed as recoverable"
            );
            fs.restore_file("/recover-me.txt")?;
            assert_eq!(fs.read_file("/recover-me.txt")?, b"recoverable");
            Ok(())
        })
        .expect("soft delete lifecycle");
    drop(session);

    let session = OdfFileSession::open(&image).expect("reopen restored file");
    assert_eq!(
        session.service().read_file("/recover-me.txt").unwrap(),
        b"recoverable"
    );
    assert!(
        !session
            .service()
            .recoverable_paths()
            .contains(&"/recover-me.txt".to_string())
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_121_file_delete_recover_restore",
        "deleted_in_active_catalog=true\nrecoverable_after_delete=true\nrestored=true\nreopen_visible=true\nfsck_clean=true\n",
    );
}

#[test]
fn cert_122_secure_delete_and_expunge_are_irrecoverable() {
    let tmp = CertTemp::new("file-irrecoverable");
    let image = tmp.path("irrecoverable.img");
    let mut session = format_session(&image);

    session
        .mutate(|fs| {
            fs.create_file("/secure.bin", b"secret", &[])?;
            fs.delete_file("/secure.bin", true)?;
            assert!(fs.read_file("/secure.bin").is_err());
            assert!(fs.restore_file("/secure.bin").is_err());
            assert!(!fs.recoverable_paths().contains(&"/secure.bin".to_string()));

            fs.create_file("/expunge.bin", b"temporary", &[])?;
            fs.delete_file("/expunge.bin", false)?;
            assert!(fs.recoverable_paths().contains(&"/expunge.bin".to_string()));
            fs.expunge_file("/expunge.bin")?;
            assert!(fs.restore_file("/expunge.bin").is_err());
            assert!(!fs.recoverable_paths().contains(&"/expunge.bin".to_string()));
            Ok(())
        })
        .expect("irrecoverable lifecycle");
    drop(session);

    let session = OdfFileSession::open(&image).expect("reopen irrecoverable image");
    assert!(session.service().read_file("/secure.bin").is_err());
    assert!(session.service().read_file("/expunge.bin").is_err());
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_122_secure_delete_expunge",
        "secure_delete_irrecoverable=true\nexpunge_irrecoverable=true\nreopen_absent=true\nfsck_clean=true\n",
    );
}

#[test]
fn cert_123_file_overwrite_range_append_truncate_exact_semantics() {
    let tmp = CertTemp::new("file-content-semantics");
    let image = tmp.path("content.img");
    let mut session = format_session(&image);

    session
        .mutate(|fs| {
            fs.create_file("/content.bin", b"0123456789", &[])?;
            fs.write_file("/content.bin", b"abcdef")?;
            assert_eq!(fs.read_file("/content.bin")?, b"abcdef");
            fs.write_file_range("/content.bin", 2, b"ZZ")?;
            assert_eq!(fs.read_file("/content.bin")?, b"abZZef");
            fs.extend_file("/content.bin", b"-tail")?;
            assert_eq!(fs.read_file("/content.bin")?, b"abZZef-tail");
            fs.truncate_file("/content.bin", 4)?;
            assert_eq!(fs.read_file("/content.bin")?, b"abZZ");
            fs.truncate_file("/content.bin", 8)?;
            assert_eq!(fs.read_file("/content.bin")?, b"abZZ\0\0\0\0");
            assert_eq!(fs.read_file_range("/content.bin", 2, 4)?, b"ZZ\0\0");
            Ok(())
        })
        .expect("content semantics");
    drop(session);

    let session = OdfFileSession::open(&image).expect("reopen content semantics");
    assert_eq!(
        session.service().read_file("/content.bin").unwrap(),
        b"abZZ\0\0\0\0"
    );
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_123_file_content_semantics",
        "overwrite_replaces=true\nrange_patch_exact=true\nappend_exact=true\ntruncate_shrink=true\ntruncate_grow_zero_filled=true\nreopen_visible=true\nfsck_clean=true\n",
    );
}

#[test]
fn cert_124_fsync_forced_flush_persistence_boundary() {
    let tmp = CertTemp::new("fsync-boundary");
    let image = tmp.path("fsync.img");
    let mut session = format_session(&image);

    session
        .mutate(|fs| {
            fs.create_file("/baseline.txt", b"baseline", &[])?;
            Ok(())
        })
        .expect("baseline commit");

    session
        .service_mut()
        .create_file("/not-flushed.txt", b"volatile", &[])
        .expect("create volatile in memory");
    drop(session);

    let mut session = OdfFileSession::open(&image).expect("reopen before forced flush");
    assert_eq!(
        session.service().read_file("/baseline.txt").unwrap(),
        b"baseline"
    );
    assert!(
        session.service().read_file("/not-flushed.txt").is_err(),
        "unflushed service mutation must not appear after reopen"
    );

    session
        .service_mut()
        .create_file("/forced.txt", b"fsync-visible", &[])
        .expect("create forced file");
    let report = session.flush().expect("explicit forced flush");
    assert!(report.incremental.updated >= 1 || report.incremental.created >= 1);
    drop(session);

    let session = OdfFileSession::open(&image).expect("reopen after forced flush");
    assert_eq!(
        session.service().read_file("/forced.txt").unwrap(),
        b"fsync-visible"
    );
    assert!(session.service().read_file("/not-flushed.txt").is_err());
    assert_clean_image(&image);

    maybe_write_evidence(
        "cert_124_fsync_forced_flush",
        &format!(
            "unflushed_absent_after_reopen=true\nforced_flush_visible_after_reopen=true\nincremental_created={}\nincremental_updated={}\nfsck_clean=true\n",
            report.incremental.created, report.incremental.updated
        ),
    );
}

fn format_session(path: &std::path::Path) -> OdfFileSession {
    let mut opts = OdfSessionOptions::with_defaults();
    opts.capacity_bytes = FILE_CAPACITY;
    opts.config = CoreFsConfig::performance_profile();
    OdfFileSession::format_new(path, &opts).expect("format file lifecycle image")
}
