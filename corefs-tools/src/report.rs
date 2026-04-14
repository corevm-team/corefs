// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Gemeinsame [`Report`]-Schnittstelle für alle Tool-Operationen.
//!
//! Jede Tool-Operation liefert ein struktutiertes Ergebnis, das drei
//! Darstellungen unterstützt:
//!
//! - [`Report::summary`] — eine Zeile, geeignet für CLI-Exit-Ausgabe
//!   oder Log-Zeilen.
//! - [`Report::render_text`] — menschenlesbare, mehrzeilige Form.
//! - [`Report::render_json`] — maschinenlesbare JSON-Ausgabe (stabil,
//!   kompatibel mit `serde_json`).
//!
//! Frontends (CLI, AnyOS-Apps) wählen die passende Darstellung; das Tool
//! selbst legt sich nicht auf eine Ausgabeform fest.

use serde::Serialize;

/// Einheitliche Schnittstelle, die jedes Tool-Ergebnis erfüllt.
///
/// `Report` ist automatisch für jeden `Serialize`-Typ verfügbar, der
/// zusätzlich die `summary`-Semantik bereitstellt. In der Regel werden
/// konkrete Report-Strukturen das Trait manuell implementieren, um eine
/// präzise `summary`-Formulierung liefern zu können.
pub trait Report {
    /// Einzeilige Zusammenfassung des Ergebnisses.
    ///
    /// Beispiele:
    /// - `"formatted 16 MiB volume (geometry: 4096 blocks, 1024 inodes)"`
    /// - `"fsck clean (0 issues)"`
    /// - `"fsck: 3 issues (2 auto-fixable)"`
    fn summary(&self) -> String;

    /// Mehrzeilige, menschenlesbare Darstellung des gesamten Reports.
    ///
    /// Standardimplementierung verwendet `summary()`.
    fn render_text(&self) -> String {
        self.summary()
    }

    /// JSON-Darstellung des Reports.
    ///
    /// Standardimplementierung gibt einen einzelnen `{ "summary": "..." }`
    /// zurück. Konkrete Reports überschreiben dies typischerweise mit
    /// einer `serde_json::to_string_pretty`-basierten Implementierung.
    fn render_json(&self) -> String {
        let escaped = self.summary().replace('\\', "\\\\").replace('"', "\\\"");
        format!("{{\"summary\":\"{escaped}\"}}")
    }
}

/// Helper zum JSON-Rendern beliebiger `Serialize`-Typen in pretty-print.
///
/// Dient als Default-Implementierung für Reports, die einfach ihr
/// gesamtes serde-Schema exportieren wollen.
pub fn to_pretty_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy;
    impl Report for Dummy {
        fn summary(&self) -> String {
            "dummy ok".to_string()
        }
    }

    #[test]
    fn default_render_text_uses_summary() {
        let d = Dummy;
        assert_eq!(d.render_text(), "dummy ok");
    }

    #[test]
    fn default_render_json_wraps_summary() {
        let d = Dummy;
        assert_eq!(d.render_json(), r#"{"summary":"dummy ok"}"#);
    }

    #[test]
    fn default_render_json_escapes_quotes() {
        struct Q;
        impl Report for Q {
            fn summary(&self) -> String {
                r#"he said "hi""#.to_string()
            }
        }
        assert_eq!(Q.render_json(), r#"{"summary":"he said \"hi\""}"#);
    }

    #[test]
    fn to_pretty_json_renders_struct() {
        #[derive(Serialize)]
        struct X {
            a: u32,
            b: &'static str,
        }
        let json = to_pretty_json(&X { a: 1, b: "ok" });
        assert!(json.contains("\"a\": 1"));
        assert!(json.contains("\"b\": \"ok\""));
    }
}
