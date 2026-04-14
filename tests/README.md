# Integration-Tests

End-to-end- und Black-Box-Tests, die CoreFS ausschliesslich über die öffentliche
Crate-API ansprechen. Jede `*.rs`-Datei in diesem Verzeichnis wird von Cargo als
eigener Test-Crate kompiliert.

Unit-Tests mit Zugriff auf private Items gehören nicht hierher, sondern in ein
`*_tests.rs` neben dem jeweiligen Modul, eingebunden per:

```rust
#[cfg(test)]
#[path = "foo_tests.rs"]
mod tests;
```
