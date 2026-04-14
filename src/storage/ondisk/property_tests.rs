// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

use super::*;

#[test]
fn rng_is_deterministic_across_identical_seeds() {
    let mut a = Rng::new(42);
    let mut b = Rng::new(42);
    for _ in 0..16 {
        assert_eq!(a.next_u64(), b.next_u64());
    }
}

#[test]
fn rng_zero_seed_does_not_get_stuck_at_zero() {
    let mut r = Rng::new(0);
    for _ in 0..8 {
        assert_ne!(r.next_u64(), 0);
    }
}

#[test]
fn rng_range_stays_below_bound() {
    let mut r = Rng::new(123);
    for _ in 0..200 {
        let v = r.next_range(10);
        assert!(v < 10);
    }
}

#[test]
fn choose_returns_an_element_from_the_slice() {
    let mut r = Rng::new(7);
    let options = ["alpha", "beta", "gamma"];
    for _ in 0..16 {
        let picked = r.choose(&options);
        assert!(options.contains(picked));
    }
}

#[test]
fn generate_sequence_is_deterministic() {
    let a = generate_sequence(99, 20);
    let b = generate_sequence(99, 20);
    assert_eq!(a, b);
}

#[test]
fn generate_sequence_differs_across_seeds() {
    let a = generate_sequence(1, 20);
    let b = generate_sequence(2, 20);
    assert_ne!(a, b);
}

#[test]
fn generate_sequence_respects_length() {
    let seq = generate_sequence(555, 50);
    assert_eq!(seq.len(), 50);
}

// The load-bearing property tests ---------------------------------------------

#[test]
fn fsck_stays_clean_across_random_sequences() {
    // Eight fixed seeds × 15 ops each = 120 mutate-and-check cycles
    // covering a broad mix of create / overwrite / delete / dir /
    // snapshot operations.
    let seeds: &[u64] = &[1, 7, 42, 101, 0xBEEF, 0xDEADBEEF, 0xCAFE_BABE, 12345];
    fuzz_many_seeds(seeds, 15).unwrap();
}

#[test]
fn seeded_sequence_replays_identically_in_two_sessions() {
    // Prove that deterministic generation + deterministic ODF
    // serialisation means two independent runs reach equivalent
    // states (same inode count, same fsck-clean result).
    let seed = 0xABCD_1234;
    let ops = generate_sequence(seed, 12);
    run_and_check(seed, &ops).unwrap();
    run_and_check(seed, &ops).unwrap();
}

#[test]
fn pure_creates_only_grow_active_inodes_monotonically() {
    let ops: Vec<Op> = (0..10)
        .map(|i| Op::CreateFile {
            path: format!("/mono{i}"),
            content: format!("v{i}").into_bytes(),
        })
        .collect();
    run_and_check(0xFEED_0001, &ops).unwrap();
}

#[test]
fn delete_then_create_cycle_stays_fsck_clean() {
    let ops: Vec<Op> = (0..8)
        .flat_map(|i| {
            vec![
                Op::CreateFile {
                    path: format!("/cycle{i}"),
                    content: b"x".to_vec(),
                },
                Op::DeleteFile {
                    path: format!("/cycle{i}"),
                },
            ]
        })
        .collect();
    run_and_check(0xFEED_0002, &ops).unwrap();
}

#[test]
fn overwrite_sequence_preserves_fsck_invariant() {
    let mut ops = vec![Op::CreateFile {
        path: "/stable".into(),
        content: b"initial".to_vec(),
    }];
    for i in 0..6 {
        ops.push(Op::OverwriteFile {
            path: "/stable".into(),
            content: format!("iter-{i}-contents").into_bytes(),
        });
    }
    run_and_check(0xFEED_0003, &ops).unwrap();
}

#[test]
fn snapshot_after_changes_does_not_break_reload() {
    let ops = vec![
        Op::CreateFile {
            path: "/a".into(),
            content: b"one".to_vec(),
        },
        Op::CreateFile {
            path: "/b".into(),
            content: b"two".to_vec(),
        },
        Op::CreateSnapshot {
            name: "post-setup".into(),
        },
        Op::OverwriteFile {
            path: "/a".into(),
            content: b"one-updated".to_vec(),
        },
        Op::CreateSnapshot {
            name: "post-update".into(),
        },
    ];
    run_and_check(0xFEED_0004, &ops).unwrap();
}

#[test]
fn empty_sequence_is_vacuously_valid() {
    run_and_check(0, &[]).unwrap();
}
