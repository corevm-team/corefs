use corefs_tools::fsck;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub struct CertTemp {
    root: PathBuf,
}

impl CertTemp {
    pub fn new(case: &str) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "corefs-cert-{case}-{}-{now}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&root).expect("create certification temp dir");
        Self { root }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for CertTemp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub fn deterministic_bytes(seed: u64, len: usize) -> Vec<u8> {
    let mut x = if seed == 0 {
        0xC0DE_5EED_D15C_A11Au64
    } else {
        seed
    };
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        out.push((x as u8).wrapping_add((i & 0xff) as u8));
    }
    out
}

pub fn assert_clean_image(path: &Path) {
    let report = fsck::check_image(path).expect("fsck image");
    assert!(
        report.is_clean,
        "fsck reported non-clean image: {:?}",
        report.issues
    );
}

pub fn maybe_write_evidence(case: &str, body: &str) {
    let Some(dir) = std::env::var_os("COREFS_CERT_EVIDENCE_DIR") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let dir = if dir.is_relative() {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("certification crate should live below workspace root")
            .join(dir)
    } else {
        dir
    };
    fs::create_dir_all(&dir).expect("create evidence dir");
    fs::write(dir.join(format!("{case}.txt")), body).expect("write evidence");
}
