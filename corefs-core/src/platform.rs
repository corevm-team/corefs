// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Plattform-Abstraktionen für den CoreFS-Kern.
//!
//! Der Kern ist plattformneutral und kennt weder `std::time` noch eine konkrete
//! Zufallsquelle. Konsumenten (Linux-Hostprogramm, AnyOS-Kernel, Userspace-Daemon)
//! liefern passende Implementierungen der hier definierten Traits.
//!
//! ## Überblick
//!
//! - [`Timestamp`] — Zeitpunkt seit der Unix-Epoche (Sekunden + Subsekunden-Nanos),
//!   plattformunabhängig vergleichbar und serialisierbar. **Wire-kompatibel** mit
//!   `std::time::SystemTime` unter `bincode`, sodass bestehende On-Disk-Formate
//!   beim Wechsel `SystemTime` → `Timestamp` byte-identisch bleiben.
//! - [`Clock`] — abstrakte Zeitquelle für "aktuelle Zeit"; nicht notwendigerweise monoton.
//! - [`Rng`] — Zufallsquelle für kryptografisches und nicht-kryptografisches
//!   Füllmaterial (Nonces, Inode-IDs, …).

use core::fmt;

const NANOS_PER_SEC: u32 = 1_000_000_000;

/// Zeitpunkt seit der Unix-Epoche (1970-01-01T00:00:00Z), dargestellt als
/// Sekunden plus Subsekunden-Nanosekunden.
///
/// ## Wire-Kompatibilität mit `std::time::SystemTime`
///
/// Die Felder sind in derselben Reihenfolge wie die Felder, die
/// `std::time::SystemTime` unter `serde` serialisiert (`secs_since_epoch: u64`,
/// `nanos_since_epoch: u32`). Dadurch produziert `bincode::serialize(&ts)`
/// byte-identische Ausgabe zu `bincode::serialize(&system_time)`, solange
/// `system_time >= UNIX_EPOCH` (für Zeitpunkte vor der Epoche ist
/// `Timestamp` undefiniert; solche Werte werden auf `EPOCH` gekappt).
///
/// Diese Eigenschaft ist **Teil der stabilen Zusicherung** — bestehende
/// Volume-Images mit `SystemTime`-Feldern bleiben nach einer Typ-Migration zu
/// `Timestamp` lesbar. Der Kompatibilitätstest
/// `wire_compatible_with_system_time_bincode` in [`tests`] sichert diese
/// Eigenschaft regressions-fest ab.
///
/// ## Wertebereich
///
/// `secs` reicht bis ins Jahr 2554 (≈ 584 Jahre ab Epoche). `nanos` liegt
/// immer im Bereich `0..1_000_000_000` — Konstruktoren normalisieren Überträge
/// entsprechend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub struct Timestamp {
    /// Sekunden seit der Unix-Epoche (1970-01-01T00:00:00Z).
    ///
    /// Der Feldname ist identisch mit `std::time::SystemTime`s serde-Feld.
    #[serde(rename = "secs_since_epoch")]
    secs: u64,

    /// Nanosekunden innerhalb der aktuellen Sekunde (0..=999_999_999).
    ///
    /// Der Feldname ist identisch mit `std::time::SystemTime`s serde-Feld.
    #[serde(rename = "nanos_since_epoch")]
    nanos: u32,
}

impl Timestamp {
    /// Der Zeitpunkt der Unix-Epoche selbst (1970-01-01T00:00:00Z).
    pub const EPOCH: Timestamp = Timestamp { secs: 0, nanos: 0 };

    /// Konstruiert einen [`Timestamp`] aus Sekunden und Subsekunden-Nanosekunden.
    ///
    /// Überträge in `nanos` werden korrekt in `secs` übertragen; `secs`-Überläufe
    /// werden gesättigt (also auf [`u64::MAX`] begrenzt).
    #[inline]
    #[must_use]
    pub const fn from_secs_nanos(secs: u64, nanos: u32) -> Self {
        let extra_secs = (nanos / NANOS_PER_SEC) as u64;
        let remainder = nanos % NANOS_PER_SEC;
        Self {
            secs: secs.saturating_add(extra_secs),
            nanos: remainder,
        }
    }

    /// Konstruiert einen [`Timestamp`] aus Sekunden seit der Unix-Epoche.
    #[inline]
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self { secs, nanos: 0 }
    }

    /// Konstruiert einen [`Timestamp`] aus Gesamt-Nanosekunden seit der
    /// Unix-Epoche.
    #[inline]
    #[must_use]
    pub const fn from_nanos(total_nanos: u64) -> Self {
        let secs = total_nanos / (NANOS_PER_SEC as u64);
        let nanos = (total_nanos % (NANOS_PER_SEC as u64)) as u32;
        Self { secs, nanos }
    }

    /// Liefert den Sekunden-Anteil seit der Unix-Epoche.
    #[inline]
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.secs
    }

    /// Liefert den Nanosekunden-Rest innerhalb der laufenden Sekunde (`0..=999_999_999`).
    #[inline]
    #[must_use]
    pub const fn subsec_nanos(self) -> u32 {
        self.nanos
    }

    /// Liefert die Gesamtanzahl Nanosekunden seit der Unix-Epoche,
    /// gesättigt auf [`u64::MAX`] bei Überlauf.
    #[inline]
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        // secs * 1e9 + nanos, saturierend.
        let secs_as_nanos = self.secs.saturating_mul(NANOS_PER_SEC as u64);
        secs_as_nanos.saturating_add(self.nanos as u64)
    }

    /// Liefert den aktuellen Zeitpunkt via [`std::time::SystemTime::now`].
    ///
    /// Nur mit aktiviertem `std`-Feature verfügbar. Im no_std-Kontext ist eine
    /// [`Clock`]-Implementierung zu verwenden.
    #[cfg(feature = "std")]
    #[must_use]
    pub fn now() -> Self {
        std::time::SystemTime::now().into()
    }
}

impl Default for Timestamp {
    #[inline]
    fn default() -> Self {
        Self::EPOCH
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:09}s since epoch", self.secs, self.nanos)
    }
}

#[cfg(feature = "std")]
impl From<std::time::SystemTime> for Timestamp {
    /// Konvertiert eine [`std::time::SystemTime`] in einen [`Timestamp`].
    ///
    /// Zeitpunkte vor der Unix-Epoche werden auf [`Timestamp::EPOCH`] gekappt,
    /// `secs`-Überläufe auf [`u64::MAX`] gesättigt.
    fn from(ts: std::time::SystemTime) -> Self {
        match ts.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => Timestamp {
                secs: d.as_secs(),
                nanos: d.subsec_nanos(),
            },
            Err(_) => Timestamp::EPOCH,
        }
    }
}

#[cfg(feature = "std")]
impl From<Timestamp> for std::time::SystemTime {
    /// Konvertiert einen [`Timestamp`] in eine [`std::time::SystemTime`].
    ///
    /// Überläufe werden auf die am weitesten in der Zukunft darstellbare
    /// `SystemTime` gesättigt.
    fn from(ts: Timestamp) -> Self {
        use std::time::{Duration, UNIX_EPOCH};
        UNIX_EPOCH
            .checked_add(Duration::new(ts.secs, ts.nanos))
            .unwrap_or_else(|| UNIX_EPOCH + Duration::new(u64::MAX, NANOS_PER_SEC - 1))
    }
}

/// Abstrakte Zeitquelle.
///
/// Implementierungen liefern die aktuelle Wanduhrzeit in einem plattformneutralen
/// [`Timestamp`]-Format. Monotonie ist *nicht* garantiert — Konsumenten, die
/// einen strikt monotonen Counter brauchen (z. B. Generation-Counter),
/// müssen das selbst auf Basis von `Clock::now` durchsetzen.
pub trait Clock: Send + Sync {
    /// Liefert die aktuelle Zeit.
    fn now(&self) -> Timestamp;
}

/// [`Clock`]-Implementierung, die auf [`std::time::SystemTime::now`] zurückgreift.
///
/// Nur mit aktiviertem `std`-Feature verfügbar.
#[cfg(feature = "std")]
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

#[cfg(feature = "std")]
impl Clock for SystemClock {
    #[inline]
    fn now(&self) -> Timestamp {
        Timestamp::now()
    }
}

/// Abstrakte Zufallsquelle.
///
/// Konsumenten müssen ggf. selbst dokumentieren, welche Qualität (kryptografisch,
/// nicht-kryptografisch) sie erwarten. Die Trait-Definition selbst macht keine
/// Qualitätsaussage.
pub trait Rng: Send + Sync {
    /// Füllt den gesamten Puffer mit Zufallsbytes.
    fn fill_bytes(&mut self, dest: &mut [u8]);

    /// Liefert einen zufälligen `u64`.
    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut buf = [0u8; 8];
        self.fill_bytes(&mut buf);
        u64::from_le_bytes(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_roundtrip_nanos() {
        let t = Timestamp::from_nanos(1_234_567_890);
        assert_eq!(t.as_nanos(), 1_234_567_890);
        assert_eq!(t.as_secs(), 1);
        assert_eq!(t.subsec_nanos(), 234_567_890);
    }

    #[test]
    fn timestamp_from_secs_nanos_normalises_overflow() {
        let t = Timestamp::from_secs_nanos(5, NANOS_PER_SEC + 250);
        assert_eq!(t.as_secs(), 6);
        assert_eq!(t.subsec_nanos(), 250);
    }

    #[test]
    fn timestamp_from_secs_no_subsec() {
        let t = Timestamp::from_secs(42);
        assert_eq!(t.as_secs(), 42);
        assert_eq!(t.subsec_nanos(), 0);
        assert_eq!(t.as_nanos(), 42 * NANOS_PER_SEC as u64);
    }

    #[test]
    fn timestamp_ordering() {
        assert!(Timestamp::from_secs(1) < Timestamp::from_secs(2));
        assert!(
            Timestamp::from_secs_nanos(1, 0) < Timestamp::from_secs_nanos(1, 1),
            "subsec nanos must break ties"
        );
        assert_eq!(Timestamp::EPOCH.as_secs(), 0);
    }

    #[test]
    fn timestamp_as_nanos_saturates_on_overflow() {
        // secs * 1e9 würde weit über u64::MAX überlaufen.
        let t = Timestamp::from_secs(u64::MAX);
        assert_eq!(t.as_nanos(), u64::MAX);
    }

    #[test]
    fn timestamp_display() {
        use alloc::string::ToString;
        let t = Timestamp::from_nanos(2_500_000_000);
        assert_eq!(t.to_string(), "2.500000000s since epoch");
    }

    #[test]
    fn timestamp_default_is_epoch() {
        assert_eq!(Timestamp::default(), Timestamp::EPOCH);
    }

    #[cfg(feature = "std")]
    #[test]
    fn timestamp_from_and_into_system_time_roundtrip() {
        use std::time::{Duration, UNIX_EPOCH};
        let original = UNIX_EPOCH + Duration::new(1_700_000_000, 123_456_789);
        let ts: Timestamp = original.into();
        assert_eq!(ts.as_secs(), 1_700_000_000);
        assert_eq!(ts.subsec_nanos(), 123_456_789);
        let back: std::time::SystemTime = ts.into();
        assert_eq!(back, original);
    }

    #[cfg(feature = "std")]
    #[test]
    fn timestamp_from_system_time_before_epoch_saturates_to_epoch() {
        use std::time::{Duration, UNIX_EPOCH};
        let before = UNIX_EPOCH - Duration::from_secs(100);
        let ts: Timestamp = before.into();
        assert_eq!(ts, Timestamp::EPOCH);
    }

    /// **Kritische Zusicherung**: `Timestamp` produziert unter `bincode`
    /// byte-identische Ausgabe zu `std::time::SystemTime`. Diese Eigenschaft
    /// ist die Basis dafür, dass bestehende Volume-Images nach dem Typ-Wechsel
    /// `SystemTime` → `Timestamp` unverändert lesbar bleiben.
    #[cfg(feature = "std")]
    #[test]
    fn wire_compatible_with_system_time_bincode() {
        use std::time::{Duration, UNIX_EPOCH};
        let samples = [
            UNIX_EPOCH,
            UNIX_EPOCH + Duration::from_secs(1),
            UNIX_EPOCH + Duration::new(1_234_567_890, 987_654_321),
            UNIX_EPOCH + Duration::new(u64::MAX / 2, 999_999_999),
        ];
        for st in samples {
            let ts: Timestamp = st.into();
            let st_bytes = bincode::serialize(&st).expect("serialize SystemTime");
            let ts_bytes = bincode::serialize(&ts).expect("serialize Timestamp");
            assert_eq!(
                st_bytes, ts_bytes,
                "bincode-Ausgabe von Timestamp und SystemTime muss identisch sein (sample: {st:?})",
            );

            // Der umgekehrte Pfad muss ebenfalls funktionieren: bincode-Bytes
            // einer SystemTime lassen sich als Timestamp deserialisieren.
            let decoded: Timestamp = bincode::deserialize(&st_bytes).expect("deserialize as Timestamp");
            assert_eq!(decoded, ts);
        }
    }

    /// Ein deterministischer Test-RNG (xorshift64), nutzbar als Referenz-Impl.
    struct TestRng(u64);
    impl Rng for TestRng {
        fn fill_bytes(&mut self, dest: &mut [u8]) {
            for chunk in dest.chunks_mut(8) {
                let mut x = self.0;
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                self.0 = x;
                let bytes = x.to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
    }

    #[test]
    fn rng_fill_bytes_is_deterministic_for_seed() {
        let mut a = TestRng(0xDEAD_BEEF);
        let mut b = TestRng(0xDEAD_BEEF);
        let mut buf_a = [0u8; 32];
        let mut buf_b = [0u8; 32];
        a.fill_bytes(&mut buf_a);
        b.fill_bytes(&mut buf_b);
        assert_eq!(buf_a, buf_b);
    }

    #[test]
    fn rng_next_u64_advances_state() {
        let mut rng = TestRng(42);
        let a = rng.next_u64();
        let b = rng.next_u64();
        assert_ne!(a, b);
    }

    /// Ein konstanter Clock, nutzbar für Tests und Simulationen.
    struct FixedClock(Timestamp);
    impl Clock for FixedClock {
        fn now(&self) -> Timestamp {
            self.0
        }
    }

    #[test]
    fn clock_trait_object_safety() {
        let c: alloc::boxed::Box<dyn Clock> = alloc::boxed::Box::new(FixedClock(Timestamp::from_secs(100)));
        assert_eq!(c.now().as_secs(), 100);
    }

    #[cfg(feature = "std")]
    #[test]
    fn system_clock_returns_reasonable_time() {
        let clock = SystemClock;
        let t = clock.now();
        // Sanity: we are after 2020-01-01 (1_577_836_800) and before 2100-01-01 (4_102_444_800).
        assert!(t.as_secs() > 1_577_836_800);
        assert!(t.as_secs() < 4_102_444_800);
    }
}
