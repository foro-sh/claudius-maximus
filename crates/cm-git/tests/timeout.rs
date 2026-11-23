//! A server that accepts a connection and never answers must fail a sync
//! instead of blocking it forever (foro-sh/claudius-maximus#28).
//!
//! Its own test binary with this single test: the timeouts are libgit2
//! process globals, so nothing else here may touch libgit2.
use std::net::TcpListener;
use std::time::{Duration, Instant};

use cm_git::{Git2Ops, GitOps, set_network_timeouts};

#[test]
fn a_server_that_never_answers_fails_the_sync() {
    set_network_timeouts(Duration::from_secs(1), Duration::from_secs(1)).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    // Accept, hold the sockets open, and never write a byte.
    let _holder = std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            held.push(stream);
        }
    });

    let dir = tempfile::tempdir().unwrap();
    let started = Instant::now();
    let result = Git2Ops.sync_default(
        &dir.path().join("clone"),
        &format!("http://127.0.0.1:{port}/x.git"),
        "token",
    );

    assert!(result.is_err(), "expected a timeout, got {result:?}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "took {:?} to give up",
        started.elapsed()
    );
}
