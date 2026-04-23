# CoreFS Performance History

Every run of `scripts/corefs-benchmark-vs-ext4.sh` (directly or via
`scripts/corefs-benchmark-record.sh`) produces a timestamped TSV here
so regressions can be spotted against the full historical series, not
just the most recent baseline.

## Files

| File | Purpose |
|---|---|
| `baseline.tsv` | The current known-good reference.  Regression checks compare against this.  Update manually after a confirmed improvement, with a commit explaining the new floor. |
| `YYYY-MM-DD_HHMMSS_<label>.tsv` | Individual run artefacts.  Don't delete — they are the evidence for any perf claim we make. |

## Workflow

```bash
# Run bench, archive with a label, diff against baseline
./scripts/corefs-benchmark-record.sh <label>

# Accept the current run as the new baseline (reviewed improvement)
cp perf-history/<newest>.tsv perf-history/baseline.tsv
git add perf-history/baseline.tsv perf-history/<newest>.tsv
git commit -m "perf: promote <label> as new baseline (reason: …)"
```

## Format

Tab-separated values, four columns:

```
fs<TAB>workload<TAB>ms<TAB>ops_or_mibs
```

`fs` is either `corefs-fuse` (CoreFS via FUSE on an image file) or
`ext4-direct` (the host ext4 on the same medium, as the upper bound).

Workloads are defined in `scripts/corefs-benchmark-vs-ext4.sh`.

## Windows Runs

The Windows benchmark scripts also archive timestamped TSV files here so
WinFSP performance changes are visible beside the Linux history.

```powershell
.\scripts\windows\corefs-benchmark-vs-ntfs.ps1 -ImagePath .\target\windows-bench\corefs.img -DriveLetter X:
.\scripts\windows\corefs-benchmark-mounted.ps1 -ImagePath .\target\windows-bench\mounted.img -DriveLetter X:
```

Windows artifacts use these labels by default:

| File | Purpose |
|---|---|
| `YYYY-MM-DD_HHMMSS_windows-vs-ntfs.tsv` | CoreFS/WinFSP compared with a direct NTFS directory. |
| `YYYY-MM-DD_HHMMSS_windows-mount.tsv` | Mounted CoreFS/WinFSP only, useful for quick native Windows smoke/perf runs. |

Use `-HistoryLabel <label>` for a custom suffix, `-HistoryDir <path>` for a
different archive directory, or `-NoPerfHistory` for one-off local runs.

## Known historical points

| Date | Label | Headline |
|---|---|---|
| 2026-04-18 | phase0 | Phase 0 baseline.  CoreFS 27× – 23 000× slower than ext4.  Root cause: full image rewrite on every mutation. |
| 2026-04-19 | phase1 | Phase 1.  Deferred checkpoint + streaming-flush fix.  CoreFS 1.8× – 221× slower than ext4.  `baseline.tsv` = this run. |
