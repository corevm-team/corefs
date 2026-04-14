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
//! - [`Timestamp`] — monotone Nanosekunden-Epoche (1970-01-01 UTC),
//!   plattformunabhängig vergleichbar und serialisierbar.
//! - [`Clock`] — Zeitquelle für "aktuelle Zeit"; nicht notwendigerweise monoton.
//! - [`Rng`] — Zufallsquelle für kryptografisches und nicht-kryptografisches
//!   Füllmaterial (Nonces, Inode-IDs, …). Die konkrete Qualität wird beim
//!   Aufruf-Kontext zugesichert.

use core::fmt;

/// Monotoner Zeitpunkt als Anzahl Nanosekunden seit der Unix-Epoche (1970-01-01 UTC).
///
/// Der Typ ist bewusst ein einfacher `u64`-Wrapper, um plattformneutral
/// serialisierbar zu sein und keine Abhängigkeit zu `std::time::SystemTime`
/// zu erzeugen. `u64` reicht bis ins Jahr 2554 (≈ 584 Jahre ab Epoche).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
#[repr(transparent)]
pub struct Timestamp(pub u64);

impl Timestamp {
    /// Der Zeitpunkt der Unix-Epoche selbst (1970-01-01T00:00:00Z).
    pub const EPOCH: Timestamp = Timestamp(0);

    /// Konstruiert einen [`Timestamp`] aus Nanosekunden seit der Unix-Epoche.
    #[inline]
    #[must_use]
    pub const fn from_nanos(nanos: u64) -> Self {
        Self(nanos)
    }

    /// Konstruiert einen [`Timestamp`] aus Sekunden seit der Unix-Epoche.
    ///
    /// Sekunden werden nach `u64`-Semantik mit `1_000_000_000` multipliziert;
    /// Überläufe werden gesättigt (also auf [`u64::MAX`] begrenzt).
    #[inline]
    #[must_use]
    pub const fn from_secs(secs: u64) -> Self {
        Self(secs.saturating_mul(1_000_000_000))
    }

    /// Liefert den Rohwert in Nanosekunden seit der Unix-Epoche.
    #[inline]
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Liefert den ganzzahligen Sekundenanteil seit der Unix-Epoche.
    #[inline]
    #[must_use]
    pub const fn as_secs(self) -> u64 {
        self.0 / 1_000_000_000
    }

    /// Liefert den Nanosekunden-Rest innerhalb der laufenden Sekunde (`0..=999_999_999`).
    #[inline]
    #[must_use]
    pub const fn subsec_nanos(self) -> u32 {
        (self.0 % 1_000_000_000) as u32
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:09}s since epoch", self.as_secs(), self.subsec_nanos())
    }
}

#[cfg(feature = "std")]
impl From<std::time::SystemTime> for Timestamp {
    /// Konvertiert eine [`std::time::SystemTime`] in einen [`Timestamp`].
    ///
    /// Zeitpunkte vor der Unix-Epoche werden auf [`Timestamp::EPOCH`] gekappt,
    /// Überläufe auf [`u64::MAX`] gesättigt.
    fn from(ts: std::time::SystemTime) -> Self {
        match ts.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => {
                let nanos = d.as_nanos();
                Timestamp(u64::try_from(nanos).unwrap_or(u64::MAX))
            }
            Err(_) => Timestamp::EPOCH,
        }
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
    fn timestamp_from_secs_saturates() {
        let max = Timestamp::from_secs(u64::MAX);
        assert_eq!(max.as_nanos(), u64::MAX);
    }

    #[test]
    fn timestamp_ordering() {
        assert!(Timestamp::from_nanos(1) < Timestamp::from_nanos(2));
        assert_eq!(Timestamp::EPOCH.as_nanos(), 0);
    }

    #[test]
    fn timestamp_display() {
        use alloc::string::ToString;
        let t = Timestamp::from_nanos(2_500_000_000);
        assert_eq!(t.to_string(), "2.500000000s since epoch");
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
}
