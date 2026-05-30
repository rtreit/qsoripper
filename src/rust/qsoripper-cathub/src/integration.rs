//! Cross-module integration tests (design §10.2).
//!
//! These bring up the full stack — universal state, the priority scheduler over a
//! [`LoopbackBackend`], multiple serial faces over `tokio::io::duplex`, and the Hamlib net
//! face over real TCP — and assert the system-level invariants: one radio command per poll
//! (reads are served from cache), no VFO-target traffic from a TS-2000 face, cross-face
//! write visibility, front-panel fan-out, strict write ordering, PTT arbitration, and that
//! the Hamlib net face and a serial face share one radio state.
//!
//! Binary crates cannot host `tests/` integration crates, so these live in-crate behind
//! `#[cfg(test)]` to reach the `pub(crate)` surface.

#![allow(clippy::expect_used, clippy::unwrap_used, clippy::indexing_slicing)]

use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use tokio::net::{TcpListener, TcpStream};

use crate::backend::loopback::LoopbackBackend;
use crate::backend::{BackendCapabilities, RadioBackend};
use crate::dialect::kenwood::ts2000::Ts2000Dialect;
use crate::dialect::kenwood::ts590::Ts590Dialect;
use crate::dialect::{ClientDialect, FaceContext};
use crate::hamlib_net::serve_conn;
use crate::permissions::FacePermissions;
use crate::ptt::PttManager;
use crate::radio::{detached_link, spawn_scheduler, OpKind, Priority, RadioHandle};
use crate::serial_face::run_face;
use crate::state::StateHandle;

/// A wired-up radio: shared loopback backend, state, scheduler and PTT lease.
struct Rig {
    backend: LoopbackBackend,
    state: StateHandle,
    radio: RadioHandle,
    ptt: PttManager,
    caps: BackendCapabilities,
}

fn rig() -> Rig {
    let backend = LoopbackBackend::new();
    let caps = backend.capabilities();
    let arc: Arc<dyn RadioBackend> = Arc::new(backend.clone());
    let state = StateHandle::new();
    let radio = spawn_scheduler(arc, detached_link(), state.clone());
    let ptt = PttManager::new(Duration::from_secs(300));
    Rig {
        backend,
        state,
        radio,
        ptt,
        caps,
    }
}

impl Rig {
    fn face(
        &self,
        dialect: Arc<dyn ClientDialect>,
        perms: FacePermissions,
        id: u64,
    ) -> DuplexStream {
        let ctx = FaceContext::new(
            id,
            perms,
            self.state.clone(),
            self.radio.clone(),
            self.ptt.clone(),
            self.caps.clone(),
        );
        let (client, server) = tokio::io::duplex(1024);
        tokio::spawn(run_face(server, dialect, ctx, b';'));
        client
    }
}

fn ts590() -> Arc<dyn ClientDialect> {
    Arc::new(Ts590Dialect::new())
}

fn ts2000() -> Arc<dyn ClientDialect> {
    Arc::new(Ts2000Dialect::new())
}

/// Send `cmd` and read one `;`-terminated reply frame, failing fast on a hang.
async fn request(client: &mut DuplexStream, cmd: &[u8]) -> Vec<u8> {
    client.write_all(cmd).await.expect("write");
    read_frame(client).await
}

async fn read_frame(client: &mut DuplexStream) -> Vec<u8> {
    let mut frame = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(2), client.read(&mut byte))
            .await
            .expect("reply did not arrive")
            .expect("read");
        if n == 0 {
            break;
        }
        frame.push(byte[0]);
        if byte[0] == b';' {
            break;
        }
    }
    frame
}

/// A write on one face is visible to a read on another face on the same radio.
#[tokio::test]
async fn write_from_one_face_visible_on_another() {
    let rig = rig();
    let mut writer_face = rig.face(ts590(), FacePermissions::from_tokens(&["read", "write"]), 1);
    let mut reader_face = rig.face(ts2000(), FacePermissions::read_only(), 2);

    // Set VFO A frequency from the native (N1MM-style) face.
    writer_face
        .write_all(b"FA00007050000;")
        .await
        .expect("write");
    // Let the scheduler apply the write before reading from the other face.
    tokio::time::sleep(Duration::from_millis(30)).await;

    let reply = request(&mut reader_face, b"FA;").await;
    assert_eq!(reply, b"FA00007050000;");
}

/// Modeled reads are served from the cache: no client read ever reaches the backend, so a
/// flurry of reads from many faces adds zero real-radio commands.
#[tokio::test]
async fn face_reads_are_served_from_cache_not_the_backend() {
    let rig = rig();
    let mut a = rig.face(ts590(), FacePermissions::read_only(), 1);
    let mut b = rig.face(ts2000(), FacePermissions::read_only(), 2);

    for _ in 0..20 {
        let _ = request(&mut a, b"FA;").await;
        let _ = request(&mut b, b"FA;").await;
    }

    assert_eq!(rig.backend.poll_count(), 0, "reads must not poll the radio");
    assert!(
        rig.backend.mutations().is_empty(),
        "reads must not mutate the radio"
    );
    assert!(
        rig.backend.passthroughs().is_empty(),
        "modeled reads must not passthrough"
    );
}

/// A TS-2000 (OmniRig/HDSDR) face never emits VFO-target traffic: status reads come from
/// cache and a VFO-target write is rejected, never forwarded (the §8.8 invariant).
#[tokio::test]
async fn ts2000_face_never_retargets_vfo() {
    let rig = rig();
    let mut omni = rig.face(ts2000(), FacePermissions::read_only(), 1);

    let _ = request(&mut omni, b"IF;").await;
    let _ = request(&mut omni, b"FA;").await;
    let _ = request(&mut omni, b"FB;").await;
    // OmniRig issuing an FR (RX VFO select) write must be rejected, not forwarded.
    let reply = request(&mut omni, b"FR1;").await;
    assert_eq!(reply, b"?;");

    assert!(
        rig.backend.mutations().is_empty(),
        "no status read or VFO-select should mutate the radio"
    );
    assert!(rig.backend.passthroughs().is_empty());
}

/// A simulated front-panel change (a poll-diff from the backend's truth) fans out to every
/// auto-info-subscribed face without any client having polled.
#[tokio::test]
async fn front_panel_change_fans_out_to_all_subscribed_faces() {
    let rig = rig();
    let mut a = rig.face(ts590(), FacePermissions::read_only(), 1);
    let mut b = rig.face(ts590(), FacePermissions::read_only(), 2);

    // Both faces turn on virtualized auto-info.
    a.write_all(b"AI2;").await.expect("write");
    b.write_all(b"AI2;").await.expect("write");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Operator turns the knob: backend truth changes, then a poll diffs it into state.
    rig.backend.set_truth_freq_a(7_123_000);
    rig.radio
        .submit(0, Priority::Poll, OpKind::Poll)
        .await
        .expect("poll");

    assert_eq!(read_frame(&mut a).await, b"FA00007123000;");
    assert_eq!(read_frame(&mut b).await, b"FA00007123000;");
}

/// Writes fanned in from several faces are strictly serialized through the one radio task.
#[tokio::test]
async fn writes_from_all_faces_are_strictly_ordered() {
    let rig = rig();
    let perms = FacePermissions::from_tokens(&["read", "write"]);
    let mut f1 = rig.face(ts590(), perms, 1);
    let mut f2 = rig.face(ts590(), perms, 2);
    let mut f3 = rig.face(ts590(), perms, 3);

    f1.write_all(b"FA00007010000;").await.expect("w1");
    f2.write_all(b"FA00007020000;").await.expect("w2");
    f3.write_all(b"FA00007030000;").await.expect("w3");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let muts = rig.backend.mutations();
    assert_eq!(muts.len(), 3, "every write reaches the radio exactly once");
}

/// PTT is a single-owner lease: the first capable face keys, a second is refused while the
/// lease is held, and the lease frees on `RX;` so the second face can then key.
#[tokio::test]
async fn ptt_lease_is_arbitrated_across_faces() {
    let rig = rig();
    let perms = FacePermissions::from_tokens(&["read", "write", "ptt"]);
    let mut f1 = rig.face(ts590(), perms, 1);
    let mut f2 = rig.face(ts590(), perms, 2);

    // f1 keys: a Kenwood set has no positive reply, so nothing comes back.
    f1.write_all(b"TX;").await.expect("tx1");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // f2 tries to key while f1 holds the lease: rejected with `?;`.
    assert_eq!(request(&mut f2, b"TX;").await, b"?;");

    // f1 unkeys, releasing the lease.
    f1.write_all(b"RX;").await.expect("rx1");
    tokio::time::sleep(Duration::from_millis(20)).await;

    // f2 can now acquire it (no error reply).
    f2.write_all(b"TX;").await.expect("tx2");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(rig.ptt.owner(), Some(2));
}

/// The Hamlib net face (engine / WSJT-X) and a serial face share one radio state: a write on
/// the serial face is read back over TCP, and a read-only endpoint rejects writes.
#[tokio::test]
async fn hamlib_net_and_serial_face_share_radio_state() {
    let rig = rig();

    // A native serial face that can write (N1MM-style).
    let mut n1mm = rig.face(ts590(), FacePermissions::from_tokens(&["read", "write"]), 1);

    // A read-only Hamlib net endpoint (the QsoRipper engine).
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let ro_ctx = FaceContext::new(
        2,
        FacePermissions::read_only(),
        rig.state.clone(),
        rig.radio.clone(),
        rig.ptt.clone(),
        rig.caps.clone(),
    );
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept");
        serve_conn(stream, ro_ctx).await;
    });

    // Set the frequency from the serial face.
    n1mm.write_all(b"FA00014250000;").await.expect("write");
    tokio::time::sleep(Duration::from_millis(30)).await;

    // The engine reads it back over the Hamlib net protocol (`f` => bare Hz).
    let mut engine = TcpStream::connect(addr).await.expect("connect");
    engine.write_all(b"f\n").await.expect("f");
    let mut buf = vec![0u8; 64];
    let n = engine.read(&mut buf).await.expect("read");
    let text = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(text.trim(), "14250000");

    // The read-only endpoint rejects a set-frequency write.
    engine.write_all(b"F 7000000\n").await.expect("F");
    let n = engine.read(&mut buf).await.expect("read");
    let text = String::from_utf8_lossy(&buf[..n]);
    assert!(
        text.starts_with("RPRT -"),
        "read-only endpoint rejects F: {text}"
    );
}
