# CoreFS Performance Log

| Timestamp | Profile | Files | Payload (B) | Snapshots | Saves | Create (ms) | Read (ms) | Snapshot (ms) | Save (ms) | MiB | Create ops/s | Read ops/s |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| 2026-04-23 10:20:47 UTC | balanced | 250 | 4096 | 1 | 1 | 5 | 8 | 3 | 10 | 0.98 | 50000.00 | 31250.00 |
| 2026-04-23 10:20:52 UTC | small-files | 2000 | 256 | 1 | 1 | 22 | 4 | 7 | 14 | 0.49 | 90909.09 | 500000.00 |
| 2026-04-23 10:20:56 UTC | metadata-heavy | 5000 | 64 | 1 | 1 | 87 | 11 | 19 | 32 | 0.31 | 57471.26 | 454545.45 |
| 2026-04-23 10:21:01 UTC | snapshot-heavy | 400 | 1024 | 10 | 1 | 6 | 1 | 21 | 27 | 0.39 | 66666.67 | 400000.00 |
| 2026-04-23 10:21:05 UTC | persist-heavy | 800 | 4096 | 2 | 5 | 8 | 6 | 17 | 204 | 3.12 | 100000.00 | 133333.33 |
