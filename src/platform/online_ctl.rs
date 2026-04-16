// Copyright (c) 2026 Mike Strathmann
// SPDX-License-Identifier: MIT

//! Control-socket infrastructure for online CoreFS tool operations.
//!
//! When a FUSE daemon mounts a CoreFS volume, it creates a Unix domain
//! socket at a well-known path derived from the mount point.  CLI tools
//! (`corefs-cli scrub --online`, etc.) connect to this socket, send an
//! [`OnlineRequest`], and read back an [`OnlineResponse`].
//!
//! The FUSE daemon processes requests by draining its
//! [`std::sync::mpsc::Receiver`] on every `fsync` / `flush` callback.
//! A tool that wants to trigger processing can call `sync` on the mount
//! point after sending the request.
//!
//! ## Socket path convention
//!
//! `/run/user/<uid>/corefs-<hash>.ctl` where `<hash>` is a short hash
//! derived from the mount-point path.  Falls back to `/tmp/corefs-<hash>.ctl`.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;

use serde::{Deserialize, Serialize};

// ── Protocol types ──────────────────────────────────────────────────────────

/// Request from a CLI tool to a running FUSE daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OnlineRequest {
    /// Run an online scrub and return the report.
    Scrub { mode: ScrubMode },
    /// Run an online defragmentation pass.
    Defrag,
    /// Create a snapshot with the given name.
    SnapshotCreate { name: String },
    /// List all snapshots.
    SnapshotList,
    /// Report daemon status (volume name, file count, dirty state).
    Status,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum ScrubMode {
    Full,
    StructuralOnly,
    ReadOnly,
}

/// Response sent back from the FUSE daemon to the CLI tool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OnlineResponse {
    /// Operation completed successfully.
    Ok { message: String },
    /// Operation failed.
    Err { message: String },
}

// ── Socket path helpers ─────────────────────────────────────────────────────

/// Compute the control socket path for a given mount point.
pub fn ctl_socket_path(mount_point: &Path) -> PathBuf {
    let hash = simple_hash(mount_point.to_string_lossy().as_bytes());
    // Prefer XDG_RUNTIME_DIR / /run/user/<uid>, fall back to /tmp.
    let dir = std::env::var("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    dir.join(format!("corefs-{hash:08x}.ctl"))
}

fn simple_hash(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in bytes {
        h ^= b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    h
}

// ── Wire encoding (length-prefixed JSON) ────────────────────────────────────

fn send_message(stream: &mut UnixStream, data: &[u8]) -> std::io::Result<()> {
    let len = (data.len() as u32).to_le_bytes();
    stream.write_all(&len)?;
    stream.write_all(data)?;
    stream.flush()
}

fn recv_message(stream: &mut UnixStream) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > 16 * 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "message too large",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

// ── Daemon-side listener ────────────────────────────────────────────────────

/// A pending request delivered to the FUSE event loop via mpsc channel.
pub struct PendingRequest {
    pub request: OnlineRequest,
    pub reply_tx: mpsc::Sender<OnlineResponse>,
}

/// Listener handle — the FUSE daemon holds this and drains `rx` on fsync.
pub struct CtlListener {
    pub rx: mpsc::Receiver<PendingRequest>,
    socket_path: PathBuf,
    _thread: thread::JoinHandle<()>,
}

impl std::fmt::Debug for CtlListener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtlListener")
            .field("socket_path", &self.socket_path)
            .finish()
    }
}

impl CtlListener {
    /// Bind a Unix socket and spawn a background thread that accepts
    /// connections.  Each connection delivers one [`PendingRequest`]
    /// to the returned `rx` channel.
    pub fn bind(mount_point: &Path) -> std::io::Result<Self> {
        let socket_path = ctl_socket_path(mount_point);

        // Remove stale socket from a previous mount.
        let _ = std::fs::remove_file(&socket_path);

        let listener = UnixListener::bind(&socket_path)?;
        // Non-blocking so the thread can check a shutdown flag.
        listener.set_nonblocking(false)?;
        let (tx, rx) = mpsc::channel();

        let path_clone = socket_path.clone();
        let handle = thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = match stream {
                    Ok(s) => s,
                    Err(_) => break,
                };
                let msg = match recv_message(&mut stream) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let request: OnlineRequest = match serde_json::from_slice(&msg) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let (reply_tx, reply_rx) = mpsc::channel();
                let pending = PendingRequest { request, reply_tx };
                if tx.send(pending).is_err() {
                    break; // main thread dropped rx
                }
                // Wait for the FUSE thread to produce a response.
                let response = reply_rx
                    .recv_timeout(std::time::Duration::from_secs(60))
                    .unwrap_or(OnlineResponse::Err {
                        message: "timeout waiting for daemon response".into(),
                    });
                let json = serde_json::to_vec(&response).unwrap_or_default();
                let _ = send_message(&mut stream, &json);
            }
            let _ = std::fs::remove_file(&path_clone);
        });

        Ok(Self {
            rx,
            socket_path,
            _thread: handle,
        })
    }

    /// Path to the control socket (for diagnostics / logging).
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

impl Drop for CtlListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

// ── Client-side helpers (used by corefs-cli --online) ───────────────────────

/// Send a request to a running FUSE daemon and wait for the response.
pub fn send_online_request(
    mount_point: &Path,
    request: &OnlineRequest,
) -> Result<OnlineResponse, String> {
    let socket_path = ctl_socket_path(mount_point);
    let mut stream = UnixStream::connect(&socket_path)
        .map_err(|e| format!("cannot connect to daemon at {}: {e}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(120)))
        .ok();
    let json = serde_json::to_vec(request).map_err(|e| format!("encode error: {e}"))?;
    send_message(&mut stream, &json).map_err(|e| format!("send error: {e}"))?;
    let reply_bytes = recv_message(&mut stream).map_err(|e| format!("recv error: {e}"))?;
    serde_json::from_slice(&reply_bytes).map_err(|e| format!("decode error: {e}"))
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn ctl_socket_path_is_deterministic() {
        let a = ctl_socket_path(Path::new("/mnt/corefs"));
        let b = ctl_socket_path(Path::new("/mnt/corefs"));
        assert_eq!(a, b);
    }

    #[test]
    fn ctl_socket_path_differs_for_different_mounts() {
        let a = ctl_socket_path(Path::new("/mnt/a"));
        let b = ctl_socket_path(Path::new("/mnt/b"));
        assert_ne!(a, b);
    }

    #[test]
    fn online_request_roundtrip_json() {
        let req = OnlineRequest::Scrub {
            mode: ScrubMode::Full,
        };
        let json = serde_json::to_vec(&req).unwrap();
        let decoded: OnlineRequest = serde_json::from_slice(&json).unwrap();
        assert!(matches!(decoded, OnlineRequest::Scrub { mode: ScrubMode::Full }));
    }

    #[test]
    fn listener_accepts_connection_and_delivers_request() {
        let mount = PathBuf::from("/tmp/corefs-test-online-ctl");
        let listener = CtlListener::bind(&mount).unwrap();
        let socket_path = listener.socket_path().to_path_buf();

        // Client sends a status request.
        let client = thread::spawn(move || {
            let mut stream = UnixStream::connect(&socket_path).unwrap();
            let req = OnlineRequest::Status;
            let json = serde_json::to_vec(&req).unwrap();
            send_message(&mut stream, &json).unwrap();
            let reply_bytes = recv_message(&mut stream).unwrap();
            let resp: OnlineResponse = serde_json::from_slice(&reply_bytes).unwrap();
            resp
        });

        // Daemon side: drain and reply.
        let pending = listener.rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert!(matches!(pending.request, OnlineRequest::Status));
        pending
            .reply_tx
            .send(OnlineResponse::Ok {
                message: "healthy".into(),
            })
            .unwrap();

        let resp = client.join().unwrap();
        assert!(matches!(resp, OnlineResponse::Ok { .. }));
    }
}
