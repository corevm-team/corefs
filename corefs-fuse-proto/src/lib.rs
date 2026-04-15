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
    /// Eintrag (Datei oder leeres Verzeichnis) entfernen.
    Unlink {
        /// Inode-Nummer des Eltern-Verzeichnisses.
        parent: InodeNo,
        /// Komponentenname (UTF-8, ohne `/`).
        name: String,
    },
    /// Neues Verzeichnis anlegen.
    Mkdir {
        /// Inode-Nummer des Eltern-Verzeichnisses.
        parent: InodeNo,
        /// Komponentenname (UTF-8, ohne `/`).
        name: String,
        /// POSIX-Mode-Bits.
        mode: u32,
    },
    /// Datei anlegen und öffnen (atomar, entspricht POSIX `open(O_CREAT)`).
    Create {
        /// Inode-Nummer des Eltern-Verzeichnisses.
        parent: InodeNo,
        /// Komponentenname (UTF-8, ohne `/`).
        name: String,
        /// POSIX-Mode-Bits.
        mode: u32,
        /// Open-Flags (POSIX, z. B. `O_RDWR`).
        flags: u32,
    },
    /// Inode-Attribute partiell setzen (chmod/chown/truncate/…).
    Setattr {
        /// Inode-Nummer.
        ino: InodeNo,
        /// Nur gesetzte Felder werden angewendet.
        attr: PartialAttr,
    },
    /// Leeres Verzeichnis entfernen.
    Rmdir {
        /// Inode-Nummer des Eltern-Verzeichnisses.
        parent: InodeNo,
        /// Komponentenname des Verzeichnisses.
        name: String,
    },
    /// Eintrag umbenennen / verschieben.
    Rename {
        /// Eltern-Inode der Quelle.
        parent: InodeNo,
        /// Ursprünglicher Name.
        old_name: String,
        /// Eltern-Inode des Ziels.
        new_parent: InodeNo,
        /// Zielname.
        new_name: String,
    },
    /// Symlink anlegen.
    Symlink {
        /// Eltern-Inode.
        parent: InodeNo,
        /// Name des neuen Symlinks.
        name: String,
        /// Ziel-Pfad.
        target: String,
    },
    /// Symlink-Ziel lesen.
    Readlink {
        /// Inode-Nummer des Symlinks.
        ino: InodeNo,
    },
    /// Offene File-Handles flushen.
    Flush {
        /// Inode-Nummer.
        ino: InodeNo,
        /// File-Handle.
        fh: FileHandle,
    },
    /// Offene File-Handles fsync-en.
    Fsync {
        /// Inode-Nummer.
        ino: InodeNo,
        /// File-Handle.
        fh: FileHandle,
        /// `true` = nur Daten, `false` = Daten + Metadaten.
        datasync: bool,
    },
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
    /// Antwort auf [`Request::Unlink`].
    Unlink,
    /// Antwort auf [`Request::Mkdir`].
    Mkdir(Attr),
    /// Antwort auf [`Request::Create`].
    Create {
        /// Attribute des frisch angelegten Inodes.
        attr: Attr,
        /// Vom Daemon vergebenes File-Handle.
        fh: FileHandle,
        /// Open-Result-Flags.
        flags: u32,
    },
    /// Antwort auf [`Request::Setattr`] — die neuen, vollständigen Attribute.
    Setattr(Attr),
    /// Antwort auf [`Request::Rmdir`].
    Rmdir,
    /// Antwort auf [`Request::Rename`].
    Rename,
    /// Antwort auf [`Request::Symlink`] — Attribute des neuen Symlinks.
    Symlink(Attr),
    /// Antwort auf [`Request::Readlink`] — Ziel-Pfad.
    Readlink(String),
    /// Antwort auf [`Request::Flush`].
    Flush,
    /// Antwort auf [`Request::Fsync`].
    Fsync,
}

/// Partielles Attributupdate für [`Request::Setattr`].
///
/// Jedes Feld ist `Option<T>` — `None` = unverändert lassen.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartialAttr {
    /// Neue POSIX-Mode-Bits.
    pub mode: Option<u32>,
    /// Neuer Owner-UID.
    pub uid: Option<u32>,
    /// Neue Owner-GID.
    pub gid: Option<u32>,
    /// Neue Dateigröße (Truncate).
    pub size: Option<u64>,
    /// Neue Modification-Time in Sekunden seit Epoche.
    pub mtime_secs: Option<u64>,
    /// Neue Access-Time in Sekunden seit Epoche.
    pub atime_secs: Option<u64>,
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

pub mod wire {
    //! Bincode-basierte Encode/Decode-Helfer.
    //!
    //! Seit der Migration auf `bincode` 2.x (no_std-fähig) sind diese Helfer
    //! unconditional verfügbar.  Sie verwenden `bincode::config::legacy()`,
    //! das wire-byte-identisch zur `bincode` 1.x Default-Konfiguration ist.
    //! AnyOS-Kernel-Konsumenten können diese Pfade direkt nutzen.

    use super::{ReplyFrame, RequestFrame};
    use alloc::vec::Vec;
    use bincode::config::{Configuration, Fixint, LittleEndian, NoLimit, legacy};
    use bincode::error::{DecodeError, EncodeError};

    #[inline]
    const fn cfg() -> Configuration<LittleEndian, Fixint, NoLimit> {
        legacy()
    }

    /// Serialisiert einen Request-Frame nach bincode (legacy/1.x-kompatibel).
    pub fn encode_request(frame: &RequestFrame) -> Result<Vec<u8>, EncodeError> {
        bincode::serde::encode_to_vec(frame, cfg())
    }

    /// Deserialisiert einen Request-Frame aus bincode-Bytes (legacy/1.x-kompatibel).
    pub fn decode_request(bytes: &[u8]) -> Result<RequestFrame, DecodeError> {
        bincode::serde::decode_from_slice::<RequestFrame, _>(bytes, cfg()).map(|(v, _)| v)
    }

    /// Serialisiert einen Reply-Frame nach bincode (legacy/1.x-kompatibel).
    pub fn encode_reply(frame: &ReplyFrame) -> Result<Vec<u8>, EncodeError> {
        bincode::serde::encode_to_vec(frame, cfg())
    }

    /// Deserialisiert einen Reply-Frame aus bincode-Bytes (legacy/1.x-kompatibel).
    pub fn decode_reply(bytes: &[u8]) -> Result<ReplyFrame, DecodeError> {
        bincode::serde::decode_from_slice::<ReplyFrame, _>(bytes, cfg()).map(|(v, _)| v)
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
    fn unlink_request_round_trips() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 5 },
            op: Request::Unlink {
                parent: 1,
                name: "gone.txt".to_string(),
            },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        assert_eq!(frame, wire::decode_request(&bytes).unwrap());
    }

    #[test]
    fn mkdir_request_and_reply_round_trip() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 6 },
            op: Request::Mkdir {
                parent: 1,
                name: "sub".to_string(),
                mode: 0o755,
            },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        assert_eq!(frame, wire::decode_request(&bytes).unwrap());

        let reply = ReplyFrame {
            header: FrameHeader { unique: 6 },
            payload: ReplyPayload::Ok(Reply::Mkdir(Attr {
                ino: 42, size: 0, blocks: 0, mode: 0o40755, nlink: 2,
                uid: 0, gid: 0, kind: 2, crtime_secs: 1, mtime_secs: 1, ctime_secs: 1,
            })),
        };
        let bytes = wire::encode_reply(&reply).unwrap();
        assert_eq!(reply, wire::decode_reply(&bytes).unwrap());
    }

    #[test]
    fn create_request_and_reply_round_trip() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 7 },
            op: Request::Create {
                parent: 1,
                name: "new.txt".to_string(),
                mode: 0o644,
                flags: 0x8002,
            },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        assert_eq!(frame, wire::decode_request(&bytes).unwrap());

        let reply = ReplyFrame {
            header: FrameHeader { unique: 7 },
            payload: ReplyPayload::Ok(Reply::Create {
                attr: Attr {
                    ino: 99, size: 0, blocks: 0, mode: 0o100644, nlink: 1,
                    uid: 0, gid: 0, kind: 1, crtime_secs: 1, mtime_secs: 1, ctime_secs: 1,
                },
                fh: 17,
                flags: 0,
            }),
        };
        let bytes = wire::encode_reply(&reply).unwrap();
        assert_eq!(reply, wire::decode_reply(&bytes).unwrap());
    }

    #[test]
    fn setattr_request_and_reply_round_trip() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 10 },
            op: Request::Setattr {
                ino: 42,
                attr: PartialAttr {
                    mode: Some(0o640),
                    uid: Some(1000),
                    gid: None,
                    size: Some(4096),
                    mtime_secs: Some(1_700_000_000),
                    atime_secs: None,
                },
            },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        assert_eq!(frame, wire::decode_request(&bytes).unwrap());

        let reply = ReplyFrame {
            header: FrameHeader { unique: 10 },
            payload: ReplyPayload::Ok(Reply::Setattr(Attr {
                ino: 42, size: 4096, blocks: 8, mode: 0o100640, nlink: 1,
                uid: 1000, gid: 0, kind: 1, crtime_secs: 1, mtime_secs: 1_700_000_000, ctime_secs: 2,
            })),
        };
        let bytes = wire::encode_reply(&reply).unwrap();
        assert_eq!(reply, wire::decode_reply(&bytes).unwrap());
    }

    #[test]
    fn rmdir_rename_round_trip() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 11 },
            op: Request::Rmdir { parent: 1, name: "old".to_string() },
        };
        let bytes = wire::encode_request(&frame).unwrap();
        assert_eq!(frame, wire::decode_request(&bytes).unwrap());

        let frame2 = RequestFrame {
            header: FrameHeader { unique: 12 },
            op: Request::Rename {
                parent: 1,
                old_name: "a".to_string(),
                new_parent: 2,
                new_name: "b".to_string(),
            },
        };
        let bytes2 = wire::encode_request(&frame2).unwrap();
        assert_eq!(frame2, wire::decode_request(&bytes2).unwrap());
    }

    #[test]
    fn symlink_readlink_round_trip() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 13 },
            op: Request::Symlink {
                parent: 1,
                name: "link".to_string(),
                target: "/tmp/target".to_string(),
            },
        };
        assert_eq!(
            frame,
            wire::decode_request(&wire::encode_request(&frame).unwrap()).unwrap()
        );

        let reply = ReplyFrame {
            header: FrameHeader { unique: 14 },
            payload: ReplyPayload::Ok(Reply::Readlink("/tmp/target".to_string())),
        };
        assert_eq!(
            reply,
            wire::decode_reply(&wire::encode_reply(&reply).unwrap()).unwrap()
        );
    }

    #[test]
    fn flush_fsync_round_trip() {
        let frame = RequestFrame {
            header: FrameHeader { unique: 15 },
            op: Request::Flush { ino: 3, fh: 5 },
        };
        assert_eq!(
            frame,
            wire::decode_request(&wire::encode_request(&frame).unwrap()).unwrap()
        );

        let frame2 = RequestFrame {
            header: FrameHeader { unique: 16 },
            op: Request::Fsync { ino: 3, fh: 5, datasync: true },
        };
        assert_eq!(
            frame2,
            wire::decode_request(&wire::encode_request(&frame2).unwrap()).unwrap()
        );

        let reply = ReplyFrame {
            header: FrameHeader { unique: 17 },
            payload: ReplyPayload::Ok(Reply::Flush),
        };
        assert_eq!(
            reply,
            wire::decode_reply(&wire::encode_reply(&reply).unwrap()).unwrap()
        );
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
