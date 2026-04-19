# Crypto & Hot-Path Profile (2026-04-19)

Run via `cargo run --release --bin corefs-crypto-bench -- <MiB>`
on pseudo-random payloads so LZ4 cannot shortcut the pipeline.

## Results

Times in milliseconds (write — first allocation, rewrite — reuses
existing extent, read).  Each row is a fresh `CoreFsService::format`
with versioning disabled so the number reflects the raw
`write_file → BlockStore::write` path.

| Size  | Profile       | write_ms | rewrite_ms | read_ms | per-MiB write |
|-------|---------------|---------:|-----------:|--------:|--------------:|
| 4 MiB | raw           |      22  |         ~ |      7  |  5.5 ms/MiB   |
| 4 MiB | encrypt-only  |      24  |         ~ |     11  |  6.0 ms/MiB   |
| 4 MiB | no-encrypt    |      27  |         ~ |     20  |  6.8 ms/MiB   |
| 4 MiB | default       |      46  |         ~ |     18  |  11.5 ms/MiB  |
| 16 MiB | raw          |      93  |        ~ |     52  |  5.8 ms/MiB   |
| 16 MiB | encrypt-only |     126  |        ~ |     68  |  7.9 ms/MiB   |
| 16 MiB | no-encrypt   |     104  |        ~ |     67  |  6.5 ms/MiB   |
| 16 MiB | default      |     147  |        ~ |     96  |  9.2 ms/MiB   |
| 32 MiB | raw          |     209  |      187 |    128  |  6.5 ms/MiB   |
| 32 MiB | encrypt-only |     296  |      274 |    163  |  9.2 ms/MiB   |
| 32 MiB | no-encrypt   |     225  |      203 |    162  |  7.0 ms/MiB   |
| 32 MiB | default      |     312  |      287 |    197  |  9.7 ms/MiB   |

## Cost breakdown per 32 MiB write

| Component                          | ms | Throughput  | Notes |
|------------------------------------|---:|-------------|-------|
| Baseline raw write                 | 209 | 155 MiB/s | catalog + allocate + `BlockStore::write` |
| ChaCha20-Poly1305 encryption        | +87 | ≈370 MiB/s | default encryption_at_rest |
| LZ4 compress (incompressible input) | +16 |    2 GiB/s | fast short-circuit on random data |
| LZ4 compress (zeros)                | –  | ~16 GiB/s | 16 MiB → ~64 KiB stored |

Rewrite vs first-write is only ~10 % cheaper, so extent allocation
is **not** the bottleneck — the inherent cost is the byte pipeline.

## Where the raw path spends 209 ms / 32 MiB

Plausible breakdown based on code inspection:

1. `bytes.to_vec()` in `CoreFsService::write_file` before the
   compress/encrypt fork — one full-payload clone.
2. `Crc32c::hash(&bytes)` in `BlockStore::write` — software CRC32C
   runs at ≈ 1 GiB/s, so ~30 ms on 32 MiB.
3. `let mut buf = vec![0u8; padded_len]; buf[..size].copy_from_slice(&bytes[..size])`
   in `BlockStore::write` — second full-payload copy into a padded
   block-aligned buffer.
4. `compat_device.write_at(byte_offset, &buf)` — `MemoryDevice`
   then does a third memcpy into its internal backing `Vec`.
5. Dedup insert / release bookkeeping — negligible for large files.

So a single 32 MiB user-visible `write_file` triggers **three full
copies of the 32 MiB payload** (clone, alloc-aligned buf, device
write) plus one **software CRC32C pass** over the same bytes.  That
is ≈ 120 MiB of RAM touched per MiB written — matching the measured
155 MiB/s throughput on a host that easily memcpys at multi-GiB/s.

Encryption adds another full pass + random-nonce + Poly1305 tag, at
the measured 370 MiB/s ChaCha20-Poly1305 rate.

## Implications for the seq_write gap

`seq_write 128 MiB` in the FUSE benchmark runs at 60–68 MiB/s,
about 10× below ext4.  Before wiring up crypto-related
optimisations, the bigger leverage is in the `BlockStore::write`
copy-count: collapsing the three passes (clone → aligned buf →
device) to one or two would give a noticeable win on every write
path, crypto-on or crypto-off.

Hardware-accelerated CRC32C (`_mm_crc32_u64` on x86_64 via the
`crc32c` crate) would remove another ~30 ms per 32 MiB.

## Suggested next steps (ranked)

1. **Collapse the double copy in `BlockStore::write`.**  Accept the
   already-aligned byte buffer from the caller, skip the intermediate
   `vec![0; padded_len]`, write it directly.  Bonus: fewer allocator
   hits on 1 MiB-chunked FUSE writes.
2. **Hardware CRC32C** for the `Crc32c::hash` on the hot path.
   Either adopt `crc32c` crate or add SSE4.2 intrinsics behind a
   cfg(target_feature).
3. **Skip LZ4 on detectably-incompressible chunks.**  LZ4 already
   short-circuits but still costs ~0.5 ms/MiB on random data; an
   entropy probe on the first few KiB could skip it entirely.
4. **Encryption only over touched blocks.**  Current code encrypts
   the whole (compressed) payload per write; extending the existing
   streaming API for ChaCha20 would let us encrypt per-block and
   skip unchanged blocks in mixed-content workloads.
