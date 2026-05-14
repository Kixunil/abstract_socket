//! Integration tests covering the public API surface this crate is meant to
//! abstract over: IPv4 TCP, IPv6 TCP, and pathname Unix-domain sockets.
//!
//! The `*_local_addr_roundtrip` tests exercise the `SocketAddr::from_syscall`
//! readback path: bind a listener, read back its address, assert that the
//! family and family-specific properties survive. Any failure on a BSD-style
//! platform (macOS, FreeBSD, iOS, etc.) is most likely a portability
//! regression in how the address family is read from a `sockaddr` — Linux
//! places `sa_family` at offset 0, while BSDs put `sa_len` at offset 0 and
//! `sa_family` at offset 1.
//!
//! The `smoke_*` tests exercise the full bind / connect / accept / payload
//! flow through a single `smoke_with(addr: &str)` helper. The helper parses
//! its argument via `SocketAddr`'s `FromStr` impl, so a TCP smoke and a UDS
//! smoke differ only by the configuration string passed in — exercising the
//! crate's whole reason for existing.

use abstract_socket::{Listener, SocketAddr, Stream};
use std::io::{self, Read, Write};
use std::thread;

/// Returns a UDS path short enough to fit in `sun_path` on every supported
/// platform. Darwin's `SUN_LEN` is ~104 bytes; using `/tmp/` plus a
/// per-process + per-test suffix keeps us well under that limit.
#[cfg(target_family = "unix")]
fn uds_path(tag: &str) -> std::path::PathBuf {
    std::path::PathBuf::from(format!("/tmp/as_t_{}_{}.sock", std::process::id(), tag))
}

#[test]
fn tcp_v4_local_addr_roundtrip() {
    let bind_addr: SocketAddr = "127.0.0.1:0".parse().expect("parse v4");
    let listener = Listener::bind(bind_addr).expect("bind 127.0.0.1:0");
    let local = listener.local_addr().expect("local_addr");

    match local {
        SocketAddr::Net(addr) => {
            assert!(addr.is_ipv4(), "expected IPv4, got {addr}");
            assert_ne!(addr.port(), 0, "OS should have assigned a non-zero port");
            assert_eq!(addr.ip().to_string(), "127.0.0.1");
        }
        #[cfg(target_family = "unix")]
        SocketAddr::Uds(_) => panic!("TCP bind returned a Uds local_addr"),
    }
}

#[test]
fn tcp_v6_local_addr_roundtrip() {
    let bind_addr: SocketAddr = "[::1]:0".parse().expect("parse v6");
    let listener = match Listener::bind(bind_addr) {
        Ok(l) => l,
        Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => {
            eprintln!("skipping: IPv6 loopback unavailable: {e}");
            return;
        }
        Err(e) => panic!("v6 bind failed unexpectedly: {e}"),
    };
    let local = listener.local_addr().expect("local_addr");

    match local {
        SocketAddr::Net(addr) => {
            assert!(addr.is_ipv6(), "expected IPv6, got {addr}");
            assert_ne!(addr.port(), 0);
        }
        #[cfg(target_family = "unix")]
        SocketAddr::Uds(_) => panic!("TCP bind returned a Uds local_addr"),
    }
}

#[cfg(target_family = "unix")]
#[test]
fn uds_local_addr_roundtrip() {
    let path = uds_path("la");
    let _ = std::fs::remove_file(&path);

    let path_str = path.to_str().expect("ascii path");
    let bind_addr: SocketAddr = path_str.parse().expect("parse uds");
    let listener = Listener::bind(bind_addr).expect("bind UDS");
    let local = listener.local_addr().expect("local_addr");

    match local {
        SocketAddr::Uds(addr) => {
            let returned = addr.as_pathname().expect("named UDS local_addr");
            assert_eq!(returned, path.as_path(), "local_addr path mismatch");
        }
        SocketAddr::Net(_) => panic!("UDS bind returned a Net local_addr"),
    }

    drop(listener);
    let _ = std::fs::remove_file(&path);
}

/// Common smoke flow: bind a listener at `addr`, accept one connection, and
/// exchange a small payload in both directions.
///
/// The `addr` argument is parsed via `SocketAddr`'s `FromStr` impl. Strings
/// starting with `/`, `./`, or `unix:` are treated as Unix-domain socket
/// paths; anything else is parsed as a TCP `host:port`. So a single helper
/// covers every transport this crate abstracts.
///
/// Bind errors are returned so callers can decide whether to skip (e.g. IPv6
/// loopback may be unavailable in some CI sandboxes) or panic. Inner errors
/// during accept / connect / I/O panic directly — those would be bugs, not
/// environmental skips.
fn smoke_with(addr: &str) -> io::Result<()> {
    let bind_addr: SocketAddr = addr.parse()?;
    let listener = Listener::bind(bind_addr)?;
    let local = listener.local_addr().expect("local_addr");

    let server = thread::spawn(move || {
        let (mut stream, _peer) = listener.accept().expect("accept");
        let mut buf = [0u8; 5];
        stream.read_exact(&mut buf).expect("server read");
        stream.write_all(b"WORLD").expect("server write");
        buf
    });

    let mut client = Stream::connect(local).expect("client connect");
    client.write_all(b"HELLO").expect("client write");
    let mut buf = [0u8; 5];
    client.read_exact(&mut buf).expect("client read");
    assert_eq!(&buf, b"WORLD", "client should receive WORLD");
    assert_eq!(
        &server.join().expect("server thread"),
        b"HELLO",
        "server should have read HELLO"
    );
    Ok(())
}

#[test]
fn smoke_tcp_v4() {
    smoke_with("127.0.0.1:0").expect("tcp v4 smoke");
}

#[test]
fn smoke_tcp_v6() {
    match smoke_with("[::1]:0") {
        Ok(()) => (),
        Err(e) if e.kind() == io::ErrorKind::AddrNotAvailable => {
            eprintln!("skipping: IPv6 loopback unavailable: {e}");
        }
        Err(e) => panic!("v6 smoke failed unexpectedly: {e}"),
    }
}

#[cfg(target_family = "unix")]
#[test]
fn smoke_uds() {
    let path = uds_path("sm");
    let _ = std::fs::remove_file(&path);
    smoke_with(path.to_str().expect("ascii path")).expect("uds smoke");
    let _ = std::fs::remove_file(&path);
}
