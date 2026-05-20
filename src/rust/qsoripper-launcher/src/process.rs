//! Spawn detached child processes and remember their PIDs so we can stop them.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use sysinfo::{Pid, ProcessRefreshKind, Signal, System};

use crate::catalog::{ComponentId, ComponentSpec};

/// A child the launcher started, tracked by OS PID so it survives the launcher
/// exiting and so a later "stop" can find it again.
#[derive(Debug, Clone)]
pub(crate) struct LaunchedProcess {
    pub(crate) component: ComponentId,
    pub(crate) pid: u32,
    #[allow(dead_code)]
    pub(crate) executable: PathBuf,
}

/// Records launched-process PIDs by component id.
#[derive(Debug, Default)]
pub(crate) struct ProcessRegistry {
    inner: BTreeMap<ComponentId, LaunchedProcess>,
}

impl ProcessRegistry {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn insert(&mut self, p: LaunchedProcess) {
        self.inner.insert(p.component, p);
    }

    #[allow(dead_code)]
    pub(crate) fn get(&self, id: ComponentId) -> Option<&LaunchedProcess> {
        self.inner.get(&id)
    }

    pub(crate) fn remove(&mut self, id: ComponentId) -> Option<LaunchedProcess> {
        self.inner.remove(&id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = &LaunchedProcess> {
        self.inner.values()
    }
}

/// Build the [`Command`] used to launch `spec` from its published artifact.
///
/// `env` entries are applied after the parent environment so callers can
/// inject `QSORIPPER_ENGINE` and `QSORIPPER_ENDPOINT` per-UI.
pub(crate) fn build_command<I, K, V>(
    spec: &ComponentSpec,
    exe: &Path,
    args: &[&OsStr],
    env: I,
) -> Command
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<OsStr>,
    V: AsRef<OsStr>,
{
    let mut cmd = Command::new(exe);
    cmd.args(args);
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    for (k, v) in env {
        cmd.env(k, v);
    }
    // Don't let child stdio inherit the launcher's raw-mode terminal.
    cmd.stdin(Stdio::null());
    cmd.stdout(Stdio::null());
    cmd.stderr(Stdio::null());

    apply_detach_flags(&mut cmd);

    // Touch `spec` so unused-warning lints don't fire on platforms where we
    // don't otherwise read from it; in practice this also keeps the signature
    // open for future per-component spawn tweaks.
    let _ = spec.id;
    cmd
}

#[cfg(windows)]
fn apply_detach_flags(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    // CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    cmd.creation_flags(CREATE_NEW_PROCESS_GROUP | DETACHED_PROCESS);
}

#[cfg(not(windows))]
fn apply_detach_flags(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // Put the child in its own process group so a SIGINT delivered to the
    // launcher's foreground group does not propagate to UIs we launched.
    // `process_group(0)` is enough for cleanly-exiting launches; we do not
    // need a full new session for the TUI launcher use case.
    cmd.process_group(0);
}

/// Spawn the executable behind `spec` and record its PID in `registry`.
pub(crate) fn spawn(
    spec: &ComponentSpec,
    exe: &Path,
    args: &[&OsStr],
    env: &[(String, String)],
    registry: &mut ProcessRegistry,
) -> Result<LaunchedProcess> {
    if !exe.exists() {
        bail!(
            "missing artifact for {}: {} (was the build run?)",
            spec.display_name,
            exe.display()
        );
    }
    let mut cmd = build_command(
        spec,
        exe,
        args,
        env.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    );
    let child = cmd
        .spawn()
        .with_context(|| format!("spawning {}", exe.display()))?;
    let entry = LaunchedProcess {
        component: spec.id,
        pid: child.id(),
        executable: exe.to_path_buf(),
    };
    // Drop the Child handle so the launcher does not block on waitpid; the
    // OS reaps via the new session/process group. Tracking is by PID from here.
    drop(child);
    registry.insert(entry.clone());
    Ok(entry)
}

/// Ask the OS to terminate the given PID. Uses SIGTERM-equivalent semantics.
pub(crate) fn stop_pid(pid: u32) -> bool {
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        ProcessRefreshKind::nothing(),
    );
    let Some(proc_handle) = sys.process(Pid::from_u32(pid)) else {
        return false;
    };
    proc_handle
        .kill_with(Signal::Term)
        .unwrap_or_else(|| proc_handle.kill())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::similar_names,
    clippy::too_many_lines,
    clippy::needless_pass_by_value
)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    use crate::catalog::{find, ENGINE_RUST};

    #[test]
    fn build_command_sets_args_and_env() {
        let spec = find(ENGINE_RUST).expect("rust engine");
        let exe = PathBuf::from("/no/such/path/qsoripper-server");
        let arg1: OsString = "--listen".into();
        let arg2: OsString = "127.0.0.1:50051".into();
        let args = [arg1.as_os_str(), arg2.as_os_str()];
        let cmd = build_command(&spec, &exe, &args, [("FOO", "bar")]);
        let argv: Vec<&OsStr> = cmd.get_args().collect();
        assert_eq!(argv, vec![arg1.as_os_str(), arg2.as_os_str()]);
        let env_pairs: Vec<(&OsStr, Option<&OsStr>)> = cmd.get_envs().collect();
        assert!(env_pairs
            .iter()
            .any(|(k, v)| *k == OsStr::new("FOO")
                && v.map(OsStr::to_os_string) == Some("bar".into())));
    }

    #[test]
    fn registry_round_trip() {
        let mut reg = ProcessRegistry::new();
        reg.insert(LaunchedProcess {
            component: ENGINE_RUST,
            pid: 1234,
            executable: PathBuf::from("x"),
        });
        assert_eq!(reg.get(ENGINE_RUST).map(|p| p.pid), Some(1234));
        assert!(reg.remove(ENGINE_RUST).is_some());
        assert!(reg.get(ENGINE_RUST).is_none());
    }
}
