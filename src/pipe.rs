// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2021 Tanner Parnell (upstream, TanninOne/nxmproxy)
// Copyright (C) 2026 The GameModManager community (this fork)
// Fork of https://github.com/TanninOne/nxmproxy - see NOTICE.md

//! Optional fast path: hand the link to an already running manager over a local
//! socket, instead of paying for a cold start.
//!
//! **Inert until a manager is configured.** Nothing here runs, and nothing is
//! created, unless a registered manager has a pipe set - so a user who has not
//! configured one sees no change at all.
//!
//! Kept from upstream, with the POSIX arm upstream never had. Upstream's
//! implementation is `File::create(r"\\.\pipe\{name}")`, which is correct on
//! Windows and wrong everywhere else: on POSIX that is not a special path, it
//! is a **relative file name**, so every forwarded link on Linux or macOS would
//! drop a stray file called `\\.\pipe\<name>` into whatever directory the
//! desktop happened to launch us from.
//!
//! ## Why a Unix domain socket and not a FIFO
//!
//! A named FIFO looks like the obvious substitute and is the one thing that
//! must not be used. `open(O_WRONLY)` on a FIFO **blocks until a reader opens
//! it**. A FIFO pointed at a manager that is not running therefore hangs the
//! OS's invocation of nxmproxy forever, once per clicked link, and the user
//! gets a dialog that never dismisses. There is no timeout that fixes this,
//! because the block is inside the kernel's open call.
//!
//! `connect(2)` to a Unix domain socket cannot block on an absent peer. An
//! absent path is `ENOENT`; a path that exists with nobody listening is
//! `ECONNREFUSED`. Both are immediate, both are errors, and both mean the same
//! thing to the caller: not running, spawn it instead.
//!
//! That also buys liveness for free. A manager that crashed leaves its socket
//! file behind, because closing a listening socket does not unlink it. The
//! stale file reports `ECONNREFUSED`, which reads exactly as "not running", so
//! the fallback works without a heartbeat, a PID file, or an age check. A FIFO
//! cannot tell a dead manager from one that has not started yet, and so cannot
//! fall back at all.
//!
//! ## What is on the wire
//!
//! One write, then close. No length prefix, no framing, no terminator, no
//! acknowledgement, and **no read timeout** - the same fire-and-forget shape MO2
//! uses (`multiprocess.cpp`: `m_Server.listen(s_Key)`, then
//! `socket.connectToServer(s_Key, QIODevice::WriteOnly)`, `socket.write(url)`,
//! `socket.disconnectFromServer()`). Dropping the stream closes it, which is
//! the last of those four.
//!
//! ## Finding the manager
//!
//! The configured address **is** the discovery. There is no discovery file, no
//! environment variable and no registry key to read: a manager that wants the
//! fast path is a listener at an address the user has already put in the
//! configuration.
//!
//! README "Delivering to an already-running manager" is the contract,
//! versioned, for anyone implementing a listener against this.

#[cfg(windows)]
fn write_pipe(pipe: &str, value: &str) -> std::io::Result<usize> {
    // The `Write` import is deliberately scoped to this arm. The import that
    // existed in upstream was at file scope, where the non-Windows arm no
    // longer needs it, and a Linux build cannot see this line at all - so a
    // missing import here would only ever have been caught by a Windows CI run.
    use std::io::Write;
    std::fs::File::create(format!(r"\\.\pipe\{pipe}"))?.write(value.as_bytes())
}

/// Deliver to a Unix domain socket.
///
/// Every failure is an `io::Error` and every one of them means the same thing
/// to the caller, which is what makes this safe to attempt on every single
/// link: absent socket, dead manager, full disk, unwritable path, a path longer
/// than `sun_path` allows. None of them wait for anything.
///
/// Note what this does **not** do: it never creates the socket file. That
/// belongs to the listener, and a file we created would read as "listening" to
/// every future attempt while nothing was there.
#[cfg(unix)]
fn write_pipe(pipe: &str, value: &str) -> std::io::Result<usize> {
    use std::io::Write;
    let mut stream = std::os::unix::net::UnixStream::connect(pipe)?;
    // `write_all`, not `write`. A short write hands the listener a truncated
    // url, and a truncated url is the one failure the listener cannot detect -
    // it is a different download, not a missing one.
    stream.write_all(value.as_bytes())?;
    // Dropping `stream` closes the socket, which is how the listener learns the
    // message ended. There is no shutdown call to make: on a Unix stream there
    // is nothing between the bytes and the fd.
    Ok(value.len())
}

/// Try to deliver the link to a listening manager.
///
/// `false` means "not delivered", which is **not an error**: the manager is
/// most likely simply not running, and the caller falls back to spawning it.
/// That is the whole safety argument for doing this once per link - a failed
/// attempt is ordinary, cheap and invisible to the user.
pub fn try_write_pipe(pipe: &str, value: &str) -> bool {
    write_pipe(pipe, value).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(not(windows))]
    #[test]
    fn a_failed_pipe_never_creates_a_file() {
        let dir = std::env::temp_dir().join(format!("nxmproxy-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let previous = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        assert!(!try_write_pipe("vortex_download", "nxm://g/mods/1"));
        std::env::set_current_dir(previous).unwrap();
        assert_eq!(
            std::fs::read_dir(&dir).unwrap().count(),
            0,
            "a failed pipe write must not leave files behind"
        );
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn an_unknown_pipe_is_not_fatal() {
        assert!(!try_write_pipe(
            "nxmproxy-no-such-pipe-test",
            "nxm://g/mods/1"
        ));
    }

    // -----------------------------------------------------------------------
    // The three tests this path exists for, and the only three.
    //
    // Everything here is a failure the OS runs once per clicked link, so the
    // consequences are severe rather than cosmetic: handing a link to the wrong
    // application, downloading the wrong thing, or hanging the desktop's
    // handler forever. Output shape and JSON field names are not defended here
    // on purpose - they are not worth a test.
    // -----------------------------------------------------------------------

    /// A short path for a real listening socket.
    ///
    /// Two constraints a test cannot ignore. `sun_path` is 108 bytes on Linux
    /// and macOS's temp directory is already about 55 of them, so a name built
    /// from a per-test subdirectory or a long tag does not fit and fails with
    /// `ENAMETOOLONG` - which would look exactly like "no listener" and turn
    /// this into a test that passes for the wrong reason. And two tests must not
    /// share a path, because a second `bind` on one path is an error. Hence:
    /// the temp directory and a short name, never a subdirectory.
    #[cfg(unix)]
    fn socket_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!("nxmproxy-{}-{tag}.sock", std::process::id()))
    }

    /// Time an attempt and hand the elapsed time back, refusing to report a
    /// hang as a failure instead of a hang.
    ///
    /// The bound is deliberately enormous. There is nothing here that should
    /// take even milliseconds; the only way this trips is the failure mode that
    /// actually matters, which is a block that never returns. A tighter bound
    /// would just be a flaky test on a loaded machine.
    #[cfg(unix)]
    fn timed(pipe: &str, url: &str) -> (bool, std::time::Duration) {
        let start = std::time::Instant::now();
        let delivered = try_write_pipe(pipe, url);
        let elapsed = start.elapsed();
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "the delivery attempt took {elapsed:?}, so it blocked rather than \
             failing; a per-URI handler that blocks wedges the desktop"
        );
        (delivered, elapsed)
    }

    /// 1. A listener that is running receives the url, byte for byte.
    ///
    /// Routing a link to the wrong thing is the worst outcome this crate has,
    /// so the bytes are the assertion: the same url, unmodified, as the only
    /// thing on the wire. The url here deliberately contains a quote, a space
    /// and a `&`, because those are exactly the characters a framing or
    /// escaping mistake mangles, and a mangled link downloads the wrong mod
    /// rather than failing.
    ///
    /// The listener is the peer nxmproxy would find in production - a real
    /// `UnixListener` at the configured address - not a mock of one.
    #[cfg(unix)]
    #[test]
    fn a_running_listener_receives_the_url_byte_for_byte() {
        let path = socket_path("alive");
        let url = "nxm://somegame/mods/1/files/2?a=\"b\"&c='d' e f";
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        // Read to end: the stream nxmproxy opens is dropped when it returns, so
        // EOF is the message terminator and this cannot block waiting for one.
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut received = Vec::new();
            std::io::Read::read_to_end(&mut stream, &mut received).unwrap();
            received
        });

        assert!(
            try_write_pipe(path.to_str().unwrap(), url),
            "a listening manager must be reachable at the configured address"
        );
        assert_eq!(
            peer.join().unwrap(),
            url.as_bytes(),
            "the listener must receive the url unmodified, as the only bytes on the wire"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 3. A manager that dies does not wedge nxmproxy.
    ///
    /// The OS runs this once per clicked link, so a delivery attempt that waits
    /// is the severe failure: a hung process per click and a dialog that never
    /// dismisses. Two properties, both checked, neither of which a FIFO can
    /// offer.
    ///
    /// * **A crashed manager leaves a stale socket file, and it reads as "not
    ///   running".** Closing a listening socket does not unlink its file, so a
    ///   manager that dies mid-session leaves a path that looks configured
    ///   forever. `connect` reports `ECONNREFUSED`, immediately, and the caller
    ///   falls back - with no heartbeat, PID file or age check to make that
    ///   true. This is the case that matters, and it is the reason the address
    ///   is a socket.
    /// * **A peer that closes mid-write is an error, not a signal.** SIGPIPE's
    ///   default action is to kill the process; Rust's runtime ignores it before
    ///   `main`, which is what turns this into an `Err`. Pinned here because
    ///   nothing else in the suite would notice that changing, and the
    ///   consequence would be one crash per link.
    #[cfg(unix)]
    #[test]
    fn a_listener_that_dies_mid_flight_does_not_wedge_us() {
        use std::io::Write;
        let path = socket_path("died");
        let url = "nxm://g/mods/1";
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();

        // The manager accepts a connection and then shuts down without reading
        // it. Its `accept` is what releases the main thread below, so the two
        // cannot deadlock and the ordering cannot be a race.
        let dying = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            drop(stream);
            drop(listener);
        });

        // The connection that is in flight when the manager dies. Only its
        // duration is asserted: whether this particular write wins the race
        // against the close is genuinely undecidable, and both outcomes are
        // correct - a link buffered for a process that has just died is a link
        // lost, but that is MO2's behaviour too and no caller can detect it.
        let (delivered, in_flight) = timed(path.to_str().unwrap(), url);
        dying.join().unwrap();
        assert!(path.exists(), "a closed socket leaves its file behind");

        // Nothing is listening now. This is the state a crashed manager leaves
        // the user in, and it has to be indistinguishable from "never
        // configured" to the caller.
        let (delivered_after, after_crash) = timed(path.to_str().unwrap(), url);
        assert!(
            !delivered_after,
            "a stale socket file must read as 'not running' so the caller spawns"
        );
        let _ = delivered;
        let _ = std::fs::remove_file(&path);

        // The write itself, at a peer that is already gone. Held here rather
        // than driven through `try_write_pipe` because that function connects
        // and writes in one turn, so a peer cannot be made to close inside the
        // gap deterministically - and this is the half that decides whether the
        // process dies or returns.
        let (ours, theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(theirs);
        let mut ours = ours;
        assert!(
            ours.write_all(url.as_bytes()).is_err(),
            "writing to a closed peer must be an error: a SIGPIPE kill would take \
             down the OS's handler for this link and every link after it"
        );

        eprintln!(
            "no listener: mid-flight attempt {in_flight:?}, stale socket file \
             {after_crash:?} - both immediate, both a fallback to spawn"
        );
    }
}
