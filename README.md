# Abstractions over TCP and Unix sockets.

**This crate is in development and wasn't widely tested yet.**
It may also lack some functionality. I'll be happy to accept contributions.

This crate helps you write applications that support both TCP and Unix sockets by providing
mostly-drop-in replacements for the respective types. If `target_family` is not `unix` the unix
socket support transparently disappears unless you're manually inspecting the socket address.
This ensures your application is multi-platform out of the box.

The crate currently only supports streams, not datagrams.

To add Unix support to an existing application supporting TCP, follow these steps:

* Replace `TcpStream` (or `UnixStream`) with `Stream`
* Replace `TcpListener` (or `UnixListener`) with `Listener`
* Replace `SocketAddr` from `std` with `SocketAddr`
* Fix remaining type issues, if any. There shouldn't be many.

## Feature flags:

`async-io` - uses the `async-io` crate to provide an `async-std`-compatible wrapper. Note that this wasn't tested in practice because I mistakenly thought I need this but I dicovered I don't just before testing it, so I just kept it in.

## MSRV

The current MSRV is 1.70. The policy is to at least support the latest Debian stable release and not bump the version for cosmetic changes.

## License

MITNFA
