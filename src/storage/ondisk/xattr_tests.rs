// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

fn sample() -> XattrBlock {
    XattrBlock {
        flags: 0x0000_0042,
        xattrs: vec![
            XattrPair {
                name: "user.colour".into(),
                value: b"#C0FFEE".to_vec(),
            },
            XattrPair {
                name: "security.capability".into(),
                value: vec![0x01, 0x02, 0x03, 0x04],
            },
        ],
        acls: vec![
            AclRecord {
                principal: AclPrincipal::User,
                subject: "alice".into(),
                permission: perm::READ | perm::WRITE,
            },
            AclRecord {
                principal: AclPrincipal::Group,
                subject: "staff".into(),
                permission: perm::READ,
            },
            AclRecord {
                principal: AclPrincipal::Everyone,
                subject: "".into(),
                permission: perm::READ | perm::EXECUTE,
            },
        ],
    }
}

#[test]
fn empty_roundtrip() {
    let blk = XattrBlock::default();
    let enc = blk.encode().unwrap();
    assert_eq!(XattrBlock::decode(&enc).unwrap(), blk);
}

#[test]
fn populated_roundtrip() {
    let blk = sample();
    let enc = blk.encode().unwrap();
    let dec = XattrBlock::decode(&enc).unwrap();
    assert_eq!(dec, blk);
    assert_eq!(dec.xattrs.len(), 2);
    assert_eq!(dec.acls.len(), 3);
}

#[test]
fn checksum_detects_corruption() {
    let mut enc = sample().encode().unwrap();
    enc[30] ^= 0xC0;
    assert!(XattrBlock::decode(&enc).is_err());
}

#[test]
fn bad_magic_rejected() {
    let mut enc = sample().encode().unwrap();
    enc[0..4].copy_from_slice(&0u32.to_le_bytes());
    let mut zeroed = enc.clone();
    zeroed[4092..4096].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(&zeroed);
    enc[4092..4096].copy_from_slice(&csum.to_le_bytes());
    let err = XattrBlock::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("magic"));
}

#[test]
fn unknown_principal_rejected() {
    let mut enc = XattrBlock {
        flags: 0,
        xattrs: vec![],
        acls: vec![AclRecord {
            principal: AclPrincipal::User,
            subject: "x".into(),
            permission: 0,
        }],
    }
    .encode()
    .unwrap();
    // ACL principal byte sits at HEADER(16) + rec-start + 2; rec is 8-byte aligned.
    // Header=16, first acl entry starts at 16.  Byte 16+2=18 holds principal.
    enc[18] = 99;
    // Recompute CRC.
    let mut zeroed = enc.clone();
    zeroed[4092..4096].fill(0);
    let csum = crate::storage::ondisk::checksum::Crc32c::hash(&zeroed);
    enc[4092..4096].copy_from_slice(&csum.to_le_bytes());
    let err = XattrBlock::decode(&enc).unwrap_err();
    assert!(format!("{err}").contains("principal"));
}

#[test]
fn many_entries_fill_block() {
    let mut blk = XattrBlock::default();
    for i in 0..100 {
        blk.xattrs.push(XattrPair {
            name: format!("k{i}"),
            value: vec![0xAA; 10],
        });
    }
    let enc = blk.encode().unwrap();
    let dec = XattrBlock::decode(&enc).unwrap();
    assert_eq!(dec.xattrs.len(), 100);
}

#[test]
fn overflow_is_rejected() {
    let mut blk = XattrBlock::default();
    for i in 0..1000 {
        blk.xattrs.push(XattrPair {
            name: format!("k{i}_long_name"),
            value: vec![0u8; 50],
        });
    }
    assert!(blk.encode().is_err());
}
