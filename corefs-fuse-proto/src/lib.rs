// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-fuse-proto
//!
//! AnyOS-natives Wire-Format für das CoreFS-FUSE-Subsystem.
//!
//! Diese Crate definiert die [`Request`]/[`Reply`]-Enums, die zwischen
//! dem AnyOS-Kernel-FUSE-Subsystem und dem Userspace-Daemon
//! (`corefsd`) ausgetauscht werden. Die Crate ist `no_std + alloc`;
//! die Encode/Decode-Helfer (`bincode`-basiert) liegen hinter dem
//! `std`-Feature, sodass AnyOS-Kernel-Konsumenten ihre eigenen
//! Wire-Codec-Pfade liefern können.
//!
//! ## Designentscheidungen
//!
//! - **AnyOS-nativ, nicht Linux-FUSE-binärkompatibel.**
//!   Wire-Format und Op-Tabelle sind eigenständig — das hält den
//!   Kernel-Code schlank. Die optionale Linux-FUSE-Wire-Kompatibilität
//!   (Variante a aus PROJECT_PROGRESS.md, Abschnitt 5.10) bleibt ein
//!   späterer Pfad und beträfe nur ein zweites Wire-Modul, nicht
//!   diese Type-Enums.
//! - **Selbstbeschreibend.** Jede Op trägt eigene Argumente; es gibt
//!   keinen impliziten Zustand zwischen Requests außer dem
//!   `unique`-Identifier zur Reply-Zuordnung.
//! - **Versionierbar.** Der Session-Header trägt eine
//!   Protokollversion ([`PROTOCOL_VERSION`]). Inkompatible
//!   Erweiterungen bumpen diesen Wert.
//!
//! ## Stabilitätsstatus
//!
//! Skelett. Die Op-Liste wächst in Folgekommits. Die aktuell
//! definierten Varianten sind ausreichend, um den Round-Trip-Test
//! gegen `bincode` abzusichern und die Crate als verwendbares
//! Workspace-Mitglied zu etablieren.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

/// Aktuelle Protokollversion (major.minor zusammengepackt: hi16=major, lo16=minor).
///
/// Konsumenten prüfen beim `Init`-Handshake, ob der gegenüberliegende
/// Endpoint eine kompatible Version spricht. Major-Mismatch ⇒ Verbindung
/// ablehnen. Minor-Bumps sind additive (neue Op-Varianten) und müssen
/// rückwärtskompatibel bleiben.
pub const PROTOCOL_VERSION: u32 = (1u32 << 16) | 0;

/// Eindeutiger Identifier eines In-Flight-Requests.
///
/// Vom Sender (i. d. R. dem Kernel) inkrementiert pro Request; der
/// Empfänger spiegelt ihn in der Reply, damit asynchrone Antworten
/// zugeordnet werden können.
pub type Unique = u64;

/// Inode-Nummer im FUSE-Sinn (nicht zu verwechseln mit dem CoreFS-`InodeId`).
pub type InodeNo = u64;

/// File-Handle, vom Daemon bei `Open`/`Create` vergeben.
pub type FileHandle = u64;

/// Header eines Wire-Frames (Request **oder** Reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeader {
    /// Eindeutige Request-ID. In Replies gespiegelt.
    pub unique: Unique,
}

/// Frame-Wrapper für Anfragen vom Kernel an den Daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestFrame {
    /// Header.
    pub header: FrameHeader,
    /// Operation und ihre Argumente.
    pub op: Request,
}

/// Frame-Wrapper für Antworten vom Daemon an den Kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplyFrame {
    /// Header (mit gespiegelter `unique`).
    pub header: FrameHeader,
    /// Antwort-Payload (Erfolg oder Fehler).
    pub payload: ReplyPayload,
}

/// Erfolgs- oder Fehler-Payload einer Reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplyPayload {
    /// Operation erfolgreich. `Reply` trägt den Operation-spezifischen
    /// Antwort-Body.
    Ok(Reply),
    /// Operation fehlgeschlagen. `errno` ist ein POSIX-kompatibler
    /// Fehlercode (`libc::ENOENT` etc.), `message` eine optionale
    /// Diagnose.
    Err {
        /// POSIX-kompatibler Fehlercode (positiv).
        errno: i32,
        /// Optionale Diagnoseinformation für Logs.
        message: Option<String>,
    },
}

/// Eine FUSE-Operation, ausgelöst vom Kernel.
///
/// Die Op-Tabelle ist bewusst minimal — sie wächst in Folgekommits, sobald
/// das AnyOS-FUSE-Subsystem konkrete Calls braucht.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    /// Initialer Handshake. Trägt die vom Kernel gesprochene Protokollversion.
    Init {
        /// Vom Kernel gesprochene Protokollversion (siehe [`PROTOCOL_VERSION`]).
        version: u32,
    },
    /// Sauberes Beenden der Session.
    Destroy,
    /// Pfadkomponente innerhalb eines Verzeichnis-Inodes auflösen.
    Lookup {
        /// Inode-Nummer des Eltern-Verzeichnisses.
        parent: InodeNo,
        /// Komponentenname (UTF-8, ohne `/`).
        name: String,
    },
    /// Inode-Attribute lesen.
    Getattr {
        /// Inode-Nummer.
        ino: InodeNo,
    },
    /// Datei zum Lesen/Schreiben öffnen.
    Open {
        /// Inode-Nummer.
        ino: InodeNo,
        /// Open-Flags (POSIX, z. B. `O_RDONLY`).
        flags: u32,
    },
    /// Bytes aus einer geöffneten Datei lesen.
    Read {
        /// Inode-Nummer.
        ino: InodeNo,
        /// File-Handle aus `Open`.
        fh: FileHandle,
        /// Byte-Offset im File.
        offset: u64,
        /// Anzahl angeforderter Bytes.
        size: u32,
    },
    /// Bytes in eine geöffnete Datei schreiben.
    Write {
        /// Inode-Nummer.
        ino: InodeNo,
        /// File-Handle aus `Open`.
        fh: FileHandle,
        /// Byte-Offset im File.
        offset: u64,
        /// Zu schreibende Daten.
        data: Vec<u8>,
    },
    /// File-Handle freigeben.
    Release {
        /// Inode-Nummer.
        ino: InodeNo,
        /// File-Handle aus `Open`.
        fh: FileHandle,
    },
    /// Verzeichnisinhalt enumerieren.
    Readdir {
        /// Inode-Nummer des Verzeichnisses.
        ino: InodeNo,
        /// Byte-Offset des nächsten zu liefernden Eintrags.
        offset: u64,
    },
    /// Filesystem-Metriken abfragen.
    Statfs,
}

/// Operation-spezifischer Antwort-Body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    /// Antwort auf [`Request::Init`].
    Init {
        /// Vom Daemon gesprochene Protokollversion.
        version: u32,
    },
    /// Antwort auf [`Request::Destroy`].
    Destroy,
    /// Antwort auf [`Request::Lookup`].
    Lookup(Attr),
    /// Antwort auf [`Request::Getattr`].
    Getattr(Attr),
    /// Antwort auf [`Request::Open`].
    Open {
        /// Vom Daemon vergebenes File-Handle.
        fh: FileHandle,
        /// Open-Result-Flags.
        flags: u32,
    },
    /// Antwort auf [`Request::Read`].
    Read {
        /// Gelesene Daten.
        data: Vec<u8>,
    },
    /// Antwort auf [`Request::Write`].
    Write {
        /// Anzahl tatsächlich geschriebener Bytes.
        written: u32,
    },
    /// Antwort auf [`Request::Release`].
    Release,
    /// Antwort auf [`Request::Readdir`].
    Readdir(Vec<DirEntry>),
    /// Antwort auf [`Request::Statfs`].
    Statfs(StatFs),
}

/// Inode-Attribute (POSIX-Quintessenz).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attr {
    /// Inode-Nummer.
    pub ino: InodeNo,
    /// Dateigröße in Bytes.
    pub size: u64,
    /// Anzahl 512-Byte-Blöcke (POSIX-Konvention).
    pub blocks: u64,
    /// POSIX-Mode-Bits (inkl. Datei-Typ).
    pub mode: u32,
    /// Anzahl Hardlinks.
    pub nlink: u32,
    /// Owner-UID.
    pub uid: u32,
    /// Owner-GID.
    pub gid: u32,
    /// Inode-Kind (1 = File, 2 = Directory, 3 = Symlink, 4 = Other).
    pub kind: u8,
    /// crtime in Sekunden seit Epoche.
    pub crtime_secs: u64,
    /// mtime in Sekunden seit Epoche.
    pub mtime_secs: u64,
    /// ctime in Sekunden seit Epoche.
    pub ctime_secs: u64,
}

/// Eintrag in einer Readdir-Antwort.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    /// Inode-Nummer des Eintrags.
    pub ino: InodeNo,
    /// Eintragstyp (siehe [`Attr::kind`]).
    pub kind: u8,
    /// Name (UTF-8).
    pub name: String,
    /// Offset, der bei der nächsten Readdir-Anfrage verwendet werden soll.
    pub next_offset: u64,
}

/// Filesystem-Metriken.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatFs {
    /// Block-Größe.
    pub bsize: u32,
    /// Gesamtanzahl Blöcke.
    pub blocks: u64,
    /// Freie Blöcke.
    pub bfree: u64,
    /// Verfügbare Blöcke (für non-root-User).
    pub bavail: u64,
    /// Inode-Slot-Kapazität.
    pub files: u64,
    /// Freie Inode-Slots.
    pub ffree: u64,
    /// Maximale Dateinamenslänge in Bytes.
    pub namelen: u32,
}

#[cfg(feature = "std")]
pub mod wire {
    //! Bincode-basierte Encode/Decode-Helfer.
    //!
    //! Nur mit dem `std`-Feature verfügbar, da `bincode` 1.x `std::io`
    //! benötigt. AnyOS-Kernel-Konsumenten liefern eigene Wire-Pfade.

    use super::{ReplyFrame, RequestFrame};
    use alloc::vec::Vec;

    /// Serialisiert einen Request-Frame nach bincode.
    pub fn encode_request(frame: &RequestFrame) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(frame)
    }

    /// Deserialisiert einen Request-Frame aus bincode-Bytes.
    pub fn decode_request(bytes: &[u8]) -> Result<RequestFrame, bincode::Error> {
        bincode::deserialize(bytes)
    }

    /// Serialisiert einen Reply-Frame nach bincode.
    pub fn encode_reply(frame: &ReplyFrame) -> Result<Vec<u8>, bincode::Error> {
        bincode::serialize(frame)
    }

    /// Deserialisiert einen Reply-Frame aus bincode-Bytes.
    pub fn decode_reply(bytes: &[u8]) -> Result<ReplyFrame, bincode::Error> {
        bincode::deserialize(bytes)
    }
}

/// Versions-String der `corefs-fuse-proto`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn protocol_version_is_one_zero() {
        assert_eq!(PROTOCOL_VERSION >> 16, 1);
        assert_eq!(PROTOCOL_VERSION & 0xFFFF, 0);
    }

    #[cfg(feature = "std")]
    #[test]
    fn request_round_trips_via_bincode() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 42 },
            op: Request::Lookup {
                parent: 1,
                name: "hello.txt".to_string(),
            },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        let decoded = wire::decode_request(&bytes).unwrap();
        assert_eq!(frame, decoded);
    }

    #[cfg(feature = "std")]
    #[test]
    fn reply_round_trips_via_bincode_for_attr() {
        let frame = ReplyFrame {
            header: FrameHeader { unique: 7 },
            payload: ReplyPayload::Ok(Reply::Getattr(Attr {
                ino: 100,
                size: 4096,
                blocks: 8,
                mode: 0o644,
                nlink: 1,
                uid: 1000,
                gid: 1000,
                kind: 1,
                crtime_secs: 1_700_000_000,
                mtime_secs: 1_700_000_100,
                ctime_secs: 1_700_000_100,
            })),
        };
        let bytes = wire::encode_reply(&frame).unwrap();
        let decoded = wire::decode_reply(&bytes).unwrap();
        assert_eq!(frame, decoded);
    }

    #[cfg(feature = "std")]
    #[test]
    fn reply_round_trips_via_bincode_for_error() {
        let frame = ReplyFrame {
            header: FrameHeader { unique: 99 },
            payload: ReplyPayload::Err {
                errno: 2, // ENOENT
                message: Some("nope".to_string()),
            },
        };
        let bytes = wire::encode_reply(&frame).unwrap();
        let decoded = wire::decode_reply(&bytes).unwrap();
        assert_eq!(frame, decoded);
    }

    #[cfg(feature = "std")]
    #[test]
    fn write_request_carries_payload() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 1 },
            op: Request::Write {
                ino: 5,
                fh: 7,
                offset: 0,
                data: vec![1, 2, 3, 4, 5],
            },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        let decoded = wire::decode_request(&bytes).unwrap();
        assert_eq!(frame, decoded);
    }

    #[cfg(feature = "std")]
    #[test]
    fn readdir_reply_round_trips() {
        let frame = ReplyFrame {
            header: FrameHeader { unique: 3 },
            payload: ReplyPayload::Ok(Reply::Readdir(vec![
                DirEntry {
                    ino: 10,
                    kind: 1,
                    name: "a.txt".to_string(),
                    next_offset: 1,
                },
                DirEntry {
                    ino: 11,
                    kind: 2,
                    name: "subdir".to_string(),
                    next_offset: 2,
                },
            ])),
        };
        let bytes = wire::encode_reply(&frame).unwrap();
        let decoded = wire::decode_reply(&bytes).unwrap();
        assert_eq!(frame, decoded);
    }

    #[test]
    fn types_implement_clone_and_eq_under_no_std() {
        let a = Attr {
            ino: 1, size: 0, blocks: 0, mode: 0, nlink: 1,
            uid: 0, gid: 0, kind: 1, crtime_secs: 0, mtime_secs: 0, ctime_secs: 0,
        };
        let b = a;
        assert_eq!(a, b);
    }
}
