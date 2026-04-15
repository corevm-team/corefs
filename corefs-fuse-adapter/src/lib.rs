// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! # corefs-fuse-adapter
//!
//! Plattformneutraler Adapter zwischen CoreFS-Operationen und dem
//! [`corefs-fuse-proto`]-Wire-Format. Diese Crate ist die Brücke,
//! die sowohl unter Linux (Daemon hinter `/dev/fuse`) als auch unter
//! AnyOS (Daemon hinter dem AnyOS-FUSE-Subsystem) wiederverwendet
//! werden kann.
//!
//! Der konkrete Transport (`/dev/fuse`-File, AnyOS-Syscall, …) ist
//! **nicht** Teil dieser Crate — er wird über [`Transport`] abstrahiert.
//! Damit bleibt die Adapter-Logik plattformneutral und kompiliert
//! `no_std + alloc`.
//!
//! ## Aktueller Status (Phase 5.1)
//!
//! Skelett. Die Crate definiert die Adapter-Trait-Skelette
//! ([`Transport`], [`FuseHandler`]) und einen einfachen
//! [`SessionLoop`], der Frames vom Transport entgegennimmt, an einen
//! Handler delegiert und die Reply zurückspielt. Die Bindung an
//! `CoreFsService` aus dem main `corefs` crate folgt in einem
//! späteren Schritt, sobald `storage` nach `corefs-core` migriert
//! ist.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(rustdoc::broken_intra_doc_links)]

extern crate alloc;

use alloc::string::String;
use core::fmt;
use corefs_fuse_proto::{Reply, ReplyFrame, ReplyPayload, Request, RequestFrame};

/// Transport-abstrakter Endpunkt: liefert eingehende Requests und
/// schluckt ausgehende Replies.
///
/// Linux-Implementierungen wickeln das über `/dev/fuse` ab, AnyOS über
/// einen Syscall oder ein Char-Device. Der Adapter selbst kennt den
/// konkreten Transport nicht.
pub trait Transport {
    /// Transport-spezifischer Fehlertyp.
    type Error: fmt::Debug;

    /// Liest den nächsten Request-Frame.
    ///
    /// `Ok(None)` signalisiert sauberes Ende der Session (z. B. Unmount).
    fn recv_request(&mut self) -> Result<Option<RequestFrame>, Self::Error>;

    /// Schickt einen Reply-Frame zurück.
    fn send_reply(&mut self, reply: &ReplyFrame) -> Result<(), Self::Error>;
}

/// Eine Implementierung des FUSE-Handlers — wird vom `SessionLoop`
/// aufgerufen, um eine konkrete Operation zu beantworten.
///
/// Die Trait-Methode bekommt den dekodierten [`Request`] und liefert
/// einen [`Reply`] (oder einen `errno`-Fehler). Der Handler ist
/// bewusst stateless gegenüber dem Transport — Session-State (offene
/// File-Handles, Inode-Mapping) lebt im Implementierer selbst.
pub trait FuseHandler {
    /// Verarbeitet eine einzelne Operation.
    ///
    /// Implementierungen, die einen POSIX-Fehler zurückgeben wollen,
    /// liefern `HandlerResult::Err { errno, message }` — der `SessionLoop`
    /// verpackt das in eine `ReplyPayload::Err`.
    fn handle(&mut self, request: Request) -> HandlerResult;
}

/// Ergebnistyp eines Handler-Aufrufs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandlerResult {
    /// Erfolgreiche Operation.
    Ok(Reply),
    /// POSIX-Fehler.
    Err {
        /// POSIX-Errno.
        errno: i32,
        /// Optionale Diagnose.
        message: Option<String>,
    },
}

impl HandlerResult {
    /// Hebt das Ergebnis in eine [`ReplyPayload`].
    #[must_use]
    pub fn into_payload(self) -> ReplyPayload {
        match self {
            HandlerResult::Ok(r) => ReplyPayload::Ok(r),
            HandlerResult::Err { errno, message } => ReplyPayload::Err { errno, message },
        }
    }
}

/// Einfacher Session-Loop: konsumiert Requests, leitet sie an einen
/// Handler weiter, schickt die Replies zurück. Beendet sich, wenn der
/// Transport `recv_request` mit `Ok(None)` quittiert.
///
/// Erwartet **nicht**, dass jeder Request beantwortet werden muss —
/// `Request::Destroy` schickt eine Reply und beendet danach die Schleife.
pub struct SessionLoop<T: Transport, H: FuseHandler> {
    transport: T,
    handler: H,
}

impl<T: Transport, H: FuseHandler> SessionLoop<T, H> {
    /// Konstruiert einen neuen Loop um Transport + Handler.
    pub fn new(transport: T, handler: H) -> Self {
        Self { transport, handler }
    }

    /// Läuft, bis der Transport `Ok(None)` liefert, ein `Destroy`-Request
    /// verarbeitet wurde, oder ein Transport-Fehler auftritt.
    ///
    /// Bei Transport-Fehlern wird die Schleife beendet und der Fehler
    /// zurückgegeben.
    /// Zerlegt die Session nach Ende des Laufs in Transport und Handler.
    /// Nützlich für Tests / Debug-Inspektion der Transport-Interna.
    pub fn into_parts(self) -> (T, H) {
        (self.transport, self.handler)
    }

    pub fn run(&mut self) -> Result<(), T::Error> {
        loop {
            let frame = match self.transport.recv_request()? {
                Some(f) => f,
                None => return Ok(()),
            };
            let is_destroy = matches!(frame.op, Request::Destroy);
            let result = self.handler.handle(frame.op);
            let reply = ReplyFrame {
                header: frame.header,
                payload: result.into_payload(),
            };
            self.transport.send_reply(&reply)?;
            if is_destroy {
                return Ok(());
            }
        }
    }
}

/// Versions-String der `corefs-fuse-adapter`-Crate (aus `Cargo.toml`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::VecDeque;
    use alloc::string::ToString;
    use alloc::vec;
    use alloc::vec::Vec;
    use corefs_fuse_proto::{Attr, FrameHeader, PROTOCOL_VERSION};

    /// In-Memory-Transport für Tests: zieht Requests aus einer Queue,
    /// sammelt Replies in einem Vec.
    struct MockTransport {
        incoming: VecDeque<RequestFrame>,
        sent: Vec<ReplyFrame>,
    }
    impl MockTransport {
        fn new(requests: Vec<RequestFrame>) -> Self {
            Self {
                incoming: requests.into(),
                sent: Vec::new(),
            }
        }
    }
    impl Transport for MockTransport {
        type Error = &'static str;
        fn recv_request(&mut self) -> Result<Option<RequestFrame>, Self::Error> {
            Ok(self.incoming.pop_front())
        }
        fn send_reply(&mut self, reply: &ReplyFrame) -> Result<(), Self::Error> {
            self.sent.push(reply.clone());
            Ok(())
        }
    }

    /// Handler, der nur `Init` mit der korrekten Version beantwortet,
    /// `Destroy` quittiert und alle anderen Ops mit `ENOSYS` ablehnt.
    struct StubHandler;
    impl FuseHandler for StubHandler {
        fn handle(&mut self, request: Request) -> HandlerResult {
            match request {
                Request::Init { .. } => HandlerResult::Ok(Reply::Init {
                    version: PROTOCOL_VERSION,
                }),
                Request::Destroy => HandlerResult::Ok(Reply::Destroy),
                Request::Getattr { ino } => HandlerResult::Ok(Reply::Getattr(Attr {
                    ino,
                    size: 0,
                    blocks: 0,
                    mode: 0o755,
                    nlink: 1,
                    uid: 0,
                    gid: 0,
                    kind: 2,
                    crtime_secs: 0,
                    mtime_secs: 0,
                    ctime_secs: 0,
                })),
                _ => HandlerResult::Err {
                    errno: 38, // ENOSYS
                    message: Some("not implemented".to_string()),
                },
            }
        }
    }

    fn req(unique: u64, op: Request) -> RequestFrame {
        RequestFrame {
            header: FrameHeader { unique },
            op,
        }
    }

    #[test]
    fn loop_terminates_on_empty_transport() {
        let transport = MockTransport::new(vec![]);
        let mut session = SessionLoop::new(transport, StubHandler);
        session.run().unwrap();
    }

    #[test]
    fn loop_terminates_on_destroy() {
        let transport = MockTransport::new(vec![
            req(1, Request::Init { version: PROTOCOL_VERSION }),
            req(2, Request::Destroy),
            // Should never be processed:
            req(3, Request::Getattr { ino: 1 }),
        ]);
        let mut session = SessionLoop::new(transport, StubHandler);
        session.run().unwrap();
        assert_eq!(session.transport.sent.len(), 2);
    }

    #[test]
    fn loop_mirrors_unique_in_reply() {
        let transport = MockTransport::new(vec![
            req(42, Request::Getattr { ino: 5 }),
            req(43, Request::Destroy),
        ]);
        let mut session = SessionLoop::new(transport, StubHandler);
        session.run().unwrap();
        assert_eq!(session.transport.sent[0].header.unique, 42);
        assert_eq!(session.transport.sent[1].header.unique, 43);
    }

    #[test]
    fn handler_err_becomes_err_payload() {
        let transport = MockTransport::new(vec![
            req(1, Request::Read { ino: 1, fh: 1, offset: 0, size: 4 }),
            req(2, Request::Destroy),
        ]);
        let mut session = SessionLoop::new(transport, StubHandler);
        session.run().unwrap();
        match &session.transport.sent[0].payload {
            ReplyPayload::Err { errno, .. } => assert_eq!(*errno, 38),
            other => panic!("expected Err, got {:?}", other),
        }
    }

    #[test]
    fn handler_ok_becomes_ok_payload() {
        let transport = MockTransport::new(vec![
            req(1, Request::Init { version: PROTOCOL_VERSION }),
            req(2, Request::Destroy),
        ]);
        let mut session = SessionLoop::new(transport, StubHandler);
        session.run().unwrap();
        assert!(matches!(
            session.transport.sent[0].payload,
            ReplyPayload::Ok(Reply::Init { .. })
        ));
    }

    #[test]
    fn handler_result_into_payload_round_trip() {
        let ok = HandlerResult::Ok(Reply::Destroy);
        let p = ok.into_payload();
        assert!(matches!(p, ReplyPayload::Ok(Reply::Destroy)));

        let err = HandlerResult::Err {
            errno: 2,
            message: Some("missing".to_string()),
        };
        let p = err.into_payload();
        assert!(matches!(p, ReplyPayload::Err { errno: 2, .. }));
    }
}
