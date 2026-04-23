mod common;

use common::maybe_write_evidence;
use corefs_core::storage::ondisk::xattr::{AclPrincipal, AclRecord, XattrBlock, XattrPair, perm};

#[test]
fn cert_110_xattr_acl_block_crc_roundtrip_and_corruption_rejection() {
    let block = XattrBlock {
        flags: 0xA5A5_0001,
        xattrs: vec![
            XattrPair {
                name: "user.mime_type".to_string(),
                value: b"application/corefs-cert".to_vec(),
            },
            XattrPair {
                name: "security.hash".to_string(),
                value: vec![0x11, 0x22, 0x33, 0x44],
            },
        ],
        acls: vec![
            AclRecord {
                principal: AclPrincipal::User,
                subject: "1000".to_string(),
                permission: perm::READ | perm::WRITE,
            },
            AclRecord {
                principal: AclPrincipal::Everyone,
                subject: String::new(),
                permission: perm::READ,
            },
        ],
    };

    let encoded = block.encode().expect("encode xattr block");
    assert_eq!(encoded.len(), 4096);
    let decoded = XattrBlock::decode(&encoded).expect("decode xattr block");
    assert_eq!(decoded, block);

    let mut corrupted = encoded.clone();
    corrupted[32] ^= 0x5A;
    assert!(
        XattrBlock::decode(&corrupted).is_err(),
        "xattr CRC must reject payload corruption"
    );

    let mut bad_magic = encoded.clone();
    bad_magic[0] = 0;
    let magic_result = XattrBlock::decode(&bad_magic);
    assert!(
        magic_result.is_err(),
        "xattr CRC/magic validation must reject header corruption"
    );

    maybe_write_evidence(
        "cert_110_xattr_acl_block",
        &format!(
            "encoded_bytes={}\nxattrs={}\nacls={}\ncorruption_rejected=true\nbad_magic_rejected=true\n",
            encoded.len(),
            decoded.xattrs.len(),
            decoded.acls.len()
        ),
    );
}
