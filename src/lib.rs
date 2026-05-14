//! Abstractions over TCP and Unix sockets.
//!
//! **This crate is in development and wasn't widely tested yet.**
//! It may also lack some functionality. I'll be happy to accept contributions.
//!
//! This crate helps you write applications that support both TCP and Unix sockets by providing
//! mostly-drop-in replacements for the respective types. If `target_family` is not `unix` the unix
//! socket support transparently disappears unless you're manually inspecting the socket address.
//! This ensures your application is multi-platform out of the box.
//!
//! The crate currently only supports streams, not datagrams.
//!
//! To add Unix support to an existing application supporting TCP, follow these steps:
//!
//! * Replace `TcpStream` (or `UnixStream`) with [`Stream`]
//! * Replace `TcpListener` (or `UnixListener`) with [`Listener`]
//! * Replace `SocketAddr` from `std` with [`SocketAddr`]
//! * Fix remaining type issues, if any. There shouldn't be many.
//!
//! ## Feature flags:
//! 
//! * `async-io` - uses the `async-io` crate to provide an `async-std`-compatible wrapper. Note that
//!   this wasn't tested in practice because I mistakenly thought I need this but I dicovered that I
//!   don't just before testing it, so I just kept it in.
//! * `parse_arg` - adds `parse_arg::ParseArg` impl for `SocketAddr` to make getting it from command
//!   line arguments easier.
//! * `serde` - adds `Deserialize` impl for `SocketAddr` so that it can be easily retrieved from a
//!   configuration file
//!
//! ## MSRV
//!
//! The current MSRV is 1.70. The policy is to at least support the latest Debian stable release and not
//! bump the version for cosmetic changes.

#![warn(missing_docs)]

#[cfg(target_family = "unix")]
use std::borrow::Cow;
use std::ffi::{OsString, OsStr};
use std::fmt;
use std::io;
#[cfg(target_family = "unix")]
use std::mem::MaybeUninit;
#[cfg(target_family = "unix")]
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
#[cfg(target_family = "unix")]
use std::os::unix::io::{AsRawFd, FromRawFd, IntoRawFd, RawFd};
#[cfg(target_family = "unix")]
use std::os::unix::ffi::{OsStrExt as _};
#[cfg(target_family = "unix")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::os::windows::io::{AsSocket, AsRawSocket, BorrowedSocket, FromRawSocket, IntoRawSocket, OwnedSocket, RawSocket};

macro_rules! delegate_raw_fd {
    ($type:ident, $field:ident) => {
        #[cfg(target_family = "unix")]
        impl FromRawFd for $type {
            unsafe fn from_raw_fd(fd: RawFd) -> Self {
                $type {
                    // SOUNDNESS: the soundness rules are delegated to the caller
                    $field: unsafe { FromRawFd::from_raw_fd(fd) },
                }
            }
        }

        #[cfg(target_family = "unix")]
        impl AsRawFd for $type {
            fn as_raw_fd(&self) -> RawFd {
                self.$field.as_raw_fd()
            }
        }

        #[cfg(target_family = "unix")]
        impl IntoRawFd for $type {
            fn into_raw_fd(self) -> RawFd {
                self.$field.into_raw_fd()
            }
        }

        #[cfg(target_family = "unix")]
        impl From<$type> for OwnedFd {
            fn from(value: $type) -> Self {
                value.$field
            }
        }

        #[cfg(target_family = "unix")]
        impl AsFd for $type {
            fn as_fd(&self) -> BorrowedFd<'_> {
                self.$field.as_fd()
            }
        }

        #[cfg(target_os = "windows")]
        impl AsSocket for $type {
            fn as_socket(&self) -> BorrowedSocket<'_> {
                self.$field.as_socket()
            }
        }

        #[cfg(target_os = "windows")]
        impl FromRawSocket for $type {
            unsafe fn from_raw_socket(socket: RawSocket) -> Self {
                $type {
                    // SOUNDNESS: the soundness rules are delegated to the caller
                    $field: unsafe { FromRawSocket::from_raw_socket(socket) },
                }
            }
        }

        #[cfg(target_os = "windows")]
        impl AsRawSocket for $type {
            fn as_raw_socket(&self) -> RawSocket {
                self.$field.as_raw_socket()
            }
        }

        #[cfg(target_os = "windows")]
        impl IntoRawSocket for $type {
            fn into_raw_socket(self) -> RawSocket {
                self.$field.into_raw_socket()
            }
        }

        #[cfg(target_os = "windows")]
        impl From<$type> for OwnedSocket {
            fn from(value: $type) -> Self {
                value.$field.into()
            }
        }
    }
}

// couldn't be bothered to impl a trait for each numeric type
#[cfg(target_family = "unix")]
macro_rules! ck_syscall {
    ($val:expr) => {
        // classic trick to prevent double evaluation without affecting lifetimes
        match $val {
            // no partial range syntax :(
            invalid if invalid < -1 => return Err(io::Error::last_os_error()),
            valid => valid,
        }
    }
}

/// Abstracts over [`TcpStream`](std::net::TcpStream) and
/// [`UnixStream`](std::os::unix::net::UnixStream).
///
/// This type provides a unified API for working with byte streams that are morally sockets - among
/// the usual read and write methods this provides other critical methods like `set_nonblocking`
/// and `shutdown` without making assumptions about which kind of socket this is.
///
/// The type does **not** track the socket type so it is effectively just a file descriptor. This
/// should work fine for most applications.
#[derive(Debug)]
pub struct Stream {
    #[cfg(target_family = "unix")]
    fd: OwnedFd,
    #[cfg(not(target_family = "unix"))]
    fd: std::net::TcpStream,
}

impl Stream {
    /// Connects to the given address.
    ///
    /// If the address starts with `unix:`, `/`, or `./` it will be treated as a path and unix
    /// socket will be created. Otherwise a TCP socket will be used.
    pub fn connect<A: ToSocketAddrs>(address: A) -> io::Result<Self> {
        each_socket_addr(address, Self::connect_one)
    }

    /// Sets the socket as non-blocking if the argument is `true`.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.fd.set_nonblocking(nonblocking)
    }

    /// Shuts down the socket.
    pub fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        self.fd.shutdown(how)
    }

    /// Attempts to clone the stream by asking the OS to create a new file descriptor.
    ///
    /// The cloned stream points to the same resource, effectively presenting shared mutability.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { fd: self.fd.try_clone()? })
    }

    fn connect_one(address: SocketAddr) -> io::Result<Self> {
        match address {
            SocketAddr::Net(address) => std::net::TcpStream::connect(address).map(Into::into),
            #[cfg(target_family = "unix")]
            SocketAddr::Uds(path) => std::os::unix::net::UnixStream::connect_addr(&path).map(Into::into),
        }
    }
}

fn each_socket_addr<A: ToSocketAddrs, R>(address: A, mut f: impl FnMut(SocketAddr) -> io::Result<R>) -> io::Result<R> {
        let mut last_error = None;
        for address in address.to_socket_addrs()? {
            match f(address) {
                Ok(stream) => return Ok(stream),
                Err(error) => last_error = Some(error),
            }
        }

        match last_error {
            Some(error) => Err(error),
            // std produces an optimized version of the IO error for the case when there are no
            // addresses but this cannot be accessed directly. Here we trigger the error
            // intentionally in a way that should be optimizable by the compiler.
            None => Err(std::net::TcpStream::connect::<&[std::net::SocketAddr]>(&[]).unwrap_err()),
        }
}

impl From<std::net::TcpStream> for Stream {
    fn from(value: std::net::TcpStream) -> Self {
        #[cfg(target_family = "unix")]
        { Stream { fd: value.into() } }
        #[cfg(not(target_family = "unix"))]
        { Stream { fd: value } }
    }
}

#[cfg(target_family = "unix")]
impl From<std::os::unix::net::UnixStream> for Stream {
    fn from(value: std::os::unix::net::UnixStream) -> Self {
        Stream { fd: value.into() }
    }
}

delegate_raw_fd!(Stream, fd);

impl io::Read for &'_ Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SOUNDNESS: the descriptor validity is guaranteed by the type invariant, the pointer and
        // length are referencing the slice.
        #[cfg(target_family = "unix")]
        unsafe {
            Ok(ck_syscall!(libc::read(self.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len())) as usize)
        }
        #[cfg(not(target_family = "unix"))]
        (&self.fd).read(buf)
    }

    // TODO uninitialized buffer
    // TODO vectored
}

impl io::Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self).read(buf)
    }
}

impl io::Write for &'_ Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SOUNDNESS: the descriptor validity is guaranteed by the type invariant, the pointer and
        // length are referencing the slice.
        #[cfg(target_family = "unix")]
        unsafe {
            Ok(ck_syscall!(libc::write(self.as_raw_fd(), buf.as_ptr().cast(), buf.len())) as usize)
        }
        #[cfg(not(target_family = "unix"))]
        (&self.fd).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    // TODO vectored
}

impl io::Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self).write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
    // TODO vectored
}

/// Abstracts over [`TcpListener`](std::net::TcpListener) and
/// [`UnixListener`](std::os::unix::net::UnixListener).
///
/// This represents a stream of incoming connections that may be TCP or Unix socket depending on
/// the kind of the listener.
#[derive(Debug)]
pub struct Listener {
    #[cfg(target_family = "unix")]
    fd: OwnedFd,
    #[cfg(not(target_family = "unix"))]
    fd: std::net::TcpListener,
}

impl Listener {
    /// Binds to the given address.
    ///
    /// If the address starts with `unix:`, `/`, or `./` it will be treated as a path and unix
    /// socket will be created. Otherwise a TCP socket will be used.
    pub fn bind<A: ToSocketAddrs>(address: A) -> io::Result<Self> {
        each_socket_addr(address, Self::bind_one)
    }

    fn bind_one(address: SocketAddr) -> io::Result<Self> {
        match address {
            SocketAddr::Net(address) => std::net::TcpListener::bind(address).map(Into::into),
            #[cfg(target_family = "unix")]
            SocketAddr::Uds(path) => std::os::unix::net::UnixListener::bind_addr(&path).map(Into::into),
        }
    }

    /// Accepts a new connection.
    ///
    /// Unless the socket is in non-blocking mode this blocks until a new connection is made.
    pub fn accept(&self) -> io::Result<(Stream, SocketAddr)> {
        #[cfg(target_family = "unix")]
        unsafe {
            SocketAddr::from_syscall(|addr, len| self.accept_maybe_without_address(Some((addr, len))))
        }
        #[cfg(not(target_family = "unix"))]
        {
            self.fd.accept().map(|(socket, addr)| (socket.into(), addr.into()))
        }
    }

    /// Returns an iterator over the incomming connections.
    pub fn incoming(&self) -> Incoming<'_> {
        Incoming(self)
    }

    /// Returns the address the listener is bound to.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        #[cfg(target_family = "unix")]
        {
        // SOUNDNESS: we pass addr and len to getsockname which properly fills them, the descriptor
        // validity is guaranteed by type invariant, the validity of len is guaranteed by
        // `from_syscall`.
            unsafe { SocketAddr::from_syscall(|addr, len| { ck_syscall!(libc::getsockname(self.fd.as_raw_fd(), addr.as_mut_ptr().cast(), len)); Ok(()) }) }
            .map(|(_, addr)| addr)
        }
        #[cfg(not(target_family = "unix"))]
        {
            self.fd.local_addr().map(Into::into)
        }
    }

    #[cfg(target_family = "unix")]
    fn accept_maybe_without_address(&self, addr: Option<(&mut MaybeUninit<CAddr>, &mut libc::socklen_t)>) -> io::Result<Stream> {
        let (ptr, len) = match addr {
            Some((addr, len)) => (addr.as_mut_ptr().cast(), len as *mut _),
            None => (std::ptr::null_mut(), std::ptr::null_mut()),
        };
        // SOUNDNESS: we pass the pointer and length to a syscall that properly fills them or null
        // pointers which are explicitly allowed. The descriptor validity is guaranteed by the
        // type.
        //
        // Not all OSes support accept4
        #[cfg(any(
            target_os = "android",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "illumos",
            target_os = "linux",
            target_os = "hurd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "cygwin",
        ))]
        unsafe {
            let fd = ck_syscall!(libc::accept4(self.fd.as_raw_fd(), ptr, len, libc::SOCK_CLOEXEC));
            Ok(Stream { fd: OwnedFd::from_raw_fd(fd) })
        }
        #[cfg(not(any(
            target_os = "android",
            target_os = "dragonfly",
            target_os = "freebsd",
            target_os = "illumos",
            target_os = "linux",
            target_os = "hurd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "cygwin",
        )))]
        unsafe {
            let fd = ck_syscall!(libc::accept(self.fd.as_raw_fd(), ptr, len));
            let fd = OwnedFd::from_raw_fd(fd);
            let flags = ck_syscall!(libc::fcntl(self.fd.as_raw_fd(), libc::F_GETFD));
            ck_syscall!(libc::fcntl(self.fd.as_raw_fd(), libc::F_SETFD, flags | libc::FD_CLOEXEC));
            Ok(Stream { fd })
        }
    }

    // Optimisation: don't bother decoding the address if it's not actually needed.
    fn accept_without_address(&self) -> io::Result<Stream> {
        #[cfg(target_family = "unix")]
        { self.accept_maybe_without_address(None) }
        #[cfg(not(target_family = "unix"))]
        { self.accept().map(|socket| socket.0) }
    }

    /// Sets the socket as non-blocking if the argument is `true`.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        self.fd.set_nonblocking(nonblocking)
    }

    /// Attempts to clone the stream by asking the OS to create a new file descriptor.
    ///
    /// The cloned stream points to the same resource, effectively presenting shared mutability.
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self { fd: self.fd.try_clone()? })
    }
}

impl From<std::net::TcpListener> for Listener {
    fn from(value: std::net::TcpListener) -> Self {
        #[cfg(target_family = "unix")]
        { Listener { fd: value.into() } }
        #[cfg(not(target_family = "unix"))]
        { Listener { fd: value } }
    }
}

#[cfg(target_family = "unix")]
impl From<std::os::unix::net::UnixListener> for Listener {
    fn from(value: std::os::unix::net::UnixListener) -> Self {
        Listener { fd: value.into() }
    }
}

#[cfg(target_family = "unix")]
fn convert_af_inet(addr: libc::sockaddr_in) -> std::net::SocketAddr {
    let ip = addr.sin_addr.s_addr.to_ne_bytes();
    let ip = std::net::Ipv4Addr::new(ip[0], ip[1], ip[2], ip[3]);
    let port = u16::from_be(addr.sin_port);
    std::net::SocketAddr::new(ip.into(), port)
}

#[cfg(target_family = "unix")]
fn convert_af_inet6(addr: libc::sockaddr_in6) -> std::net::SocketAddr {
    fn seg(ip: [u8; 16], idx: usize) -> u16 {
        u16::from_be_bytes([ip[idx * 2], ip[idx * 2 + 1]])
    }
    let ip = addr.sin6_addr.s6_addr;
    let ip = std::net::Ipv6Addr::new(seg(ip, 0), seg(ip, 1), seg(ip, 2), seg(ip, 3), seg(ip, 4), seg(ip, 5), seg(ip, 6), seg(ip, 7));
    let port = u16::from_be(addr.sin6_port);
    std::net::SocketAddr::new(ip.into(), port)
}

#[cfg(target_family = "unix")]
fn convert_af_unix(addr: libc::sockaddr_un) -> PathBuf {
    let len = addr.sun_path.iter().position(|&x| x == 0).unwrap_or(addr.sun_path.len());
    let path = OsStr::from_bytes(c_bytes_to_u8(&addr.sun_path[..len]));
    path.to_owned().into()
}

#[cfg(target_family = "unix")]
fn c_bytes_to_u8(slice: &[libc::c_char]) -> &[u8] {
    // SOUNDNESS: `c_char` has the same memory layout as `u8`, we do this in a dedicated function
    // to enforce lifetimes.
    const _: () = {
        assert!(std::mem::size_of::<libc::c_char>() == std::mem::size_of::<u8>());
        assert!(std::mem::align_of::<libc::c_char>() == std::mem::align_of::<u8>());
    };
    unsafe {
        std::slice::from_raw_parts(slice.as_ptr().cast(), slice.len())
    }
}

#[cfg(target_family = "unix")]
trait OwnedFdExt {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()>;
    fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()>;
}

#[cfg(target_family = "unix")]
impl OwnedFdExt for OwnedFd {
    fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let mut nonblocking = nonblocking as libc::c_int;
        // SOUNDNESS: descriptor validity is guaranteed by the invariant, the input values are
        // valid way to set non-blocking flag.
        unsafe { ck_syscall!(libc::ioctl(self.as_raw_fd(), libc::FIONBIO, &mut nonblocking)) };
        Ok(())
    }

    fn shutdown(&self, how: std::net::Shutdown) -> io::Result<()> {
        let how = match how {
            std::net::Shutdown::Read => libc::SHUT_RD,
            std::net::Shutdown::Write => libc::SHUT_WR,
            std::net::Shutdown::Both => libc::SHUT_RDWR,
        };

        // SOUNDNESS: descriptor validity is guaranteed by the invariant.
        unsafe { ck_syscall!(libc::shutdown(self.as_raw_fd(), how)); }
        Ok(())
    }
}

/// An iterator over incoming connections.
///
/// This iterator will never return `None` and is thus infinite. However you probably still want to
/// break the iteration in case of critical errors.
#[must_use = "iterators are lazy and do nothing unless consumed"]
#[derive(Debug)]
pub struct Incoming<'a>(&'a Listener);

impl Iterator for Incoming<'_> {
    type Item = io::Result<Stream>;

    fn next(&mut self) -> Option<Self::Item> {
        Some(self.0.accept_without_address())
    }
}

/// A trait implemented by types that can be resolved to network or unix socket addresses (paths).
///
/// This is equivalent to the trait from `std`, except it uses a different address type.
pub trait ToSocketAddrs {
    /// The type of the iterator returned by `to_socket_addrs`.
    ///
    /// Note that unlike `std` this has GAT, because GAT was not available at the time `std` used
    /// it. However it is not actually used by the library, just provided for whoever needs it.
    type Iter<'a>: Iterator<Item = SocketAddr> where Self: 'a;

    /// Resolves `self` returning an iterator of socket addresses.
    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>>;
}

impl<T: ToSocketAddrs> ToSocketAddrs for &'_ T {
    type Iter<'a> = T::Iter<'a> where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        (*self).to_socket_addrs()
    }
}

delegate_raw_fd!(Listener, fd);

/// Represents a socket address.
///
/// This type can represent either a network address or a (valid) unix socket path.
#[cfg_attr(not(target_family = "unix"), non_exhaustive)]
#[derive(Debug, Clone)]
pub enum SocketAddr {
    /// A network address - IP(4 or 6) and a port.
    Net(std::net::SocketAddr),
    /// A Unix domain socket address - a filesystem path obeying length limit.
    #[cfg(target_family = "unix")]
    Uds(std::os::unix::net::SocketAddr),
}

impl SocketAddr {
    /// Creates a UDS address from given path.
    ///
    /// This is a convenience method avoiding intermediate conversions.
    #[cfg(target_family = "unix")]
    pub fn unix_from_pathname<P: AsRef<Path>>(path: P) -> io::Result<Self> {
        std::os::unix::net::SocketAddr::from_pathname(path).map(SocketAddr::Uds)
    }

    /// Returns the port number or `None` if the address is UDS.
    pub fn port(&self) -> Option<u16> {
        match self {
            Self::Net(addr) => Some(addr.port()),
            #[cfg(target_family = "unix")]
            Self::Uds(_) => None,
        }
    }
    
    /// Constructs `Self` by calling a closure which is supposed to call a syscall using the
    /// arguments.
    ///
    /// # Safety
    ///
    /// The caller must pass a closure that writes valid `CAddr` value (by making a syscall) unless
    /// `Err` is returned. In turn, the this function guarantees that the parameters are
    /// appropriate for syscalls expecting to fill these values.
    #[cfg(target_family = "unix")]
    unsafe fn from_syscall<R>(f: impl FnOnce(&mut MaybeUninit<CAddr>, &mut libc::socklen_t) -> io::Result<R>) -> io::Result<(R, Self)> {
        // Ensure in compile time that our size is sane
        const ADDR_LEN: libc::socklen_t = {
            let len = std::mem::size_of::<CAddr>() as libc::socklen_t;
            assert!(len as usize == std::mem::size_of::<CAddr>());
            len
        };

        let mut addr = MaybeUninit::<CAddr>::uninit();
        let mut addr_len = ADDR_LEN;
        let result = f(&mut addr, &mut addr_len)?;
        // we trust the OS to provide reasonable length but keep this in for debugging
        debug_assert!(addr_len >= std::mem::size_of::<libc::sockaddr>() as libc::socklen_t);
        // To avoid a ton of unsafe-marked code we do most of the stuff in the safe functions.
        let addr = unsafe {
            let addr = addr.assume_init();
            match i32::from(addr.sa.sa_family) {
                libc::AF_INET => SocketAddr::from(convert_af_inet(addr.inet)),
                libc::AF_INET6 => SocketAddr::from(convert_af_inet6(addr.inet6)),
                libc::AF_UNIX => SocketAddr::try_from(convert_af_unix(addr.unix)).expect("OS should return a valid path"),
                _ => return Err(io::Error::new(io::ErrorKind::InvalidInput, "socket is not of a supported type")),
            }
        };
        Ok((result, addr))
    }
}

impl fmt::Display for SocketAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SocketAddr::Net(addr) => fmt::Display::fmt(addr, f),
            #[cfg(target_family = "unix")]
            SocketAddr::Uds(addr) => {
                match addr.as_pathname() {
                    Some(path) => fmt::Display::fmt(&path.display(), f),
                    None => fmt::Display::fmt("unnamed unix socket", f),
                }
            },
        }
    }
}

#[cfg(target_family = "unix")]
#[repr(C)]
union CAddr {
    // We can access this first to check which variant it is
    sa: libc::sockaddr,
    inet: libc::sockaddr_in,
    inet6: libc::sockaddr_in6,
    unix: libc::sockaddr_un,
}

#[cfg(target_family = "unix")]
impl TryFrom<PathBuf> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: PathBuf) -> Result<Self, Self::Error> {
        Self::unix_from_pathname(&value)
    }
}

#[cfg(target_family = "unix")]
impl<'a> TryFrom<&'a PathBuf> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: &'a PathBuf) -> Result<Self, Self::Error> {
        Self::unix_from_pathname(value)
    }
}

#[cfg(target_family = "unix")]
impl<'a> TryFrom<&'a Path> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: &'a Path) -> Result<Self, Self::Error> {
        Self::unix_from_pathname(value)
    }
}

#[cfg(target_family = "unix")]
impl<'a> TryFrom<Cow<'a, Path>> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: Cow<'a, Path>) -> Result<Self, Self::Error> {
        Self::unix_from_pathname(&value)
    }
}

#[cfg(target_family = "unix")]
impl From<std::os::unix::net::SocketAddr> for SocketAddr {
    fn from(value: std::os::unix::net::SocketAddr) -> Self {
        SocketAddr::Uds(value)
    }
}

impl From<std::net::SocketAddr> for SocketAddr {
    fn from(value: std::net::SocketAddr) -> Self {
        SocketAddr::Net(value)
    }
}

impl From<std::net::SocketAddrV4> for SocketAddr {
    fn from(value: std::net::SocketAddrV4) -> Self {
        SocketAddr::Net(value.into())
    }
}

impl From<std::net::SocketAddrV6> for SocketAddr {
    fn from(value: std::net::SocketAddrV6) -> Self {
        SocketAddr::Net(value.into())
    }
}

impl From<([u8; 4], u16)> for SocketAddr {
    fn from(value: ([u8; 4], u16)) -> Self {
        SocketAddr::Net(value.into())
    }
}

impl std::str::FromStr for SocketAddr {
    type Err = std::io::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        SocketAddr::try_from(s)
    }
}

#[cfg(target_family = "unix")]
const UNIX_PREFIX: &str = "unix:";

/// Treats the string as Unix socket path if it begins with "/", "./", or "unix:", parses as
/// network address otherwise.
impl TryFrom<String> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

/// Treats the string as Unix socket path if it begins with "/", "./", or "unix:", parses as
/// network address otherwise.
impl<'a> TryFrom<&'a str> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: &'a str) -> Result<Self, Self::Error> {
        #[cfg(target_family = "unix")]
        if value.starts_with('/') || value.starts_with("./") {
            Path::new(value).try_into()
        } else if value.starts_with(UNIX_PREFIX) {
            Path::new(&value[UNIX_PREFIX.len()..]).try_into()
        } else {
            value.parse().map(SocketAddr::Net).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
        }
        #[cfg(not(target_family = "unix"))]
        value.parse().map(SocketAddr::Net).map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
    }
}

/// Treats the string as Unix socket path if it begins with "/", "./", or "unix:", parses as
/// network address otherwise.
impl<'a> TryFrom<&'a String> for SocketAddr {
    type Error = io::Error;

    fn try_from(value: &'a String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

/// Treats the string as Unix socket path if it begins with "/", "./", or "unix:", parses as
/// network address otherwise.
impl TryFrom<OsString> for SocketAddr {
    type Error = AddrParseError;

    fn try_from(value: OsString) -> Result<Self, Self::Error> {
        value.as_os_str().try_into()
    }
}

/// Treats the string as Unix socket path if it begins with "/", "./", or "unix:", parses as
/// network address otherwise.
impl<'a> TryFrom<&'a OsStr> for SocketAddr {
    type Error = AddrParseError;

    fn try_from(value: &'a OsStr) -> Result<Self, Self::Error> {
        #[cfg(target_family = "unix")]
        if value.as_bytes().starts_with(b"/") || value.as_bytes().starts_with(b"./") {
            Path::new(value).try_into().map_err(Into::into)
        } else if value.as_bytes().starts_with(UNIX_PREFIX.as_bytes()) {
            Path::new(OsStr::from_bytes(&value.as_bytes()[UNIX_PREFIX.len()..])).try_into().map_err(Into::into)
        } else {
            value
                .to_str()
                .ok_or(AddrParseError::NotUtf8)?
                .parse()
                .map(SocketAddr::Net)
                .map_err(Into::into)
        }
        #[cfg(not(target_family = "unix"))]
        value
            .to_str()
            .ok_or(AddrParseError::NotUtf8)?
            .parse()
            .map(SocketAddr::Net)
            .map_err(Into::into)
    }
}

impl<'a> TryFrom<&'a OsString> for SocketAddr {
    type Error = AddrParseError;

    fn try_from(value: &'a OsString) -> Result<Self, Self::Error> {
        value.as_os_str().try_into()
    }
}

/// Error returned when attempting to parse `OsStr` as socket address.
#[derive(Debug)]
pub enum AddrParseError {
    /// The given `OsStr` was not valid UTF-8.
    NotUtf8,
    /// The address is invalid for reason other than UTF-8 encoding.
    InvalidAddr(io::Error),
}

impl fmt::Display for AddrParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            AddrParseError::NotUtf8 => fmt::Display::fmt("input is not UTF-8", f),
            AddrParseError::InvalidAddr(_) => fmt::Display::fmt("invalid network address", f),
        }
    }
}

impl std::error::Error for AddrParseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AddrParseError::NotUtf8 => None,
            AddrParseError::InvalidAddr(error) => Some(error),
        }
    }
}

impl From<std::net::AddrParseError> for AddrParseError {
    fn from(value: std::net::AddrParseError) -> Self {
        AddrParseError::InvalidAddr(io::Error::new(io::ErrorKind::InvalidInput, value))
    }
}

impl From<io::Error> for AddrParseError {
    fn from(value: io::Error) -> Self {
        AddrParseError::InvalidAddr(value)
    }
}

impl From<AddrParseError> for io::Error {
    fn from(value: AddrParseError) -> Self {
        io::Error::new(io::ErrorKind::InvalidInput, value)
    }
}

/// Iterator over resolved addresses.
pub struct ResolvedAddrs(ResolvedAddrsInner);

enum ResolvedAddrsInner {
    Net(std::vec::IntoIter<std::net::SocketAddr>),
    #[cfg(target_family = "unix")]
    Uds(std::iter::Once<std::os::unix::net::SocketAddr>),
}

impl Iterator for ResolvedAddrs {
    type Item = SocketAddr;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            ResolvedAddrsInner::Net(iter) => iter.next().map(Into::into),
            #[cfg(target_family = "unix")]
            ResolvedAddrsInner::Uds(iter) => iter.next().map(Into::into),
        }
    }
}

impl DoubleEndedIterator for ResolvedAddrs {
    fn next_back(&mut self) -> Option<Self::Item> {
        match &mut self.0 {
            ResolvedAddrsInner::Net(iter) => iter.next_back().map(Into::into),
            #[cfg(target_family = "unix")]
            ResolvedAddrsInner::Uds(iter) => iter.next_back().map(Into::into),
        }
    }
}

impl ExactSizeIterator for ResolvedAddrs {}
impl std::iter::FusedIterator for ResolvedAddrs {}

#[cfg(target_family = "unix")]
impl ToSocketAddrs for PathBuf {
    type Iter<'a> = std::iter::Once<SocketAddr> where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        Ok(std::iter::once(self.try_into()?))
    }
}

#[cfg(target_family = "unix")]
impl ToSocketAddrs for Path {
    type Iter<'a> = std::iter::Once<SocketAddr> where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        Ok(std::iter::once(self.try_into()?))
    }
}

impl ToSocketAddrs for String {
    type Iter<'a> = ResolvedAddrs where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        (**self).to_socket_addrs()
    }
}

impl ToSocketAddrs for str {
    type Iter<'a> = ResolvedAddrs where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        #[cfg(target_family = "unix")]
        let inner = if let Some(address) = self.strip_prefix(UNIX_PREFIX) {
            ResolvedAddrsInner::Uds(std::iter::once(std::os::unix::net::SocketAddr::from_pathname(address)?))
        } else if self.starts_with("/") || self.starts_with("./") {
            ResolvedAddrsInner::Uds(std::iter::once(std::os::unix::net::SocketAddr::from_pathname(self)?))
        } else {
            ResolvedAddrsInner::Net(std::net::ToSocketAddrs::to_socket_addrs(self)?)
        };

        #[cfg(not(target_family = "unix"))]
        let inner = ResolvedAddrsInner::Net(std::net::ToSocketAddrs::to_socket_addrs(self)?);

        Ok(ResolvedAddrs(inner))
    }
}

impl ToSocketAddrs for OsString {
    type Iter<'a> = ResolvedAddrs where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        (**self).to_socket_addrs()
    }
}

impl ToSocketAddrs for OsStr {
    type Iter<'a> = ResolvedAddrs where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        #[cfg(target_family = "unix")]
        let inner = if let Some(address) = self.strip_prefix(UNIX_PREFIX) {
            ResolvedAddrsInner::Uds(std::iter::once(std::os::unix::net::SocketAddr::from_pathname(address)?))
        } else if self.starts_with("/") || self.starts_with("./") {
            ResolvedAddrsInner::Uds(std::iter::once(std::os::unix::net::SocketAddr::from_pathname(self)?))
        } else {
            let utf8 = self
                .to_str()
                .ok_or(io::Error::new(io::ErrorKind::InvalidInput, "the address is not UTF-8"))?;
            ResolvedAddrsInner::Net(std::net::ToSocketAddrs::to_socket_addrs(utf8)?)
        };
        #[cfg(not(target_family = "unix"))]
        let inner = {
            let utf8 = self
                .to_str()
                .ok_or(io::Error::new(io::ErrorKind::InvalidInput, "the address is not UTF-8"))?;
            ResolvedAddrsInner::Net(std::net::ToSocketAddrs::to_socket_addrs(utf8)?)
        };

        Ok(ResolvedAddrs(inner))
    }
}

#[cfg(target_family = "unix")]
trait OsStrExt {
    fn starts_with(&self, s: &str) -> bool;
    fn strip_prefix(&self, s: &str) -> Option<&OsStr>;
}

#[cfg(target_family = "unix")]
impl OsStrExt for OsStr {
    fn starts_with(&self, s: &str) -> bool {
        self.as_bytes().starts_with(s.as_ref())
    }

    fn strip_prefix(&self, s: &str) -> Option<&OsStr> {
        if self.starts_with(s) {
            Some(OsStr::from_bytes(&self.as_bytes()[s.len()..]))
        } else {
            None
        }
    }
}

impl ToSocketAddrs for SocketAddr {
    type Iter<'a> = std::iter::Once<SocketAddr> where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        Ok(std::iter::once(self.clone()))
    }
}

impl ToSocketAddrs for std::net::SocketAddr {
    type Iter<'a> = std::iter::Once<SocketAddr> where Self: 'a;

    fn to_socket_addrs(&self) -> io::Result<Self::Iter<'_>> {
        Ok(std::iter::once((*self).into()))
    }
}

#[cfg(feature = "parse_arg")]
impl parse_arg::ParseArgFromStr for SocketAddr {
    fn describe_type<W: fmt::Write>(mut writer: W) -> fmt::Result {
        std::net::SocketAddr::describe_type(&mut writer)?;
        write!(writer, " or an absolute Unix path or a relative Unix path starting with `./` or `unix:` followed by a Unix path")
    }
}

#[cfg(feature = "serde")]
impl<'de> serde::Deserialize<'de> for SocketAddr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = SocketAddr;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "a version 4 or 6 network socket address (IP:port) or an absolute Unix path or a relative Unix path starting with `./` or `unix:` followed by a Unix path")
            }

            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Self::Value, E> {
                s.parse().map_err(|_| E::invalid_value(serde::de::Unexpected::Str(s), &"a version 4 or 6 network socket address (IP:port) or an absolute Unix path or a relative Unix path starting with `./` or `unix:` followed by a Unix path"))
            }
        }

        deserializer.deserialize_str(Visitor)
    }
}

/// Wrappers compatible with the `async-std` ecosystem.
///
/// **Note that this is untested and unfinished!** It compiles and that's it.
#[cfg(feature = "async-io")]
pub mod async_std {
    use async_io::Async;
    use futures_lite::{ready, AsyncRead, AsyncWrite};
    use std::io;
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};
    use super::SocketAddr;

    // SOUNDNESS: We are not dropping stream in `Read` and `Write` impls.
    unsafe impl async_io::IoSafe  for super::Stream {}

    /// An asynchronous version of abstract listening socket.
    ///
    /// See the documentation of [`super::Listener`], this only differs in supporting `async`.
    pub struct Listener {
        // Inspired by async_std
        // I have no idea why Arc is used there but I better use it here as well for consistency
        inner: Arc<Async<super::Listener>>,
    }

    impl Listener {
        /// Accepts the incoming connection.
        ///
        /// The returned future will await until a client connects.
        ///
        /// See the documentation of [`super::Listener::accept`], this only differs in supporting `async`.
        pub async fn accept(&self) -> io::Result<(Stream, SocketAddr)> {
            let (stream, addr) = self.inner.read_with(|listener| listener.accept()).await?;
            Ok((stream.try_into()?, addr))
        }
    }

    /// An asynchronous version of abstract byte stream socket.
    ///
    /// See the documentation of [`super::Stream`], this only differs in supporting `async`.
    pub struct Stream {
        inner: Arc<Async<super::Stream>>,
        readable: Option<async_io::ReadableOwned<super::Stream>>,
        writable: Option<async_io::WritableOwned<super::Stream>>,
    }

    impl Stream {
        //pub fn connect
    }

    impl AsyncRead for Stream {
        fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
            use std::future::Future;
            use std::io::Read;

            loop {
                match self.inner.get_ref().read(buf) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {},
                    result => {
                        self.readable = None;
                        return Poll::Ready(result);
                    },
                }

                // The Deref trait on pin is making it impossible to make distinct borrows using
                // simply `self.field`, so we have to pre-deref it here.
                let this = &mut *self;
                let inner = &this.inner;
                let f = this.readable.get_or_insert_with(|| inner.clone().readable_owned());
                let result = ready!(Pin::new(f).poll(cx));
                self.readable = None;
                result?;
            }
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<io::Result<usize>> {
            use std::future::Future;
            use std::io::Write;

            loop {
                match self.inner.get_ref().write(buf) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {},
                    result => {
                        self.writable = None;
                        return Poll::Ready(result);
                    },
                }

                // The Deref trait on pin is making it impossible to make distinct borrows using
                // simply `self.field`, so we have to pre-deref it here.
                let this = &mut *self;
                let inner = &this.inner;
                let f = this.writable.get_or_insert_with(|| inner.clone().writable_owned());
                let result = ready!(Pin::new(f).poll(cx));
                self.writable = None;
                result?;
            }
        }

        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            use std::future::Future;
            use std::io::Write;

            loop {
                match self.inner.get_ref().flush() {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {},
                    result => {
                        self.writable = None;
                        return Poll::Ready(result);
                    },
                }

                // The Deref trait on pin is making it impossible to make distinct borrows using
                // simply `self.field`, so we have to pre-deref it here.
                let this = &mut *self;
                let inner = &this.inner;
                let f = this.writable.get_or_insert_with(|| inner.clone().writable_owned());
                let result = ready!(Pin::new(f).poll(cx));
                self.writable = None;
                result?;
            }
        }

        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(self.inner.get_ref().shutdown(std::net::Shutdown::Write))
        }
    }

    impl std::panic::UnwindSafe for Stream {}
    impl std::panic::RefUnwindSafe for Stream {}

    impl TryFrom<super::Stream> for Stream {
        type Error = io::Error;

        fn try_from(value: super::Stream) -> Result<Self, Self::Error> {
            Ok(Stream {
                inner: Arc::new(Async::new(value)?),
                readable: None,
                writable: None,
            })
        }
    }
}
