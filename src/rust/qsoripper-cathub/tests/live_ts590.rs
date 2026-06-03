//! Opt-in live TS-590 hardware tests.
//!
//! These tests are ignored by default and only touch hardware when
//! `QSORIPPER_CATHUB_LIVE_TS590=1` is set. They start the built cathub binary against the
//! configured radio serial port, query its Hamlib endpoint, and then stop the process.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const LIVE_FLAG: &str = "QSORIPPER_CATHUB_LIVE_TS590";
const INTERACTIVE_FLAG: &str = "QSORIPPER_CATHUB_LIVE_INTERACTIVE";
const PORT_ENV: &str = "QSORIPPER_CATHUB_LIVE_PORT";
const BAUD_ENV: &str = "QSORIPPER_CATHUB_LIVE_BAUD";
const DEFAULT_PORT: &str = "COM3";
const DEFAULT_BAUD: u32 = 115_200;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Snapshot {
    vfo: String,
    freq_hz: u64,
    mode: String,
    passband_hz: u32,
}

struct LiveCathub {
    child: Child,
    config_path: PathBuf,
    read_only_addr: SocketAddr,
}

impl Drop for LiveCathub {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.config_path);
    }
}

fn live_enabled() -> bool {
    std::env::var(LIVE_FLAG).is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn interactive_enabled() -> bool {
    std::env::var(INTERACTIVE_FLAG)
        .is_ok_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn free_loopback_addr() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("allocate free loopback port");
    listener.local_addr().expect("local address")
}

fn temp_config_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "qsoripper-cathub-live-{tag}-{}-{}.toml",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ))
}

fn write_live_config(read_only_addr: SocketAddr, read_write_addr: SocketAddr) -> PathBuf {
    let port = std::env::var(PORT_ENV).unwrap_or_else(|_| DEFAULT_PORT.to_string());
    let baud = std::env::var(BAUD_ENV)
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(DEFAULT_BAUD);
    let path = temp_config_path("ts590");
    let text = format!(
        r#"[radio]
backend = "ts590"
transport = "serial"
port = "{port}"
baud = {baud}

[poll]
baseline_ms = 100
heartbeat_ms = 1000

[ptt]
max_tx_ms = 300000

[events]
native_push = true

[[hamlib_net]]
name = "engine-readonly"
bind = "{read_only_addr}"
perms = ["read"]

[[hamlib_net]]
name = "live-readwrite"
bind = "{read_write_addr}"
perms = ["read", "write"]
"#
    );
    std::fs::write(&path, text).expect("write live cathub config");
    path
}

fn start_live_cathub() -> Option<LiveCathub> {
    if !live_enabled() {
        eprintln!("Skipping live TS-590 test; set {LIVE_FLAG}=1 to enable hardware access.");
        return None;
    }

    let read_only_addr = free_loopback_addr();
    let read_write_addr = free_loopback_addr();
    let config_path = write_live_config(read_only_addr, read_write_addr);
    let binary = env!("CARGO_BIN_EXE_qsoripper-cathub");
    let child = Command::new(binary)
        .arg("--config")
        .arg(&config_path)
        .env("CATHUB_LOG", "qsoripper_cathub=debug")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("start qsoripper-cathub");

    let mut hub = LiveCathub {
        child,
        config_path,
        read_only_addr,
    };
    wait_for_endpoint(&mut hub).expect("wait for cathub Hamlib endpoint");
    Some(hub)
}

fn wait_for_endpoint(hub: &mut LiveCathub) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if TcpStream::connect(hub.read_only_addr).is_ok() {
            return Ok(());
        }
        if let Some(status) = hub.child.try_wait().expect("check cathub process") {
            return Err(format!(
                "cathub exited before Hamlib endpoint {} became reachable: {status}",
                hub.read_only_addr
            ));
        }
        assert!(
            Instant::now() < deadline,
            "cathub Hamlib endpoint {} did not become reachable",
            hub.read_only_addr
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn rigctl_lines(addr: SocketAddr, command: &str, expected_lines: usize) -> Vec<String> {
    let mut stream = TcpStream::connect(addr).expect("connect to cathub hamlib endpoint");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set read timeout");
    stream
        .write_all(format!("{command}\n").as_bytes())
        .expect("write rigctl command");
    stream.flush().expect("flush rigctl command");

    let mut reader = BufReader::new(stream);
    let mut lines = Vec::with_capacity(expected_lines);
    for _ in 0..expected_lines {
        let mut line = String::new();
        reader.read_line(&mut line).expect("read rigctl line");
        assert!(
            !line.is_empty(),
            "cathub closed connection before replying to {command}"
        );
        lines.push(line.trim().to_string());
    }
    lines
}

fn snapshot(addr: SocketAddr) -> Snapshot {
    let vfo = rigctl_lines(addr, "v", 1).remove(0);
    let freq_hz = rigctl_lines(addr, "f", 1)
        .remove(0)
        .parse::<u64>()
        .expect("frequency is integer Hz");
    let mode_lines = rigctl_lines(addr, "m", 2);
    let mode = mode_lines.first().expect("mode line").clone();
    let passband_hz = mode_lines
        .get(1)
        .expect("passband line")
        .parse::<u32>()
        .expect("passband is integer Hz");
    Snapshot {
        vfo,
        freq_hz,
        mode,
        passband_hz,
    }
}

fn vfo_info(addr: SocketAddr, vfo: &str) -> Snapshot {
    let lines = rigctl_lines(addr, &format!("\\get_vfo_info {vfo}"), 5);
    Snapshot {
        vfo: vfo.to_string(),
        freq_hz: lines
            .first()
            .expect("VFO frequency line")
            .parse::<u64>()
            .expect("VFO frequency is Hz"),
        mode: lines.get(1).expect("VFO mode line").clone(),
        passband_hz: lines
            .get(2)
            .expect("VFO passband line")
            .parse::<u32>()
            .expect("VFO passband is Hz"),
    }
}

fn wait_for_snapshot(
    addr: SocketAddr,
    predicate: impl Fn(&Snapshot) -> bool,
    description: &str,
) -> Snapshot {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snap = snapshot(addr);
        if predicate(&snap) {
            return snap;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}; last snapshot was {snap:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn wait_for_operator(message: &str) {
    eprintln!("{message}");
    eprintln!("Press Enter when ready.");
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .expect("read operator confirmation");
}

#[test]
#[ignore = "requires a real TS-590 connected to the configured CAT port"]
fn live_ts590_startup_snapshot_is_coherent() {
    let Some(hub) = start_live_cathub() else {
        return;
    };

    let active = wait_for_snapshot(
        hub.read_only_addr,
        |snap| snap.freq_hz > 0 && (snap.vfo == "VFOA" || snap.vfo == "VFOB"),
        "non-zero active VFO snapshot",
    );
    let active_info = vfo_info(hub.read_only_addr, &active.vfo);

    assert_eq!(
        active.freq_hz, active_info.freq_hz,
        "Hamlib f must match active VFO info"
    );
    assert_eq!(
        active.mode, active_info.mode,
        "Hamlib m must match active VFO info"
    );
    assert!(active.passband_hz > 0);
}

#[test]
#[ignore = "requires operator-assisted real TS-590 VFO switching"]
fn live_ts590_manual_vfo_switch_matrix() {
    if !interactive_enabled() {
        eprintln!(
            "Skipping operator-assisted VFO matrix; set {INTERACTIVE_FLAG}=1 in addition to {LIVE_FLAG}=1."
        );
        return;
    }
    let Some(hub) = start_live_cathub() else {
        return;
    };

    wait_for_operator(
        "Set VFO A and VFO B to different frequencies and modes, select VFO A, and wait for the dial to settle.",
    );
    let a = wait_for_snapshot(
        hub.read_only_addr,
        |snap| snap.vfo == "VFOA" && snap.freq_hz > 0,
        "active VFO A",
    );
    let a_info = vfo_info(hub.read_only_addr, "VFOA");
    assert_eq!(a.freq_hz, a_info.freq_hz);
    assert_eq!(a.mode, a_info.mode);

    wait_for_operator("Switch the radio to VFO B and wait for the dial to settle.");
    let b = wait_for_snapshot(
        hub.read_only_addr,
        |snap| snap.vfo == "VFOB" && snap.freq_hz > 0 && snap.freq_hz != a.freq_hz,
        "active VFO B with its own frequency",
    );
    let b_info = vfo_info(hub.read_only_addr, "VFOB");
    assert_eq!(b.freq_hz, b_info.freq_hz);
    assert_eq!(b.mode, b_info.mode);
    assert_ne!(
        a.freq_hz, b.freq_hz,
        "live VFO matrix requires intentionally different A/B frequencies"
    );
    assert_ne!(
        a.mode, b.mode,
        "live VFO matrix requires intentionally different A/B modes"
    );

    wait_for_operator("Switch the radio back to VFO A and wait for the dial to settle.");
    let a_again = wait_for_snapshot(
        hub.read_only_addr,
        |snap| snap.vfo == "VFOA" && snap.freq_hz == a.freq_hz,
        "return to VFO A frequency",
    );
    assert_eq!(a_again.mode, a.mode);
}
