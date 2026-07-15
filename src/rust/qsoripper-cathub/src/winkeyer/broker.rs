//! Pure WinKeyer multi-client scheduling and ownership state.
//!
//! The physical actor applies [`PhysicalAction`] values returned here. Keeping arbitration
//! independent of serial I/O makes interleaving, cancellation, pot restoration, and crash
//! recovery deterministic under unit tests.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use super::protocol::DeviceStatus;

/// Stable identity assigned to one virtual endpoint or typed API connection.
pub(crate) type ClientId = u64;
/// Stable identity assigned to one queued transmit job.
pub(crate) type JobId = u64;
/// Conservative payload ceiling below the physical WinKeyer input FIFO capacity.
pub(crate) const MAX_JOB_BYTES: usize = 120;
pub(crate) const MAX_QUEUED_JOBS: usize = 64;

/// Speed source desired by a client or one transmit job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpeedMode {
    /// Read speed directly from the physical potentiometer (`Set WPM Speed 0`).
    Pot,
    /// Use a fixed host-selected speed.
    Fixed(u8),
}

impl SpeedMode {
    pub(crate) fn command(self) -> [u8; 2] {
        [
            0x02,
            match self {
                Self::Pot => 0,
                Self::Fixed(wpm) => wpm,
            },
        ]
    }
}

/// One side effect the physical actor must perform in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PhysicalAction {
    /// Write bytes to the physical WinKeyer.
    Write(Vec<u8>),
    /// A job reached its normal completion boundary.
    Completed { job_id: JobId, client_id: ClientId },
    /// A job was canceled before or during transmission.
    Canceled { job_id: JobId, client_id: ClientId },
}

/// One immutable queued unit of transmission.
#[derive(Debug, Clone)]
struct Job {
    id: JobId,
    client_id: ClientId,
    payload: TransmitPayload,
    speed: SpeedMode,
    queued_at: Instant,
    stream: bool,
}

/// A transmit request keeps operator text distinct from WinKeyer wire-control bytes.
/// Only `wire_bytes` are written to the device; `intended_text` is the authoritative
/// character payload used for diagnostics and conformance tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TransmitPayload {
    wire_bytes: Vec<u8>,
    intended_text: Vec<u8>,
}

impl TransmitPayload {
    pub(crate) fn plain_text(intended_text: Vec<u8>) -> Self {
        Self {
            wire_bytes: intended_text.clone(),
            intended_text,
        }
    }

    pub(crate) fn legacy_stream(control_prefix: Vec<u8>, intended_text: Vec<u8>) -> Self {
        let mut wire_bytes = control_prefix;
        wire_bytes.extend_from_slice(&intended_text);
        Self {
            wire_bytes,
            intended_text,
        }
    }

    pub(crate) fn wire_bytes(&self) -> &[u8] {
        &self.wire_bytes
    }

    pub(crate) fn intended_text(&self) -> &[u8] {
        &self.intended_text
    }

    fn append(&mut self, other: &Self) {
        self.wire_bytes.extend_from_slice(&other.wire_bytes);
        self.intended_text.extend_from_slice(&other.intended_text);
    }
}

impl From<Vec<u8>> for TransmitPayload {
    fn from(value: Vec<u8>) -> Self {
        Self::plain_text(value)
    }
}

#[derive(Debug, Clone)]
struct ActiveJob {
    job: Job,
    saw_busy: bool,
    observed_status: bool,
    started_at: Instant,
}

#[derive(Debug, Clone)]
struct ClientState {
    desired_speed: SpeedMode,
    primary: bool,
    profile: BTreeMap<u16, Vec<u8>>,
}

/// Snapshot exposed to status and event surfaces.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct BrokerSnapshot {
    pub(crate) firmware_revision: Option<u8>,
    pub(crate) connected: bool,
    pub(crate) busy: bool,
    pub(crate) break_in: bool,
    pub(crate) key_down: bool,
    pub(crate) pot_value: Option<u8>,
    pub(crate) pot_wpm: Option<u8>,
    pub(crate) active_client_id: Option<ClientId>,
    pub(crate) active_job_id: Option<JobId>,
    pub(crate) queued_jobs: usize,
    pub(crate) last_error: Option<String>,
    pub(crate) last_safety_action: Option<String>,
}

/// Stateful scheduler for a single physical keyer.
#[derive(Debug)]
pub(crate) struct BrokerCore {
    clients: BTreeMap<ClientId, ClientState>,
    applied_profile_client_id: Option<ClientId>,
    queue: VecDeque<Job>,
    active: Option<ActiveJob>,
    next_job_id: JobId,
    firmware_revision: Option<u8>,
    connected: bool,
    status: DeviceStatus,
    pot_value: Option<u8>,
    pot_wpm: Option<u8>,
    physical_pot_min_wpm: u8,
    last_error: Option<String>,
    last_safety_action: Option<String>,
    max_tx: Duration,
}

impl BrokerCore {
    pub(crate) fn new(max_tx: Duration) -> Self {
        Self {
            clients: BTreeMap::new(),
            applied_profile_client_id: None,
            queue: VecDeque::new(),
            active: None,
            next_job_id: 1,
            firmware_revision: None,
            connected: false,
            status: DeviceStatus {
                raw: 0xc0,
                waiting: false,
                key_down: false,
                busy: false,
                break_in: false,
                xoff: false,
            },
            pot_value: None,
            pot_wpm: None,
            physical_pot_min_wpm: 5,
            last_error: None,
            last_safety_action: None,
            max_tx,
        }
    }

    pub(crate) fn connect_physical(&mut self, firmware_revision: u8) {
        self.firmware_revision = Some(firmware_revision);
        self.connected = true;
        self.applied_profile_client_id = None;
        self.last_error = None;
    }

    pub(crate) fn physical_error(&mut self, error: impl Into<String>) -> Vec<PhysicalAction> {
        self.connected = false;
        self.firmware_revision = None;
        self.applied_profile_client_id = None;
        self.last_error = Some(error.into());
        let mut actions = Vec::new();
        if let Some(active) = self.active.take() {
            actions.push(PhysicalAction::Canceled {
                job_id: active.job.id,
                client_id: active.job.client_id,
            });
        }
        while let Some(job) = self.queue.pop_front() {
            actions.push(PhysicalAction::Canceled {
                job_id: job.id,
                client_id: job.client_id,
            });
        }
        actions
    }

    pub(crate) fn register_client(&mut self, client_id: ClientId, primary: bool) -> bool {
        if primary && self.clients.values().any(|client| client.primary) {
            return false;
        }
        self.clients.insert(
            client_id,
            ClientState {
                desired_speed: SpeedMode::Pot,
                primary,
                profile: BTreeMap::new(),
            },
        );
        true
    }

    pub(crate) fn unregister_client(&mut self, client_id: ClientId) -> Vec<PhysicalAction> {
        self.clients.remove(&client_id);
        if self.applied_profile_client_id == Some(client_id) {
            self.applied_profile_client_id = None;
        }
        let mut actions = Vec::new();
        self.queue.retain(|job| {
            if job.client_id == client_id {
                actions.push(PhysicalAction::Canceled {
                    job_id: job.id,
                    client_id,
                });
                false
            } else {
                true
            }
        });
        if self.active.as_ref().map(|active| active.job.client_id) == Some(client_id) {
            if let Some(active) = self.active.take() {
                actions.push(PhysicalAction::Write(vec![0x0a]));
                actions.push(PhysicalAction::Canceled {
                    job_id: active.job.id,
                    client_id,
                });
            }
            self.last_safety_action = Some(format!(
                "cleared WinKeyer buffer after active client {client_id} disconnected"
            ));
            actions.extend(self.start_next_or_restore());
        }
        actions
    }

    pub(crate) fn set_client_speed(
        &mut self,
        client_id: ClientId,
        speed: SpeedMode,
    ) -> Vec<PhysicalAction> {
        let Some(client) = self.clients.get_mut(&client_id) else {
            return Vec::new();
        };
        client.desired_speed = speed;
        if self.active.is_none() && client.primary {
            vec![PhysicalAction::Write(speed.command().to_vec())]
        } else {
            Vec::new()
        }
    }

    pub(crate) fn set_client_command(
        &mut self,
        client_id: ClientId,
        command: Vec<u8>,
    ) -> Option<Vec<PhysicalAction>> {
        let opcode = *command.first()?;
        let key = if opcode == 0x00 {
            0x100 | u16::from(*command.get(1)?)
        } else {
            u16::from(opcode)
        };
        let client = self.clients.get_mut(&client_id)?;
        client.profile.insert(key, command.clone());
        let applies_now = self.active.is_none() && client.primary;
        if applies_now {
            if let Some(minimum) = speed_pot_minimum(&command) {
                self.physical_pot_min_wpm = minimum;
                self.refresh_pot_wpm();
            }
        } else if self.applied_profile_client_id == Some(client_id) {
            // The profile changed while this client owned an active stream. It could not
            // be applied mid-stream, so force a replay at the next ownership boundary.
            self.applied_profile_client_id = None;
        }
        Some(if applies_now {
            self.applied_profile_client_id = Some(client_id);
            vec![PhysicalAction::Write(command)]
        } else {
            Vec::new()
        })
    }

    pub(crate) fn active_owner_command(
        &self,
        client_id: ClientId,
        command: Vec<u8>,
    ) -> Option<Vec<PhysicalAction>> {
        let allowed = self.active.as_ref().map_or_else(
            || {
                self.clients
                    .get(&client_id)
                    .is_some_and(|client| client.primary)
            },
            |active| active.job.client_id == client_id,
        );
        allowed.then(|| vec![PhysicalAction::Write(command)])
    }

    pub(crate) fn enqueue<P: Into<TransmitPayload>>(
        &mut self,
        client_id: ClientId,
        payload: P,
        speed: Option<SpeedMode>,
        stream: bool,
        now: Instant,
    ) -> Option<(JobId, Vec<PhysicalAction>)> {
        let payload = payload.into();
        if payload.wire_bytes.is_empty()
            || payload.wire_bytes.len() > MAX_JOB_BYTES
            || !self.connected
        {
            return None;
        }
        let client = self.clients.get(&client_id)?;
        if stream {
            if let Some(active) = self.active.as_mut() {
                if active.job.client_id == client_id && active.job.stream {
                    if active.job.payload.wire_bytes.len() + payload.wire_bytes.len()
                        > MAX_JOB_BYTES
                    {
                        return None;
                    }
                    active.job.payload.append(&payload);
                    return Some((
                        active.job.id,
                        vec![PhysicalAction::Write(payload.wire_bytes)],
                    ));
                }
            }
            if let Some(queued) = self.queue.back_mut() {
                if queued.client_id == client_id && queued.stream {
                    if queued.payload.wire_bytes.len() + payload.wire_bytes.len() > MAX_JOB_BYTES {
                        return None;
                    }
                    queued.payload.append(&payload);
                    return Some((queued.id, Vec::new()));
                }
            }
        }
        if self.queue.len() >= MAX_QUEUED_JOBS {
            return None;
        }
        let job_id = self.next_job_id;
        self.next_job_id = self.next_job_id.saturating_add(1);
        self.queue.push_back(Job {
            id: job_id,
            client_id,
            payload,
            speed: speed.unwrap_or(client.desired_speed),
            queued_at: now,
            stream,
        });
        let actions = if self.active.is_none() {
            self.start_next_or_restore()
        } else {
            Vec::new()
        };
        Some((job_id, actions))
    }

    pub(crate) fn cancel_client(
        &mut self,
        client_id: ClientId,
        include_active: bool,
    ) -> Vec<PhysicalAction> {
        let mut actions = Vec::new();
        self.queue.retain(|job| {
            if job.client_id == client_id {
                actions.push(PhysicalAction::Canceled {
                    job_id: job.id,
                    client_id,
                });
                false
            } else {
                true
            }
        });
        let active_client_id = self.active.as_ref().map(|active| active.job.client_id);
        if include_active && active_client_id == Some(client_id) {
            if let Some(active) = self.active.take() {
                actions.insert(0, PhysicalAction::Write(vec![0x0a]));
                actions.push(PhysicalAction::Canceled {
                    job_id: active.job.id,
                    client_id,
                });
            }
            actions.extend(self.start_next_or_restore());
        } else if include_active
            && active_client_id.is_none()
            && self
                .clients
                .get(&client_id)
                .is_some_and(|client| client.primary)
        {
            // A primary idle controller is allowed to clear stale bytes left in the
            // physical FIFO. N1MM sends this when Ctrl+K opens; dropping it can make a
            // later character transmit remnants from an earlier stream.
            actions.insert(0, PhysicalAction::Write(vec![0x0a]));
        }
        actions
    }

    pub(crate) fn cancel_job(
        &mut self,
        client_id: ClientId,
        job_id: JobId,
    ) -> (bool, Vec<PhysicalAction>) {
        if self
            .active
            .as_ref()
            .is_some_and(|active| active.job.client_id == client_id && active.job.id == job_id)
        {
            let mut actions = vec![PhysicalAction::Write(vec![0x0a])];
            if let Some(active) = self.active.take() {
                actions.push(PhysicalAction::Canceled {
                    job_id: active.job.id,
                    client_id,
                });
            }
            actions.extend(self.start_next_or_restore());
            return (true, actions);
        }
        let mut canceled = false;
        let mut actions = Vec::new();
        self.queue.retain(|job| {
            if job.client_id == client_id && job.id == job_id {
                canceled = true;
                actions.push(PhysicalAction::Canceled { job_id, client_id });
                false
            } else {
                true
            }
        });
        (canceled, actions)
    }

    pub(crate) fn emergency_stop(&mut self, reason: &str) -> Vec<PhysicalAction> {
        let mut actions = vec![PhysicalAction::Write(vec![0x0a, 0x0b, 0x00])];
        if let Some(active) = self.active.take() {
            actions.push(PhysicalAction::Canceled {
                job_id: active.job.id,
                client_id: active.job.client_id,
            });
        }
        while let Some(job) = self.queue.pop_front() {
            actions.push(PhysicalAction::Canceled {
                job_id: job.id,
                client_id: job.client_id,
            });
        }
        self.last_safety_action = Some(reason.to_string());
        actions.extend(self.restore_foreground_speed());
        actions
    }

    pub(crate) fn record_safety_action(&mut self, action: impl Into<String>) {
        self.last_safety_action = Some(action.into());
    }

    pub(crate) fn observe_status(&mut self, status: DeviceStatus) -> Vec<PhysicalAction> {
        self.status = status;
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        active.observed_status = true;
        if status.busy || status.waiting || status.key_down {
            active.saw_busy = true;
            return Vec::new();
        }
        if active.saw_busy {
            return self.complete_active();
        }
        Vec::new()
    }

    /// Confirm an idle boundary when a very short job completed before a busy status could
    /// be observed. The actor calls this only after its settle timer and an idle status.
    pub(crate) fn confirm_idle_after_settle(
        &mut self,
        now: Instant,
        settle: Duration,
    ) -> Vec<PhysicalAction> {
        if self.active.as_ref().is_some_and(|active| {
            active.observed_status && now.duration_since(active.started_at) >= settle
        }) && !self.status.busy
            && !self.status.waiting
            && !self.status.key_down
        {
            self.complete_active()
        } else {
            Vec::new()
        }
    }

    pub(crate) fn observe_pot(&mut self, value: u8) {
        self.pot_value = Some(value);
        self.refresh_pot_wpm();
    }

    pub(crate) fn watchdog(&mut self, now: Instant) -> Vec<PhysicalAction> {
        let expired = self
            .active
            .as_ref()
            .is_some_and(|active| now.duration_since(active.started_at) >= self.max_tx);
        if expired {
            return self.emergency_stop("WinKeyer transmit safety ceiling reached");
        }
        Vec::new()
    }

    pub(crate) fn snapshot(&self) -> BrokerSnapshot {
        BrokerSnapshot {
            firmware_revision: self.firmware_revision,
            connected: self.connected,
            busy: self.status.busy || self.active.is_some(),
            break_in: self.status.break_in,
            key_down: self.status.key_down,
            pot_value: self.pot_value,
            pot_wpm: self.pot_wpm,
            active_client_id: self.active.as_ref().map(|active| active.job.client_id),
            active_job_id: self.active.as_ref().map(|active| active.job.id),
            queued_jobs: self.queue.len(),
            last_error: self.last_error.clone(),
            last_safety_action: self.last_safety_action.clone(),
        }
    }

    /// Reapply the primary client's transient profile after exclusive maintenance.
    pub(crate) fn restore_foreground(&mut self) -> Vec<PhysicalAction> {
        self.restore_foreground_speed()
    }

    fn complete_active(&mut self) -> Vec<PhysicalAction> {
        let Some(active) = self.active.take() else {
            return Vec::new();
        };
        let mut actions = vec![PhysicalAction::Completed {
            job_id: active.job.id,
            client_id: active.job.client_id,
        }];
        actions.extend(self.start_next_or_restore());
        actions
    }

    fn start_next_or_restore(&mut self) -> Vec<PhysicalAction> {
        if let Some(job) = self.queue.pop_front() {
            debug_assert!(job.queued_at <= Instant::now());
            let profile_changed = self.applied_profile_client_id != Some(job.client_id);
            let mut actions = if profile_changed {
                self.clients
                    .get(&job.client_id)
                    .map_or_else(Vec::new, |client| {
                        client
                            .profile
                            .values()
                            .cloned()
                            .map(PhysicalAction::Write)
                            .collect()
                    })
            } else {
                Vec::new()
            };
            if profile_changed {
                if let Some(minimum) = self
                    .clients
                    .get(&job.client_id)
                    .and_then(|client| profile_pot_minimum(&client.profile))
                {
                    self.physical_pot_min_wpm = minimum;
                    self.refresh_pot_wpm();
                }
                self.applied_profile_client_id = Some(job.client_id);
            }
            // A legacy WinKeyer stream may carry Buffered Speed (0x1c) after pointer
            // setup. Inserting an unbuffered speed command between those operations
            // breaks N1MM's pointer context and keys the WPM byte as a phantom character.
            // Typed/plain-text jobs still require the broker-selected speed command.
            if job.payload.wire_bytes.first() != Some(&0x1c) {
                actions.push(PhysicalAction::Write(job.speed.command().to_vec()));
            }
            actions.push(PhysicalAction::Write(job.payload.wire_bytes.clone()));
            actions.push(PhysicalAction::Write(vec![0x15]));
            self.active = Some(ActiveJob {
                job,
                saw_busy: false,
                observed_status: false,
                started_at: Instant::now(),
            });
            actions
        } else {
            self.restore_foreground_speed()
        }
    }

    fn restore_foreground_speed(&mut self) -> Vec<PhysicalAction> {
        let foreground = self
            .clients
            .iter()
            .find(|(_, client)| client.primary)
            .map(|(id, client)| (*id, client));
        let profile_changed =
            foreground.is_some_and(|(id, _)| Some(id) != self.applied_profile_client_id);
        let mut actions: Vec<_> = if profile_changed {
            foreground
                .into_iter()
                .flat_map(|(_, client)| client.profile.values().cloned())
                .map(PhysicalAction::Write)
                .collect()
        } else {
            Vec::new()
        };
        if profile_changed {
            self.applied_profile_client_id = foreground.map(|(id, _)| id);
        }
        let speed = foreground.map_or(SpeedMode::Pot, |(_, client)| client.desired_speed);
        self.physical_pot_min_wpm = foreground
            .and_then(|(_, client)| profile_pot_minimum(&client.profile))
            .unwrap_or(5);
        self.refresh_pot_wpm();
        actions.push(PhysicalAction::Write(speed.command().to_vec()));
        actions
    }

    fn refresh_pot_wpm(&mut self) {
        self.pot_wpm = self
            .pot_value
            .map(|offset| self.physical_pot_min_wpm.saturating_add(offset));
    }
}

fn speed_pot_minimum(command: &[u8]) -> Option<u8> {
    (command.first() == Some(&0x05))
        .then(|| command.get(1).copied())
        .flatten()
}

fn profile_pot_minimum(profile: &BTreeMap<u16, Vec<u8>>) -> Option<u8> {
    profile
        .get(&0x05)
        .and_then(|command| speed_pot_minimum(command))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn connected() -> BrokerCore {
        let mut broker = BrokerCore::new(Duration::from_secs(30));
        broker.connect_physical(31);
        assert!(broker.register_client(1, true));
        assert!(broker.register_client(2, false));
        broker
    }

    fn idle() -> DeviceStatus {
        DeviceStatus {
            raw: 0xc0,
            waiting: false,
            key_down: false,
            busy: false,
            break_in: false,
            xoff: false,
        }
    }

    fn busy() -> DeviceStatus {
        DeviceStatus {
            raw: 0xc4,
            busy: true,
            ..idle()
        }
    }

    #[test]
    fn only_one_primary_client_is_allowed() {
        let mut broker = BrokerCore::new(Duration::from_secs(30));
        assert!(broker.register_client(1, true));
        assert!(!broker.register_client(2, true));
    }

    #[test]
    fn jobs_from_two_clients_never_interleave() {
        let mut broker = connected();
        let now = Instant::now();
        let (_, first) = broker
            .enqueue(1, b"CQ".to_vec(), Some(SpeedMode::Fixed(20)), false, now)
            .expect("first");
        assert_eq!(
            first,
            vec![
                PhysicalAction::Write(vec![0x02, 20]),
                PhysicalAction::Write(b"CQ".to_vec()),
                PhysicalAction::Write(vec![0x15]),
            ]
        );
        let (_, second) = broker
            .enqueue(2, b"TEST".to_vec(), Some(SpeedMode::Fixed(30)), false, now)
            .expect("second");
        assert!(second.is_empty(), "second job must remain queued");

        assert!(broker.observe_status(busy()).is_empty());
        let boundary = broker.observe_status(idle());
        assert_eq!(
            boundary,
            vec![
                PhysicalAction::Completed {
                    job_id: 1,
                    client_id: 1
                },
                PhysicalAction::Write(vec![0x02, 30]),
                PhysicalAction::Write(b"TEST".to_vec()),
                PhysicalAction::Write(vec![0x15]),
            ]
        );
    }

    #[test]
    fn fixed_job_restores_primary_pot_mode_when_queue_drains() {
        let mut broker = connected();
        broker.set_client_speed(1, SpeedMode::Pot);
        let (_, _) = broker
            .enqueue(
                2,
                b"TEST".to_vec(),
                Some(SpeedMode::Fixed(35)),
                false,
                Instant::now(),
            )
            .expect("job");
        broker.observe_status(busy());
        let actions = broker.observe_status(idle());
        assert_eq!(
            actions,
            vec![
                PhysicalAction::Completed {
                    job_id: 1,
                    client_id: 2
                },
                PhysicalAction::Write(vec![0x02, 0]),
            ]
        );
    }

    #[test]
    fn pot_notification_updates_shared_snapshot() {
        let mut broker = connected();
        broker.observe_pot(27);
        assert_eq!(broker.snapshot().pot_value, Some(27));
        assert_eq!(broker.snapshot().pot_wpm, Some(32));
        broker.set_client_command(1, vec![0x05, 10, 20, 0]);
        assert_eq!(broker.snapshot().pot_wpm, Some(37));
    }

    #[test]
    fn canceling_queued_client_does_not_clear_active_client() {
        let mut broker = connected();
        broker.enqueue(1, b"CQ".to_vec(), None, false, Instant::now());
        broker.enqueue(2, b"TEST".to_vec(), None, false, Instant::now());
        assert_eq!(
            broker.cancel_client(2, true),
            vec![PhysicalAction::Canceled {
                job_id: 2,
                client_id: 2
            }]
        );
        assert_eq!(broker.snapshot().active_client_id, Some(1));
    }

    #[test]
    fn active_owner_abort_clears_buffer_then_starts_next_client() {
        let mut broker = connected();
        broker.enqueue(1, b"CQ".to_vec(), None, false, Instant::now());
        broker.enqueue(
            2,
            b"TEST".to_vec(),
            Some(SpeedMode::Fixed(25)),
            false,
            Instant::now(),
        );
        let actions = broker.cancel_client(1, true);
        assert_eq!(actions[0], PhysicalAction::Write(vec![0x0a]));
        assert_eq!(
            actions[1],
            PhysicalAction::Canceled {
                job_id: 1,
                client_id: 1
            }
        );
        assert_eq!(actions[2], PhysicalAction::Write(vec![0x02, 25]));
        assert_eq!(actions[3], PhysicalAction::Write(b"TEST".to_vec()));
        assert_eq!(broker.snapshot().active_client_id, Some(2));
    }

    #[test]
    fn primary_idle_clear_reaches_the_physical_fifo() {
        let mut broker = connected();
        assert_eq!(
            broker.cancel_client(1, true),
            vec![PhysicalAction::Write(vec![0x0a])]
        );
        assert!(broker.cancel_client(2, true).is_empty());
    }

    #[test]
    fn cancel_job_cannot_cancel_another_clients_job() {
        let mut broker = connected();
        broker.enqueue(1, b"CQ".to_vec(), None, false, Instant::now());
        let (canceled, actions) = broker.cancel_job(2, 1);
        assert!(!canceled);
        assert!(actions.is_empty());
        assert_eq!(broker.snapshot().active_client_id, Some(1));
    }

    #[test]
    fn disconnecting_active_client_clears_and_records_safety_action() {
        let mut broker = connected();
        broker.enqueue(1, b"CQ".to_vec(), None, false, Instant::now());
        let actions = broker.unregister_client(1);
        assert_eq!(actions[0], PhysicalAction::Write(vec![0x0a]));
        assert!(broker
            .snapshot()
            .last_safety_action
            .expect("safety")
            .contains("disconnected"));
    }

    #[test]
    fn watchdog_clears_key_and_all_queued_work() {
        let start = Instant::now();
        let mut broker = BrokerCore::new(Duration::from_secs(1));
        broker.connect_physical(31);
        broker.register_client(1, true);
        broker.enqueue(1, b"CQ".to_vec(), None, false, start);
        let actions = broker.watchdog(start + Duration::from_secs(2));
        assert_eq!(actions[0], PhysicalAction::Write(vec![0x0a, 0x0b, 0x00]));
        assert!(!broker.snapshot().busy);
    }

    #[test]
    fn short_job_can_complete_after_idle_settle_without_observed_busy() {
        let mut broker = connected();
        broker.enqueue(1, b"E".to_vec(), None, false, Instant::now());
        broker.observe_status(idle());
        let actions = broker.confirm_idle_after_settle(
            Instant::now() + Duration::from_millis(300),
            Duration::from_millis(250),
        );
        assert!(matches!(
            actions.first(),
            Some(PhysicalAction::Completed { .. })
        ));
    }

    #[test]
    fn active_stream_appends_without_creating_an_interleavable_job() {
        let mut broker = connected();
        let now = Instant::now();
        let (job, _) = broker
            .enqueue(1, b"C".to_vec(), None, true, now)
            .expect("stream");
        let (same_job, actions) = broker
            .enqueue(1, b"Q".to_vec(), None, true, now)
            .expect("append");
        assert_eq!(same_job, job);
        assert_eq!(actions, vec![PhysicalAction::Write(b"Q".to_vec())]);
        assert_eq!(broker.snapshot().queued_jobs, 0);
    }

    #[test]
    fn oversized_jobs_and_stream_growth_are_rejected_before_fifo_overflow() {
        let mut broker = connected();
        assert!(broker
            .enqueue(
                1,
                vec![b'X'; MAX_JOB_BYTES + 1],
                None,
                false,
                Instant::now(),
            )
            .is_none());
        broker
            .enqueue(1, vec![b'A'; MAX_JOB_BYTES - 1], None, true, Instant::now())
            .expect("stream");
        assert!(broker
            .enqueue(1, b"BC".to_vec(), None, true, Instant::now())
            .is_none());
    }

    #[test]
    fn per_client_profile_is_replayed_only_when_that_client_becomes_active() {
        let mut broker = connected();
        assert!(broker
            .set_client_command(2, vec![0x0e, 0x04])
            .expect("client")
            .is_empty());
        let (_, actions) = broker
            .enqueue(2, b"E".to_vec(), None, false, Instant::now())
            .expect("job");
        assert_eq!(actions[0], PhysicalAction::Write(vec![0x0e, 0x04]));
        assert_eq!(actions[1], PhysicalAction::Write(vec![0x02, 0]));
        assert_eq!(actions[2], PhysicalAction::Write(b"E".to_vec()));
    }

    #[test]
    fn already_applied_primary_profile_is_not_inserted_into_keyboard_stream() {
        let mut broker = connected();
        assert_eq!(
            broker
                .set_client_command(1, vec![0x0e, 0x04])
                .expect("primary"),
            vec![PhysicalAction::Write(vec![0x0e, 0x04])]
        );
        let payload = TransmitPayload::legacy_stream(vec![0x1c, 23], b"TEST".to_vec());
        assert_eq!(payload.intended_text(), b"TEST");
        assert_eq!(payload.wire_bytes(), b"\x1c\x17TEST");
        let (_, actions) = broker
            .enqueue(1, payload, None, true, Instant::now())
            .expect("keyboard stream");
        assert_eq!(
            actions,
            vec![
                PhysicalAction::Write(b"\x1c\x17TEST".to_vec()),
                PhysicalAction::Write(vec![0x15]),
            ]
        );
    }

    #[test]
    fn profile_change_during_active_stream_is_replayed_at_next_boundary() {
        let mut broker = connected();
        broker
            .enqueue(1, b"T".to_vec(), None, true, Instant::now())
            .expect("active stream");
        assert_eq!(broker.applied_profile_client_id, Some(1));
        assert!(broker
            .set_client_command(1, vec![0x0e, 0x04])
            .expect("active client")
            .is_empty());
        assert_eq!(broker.applied_profile_client_id, None);

        broker.observe_status(busy());
        let actions = broker.observe_status(idle());
        assert!(actions.contains(&PhysicalAction::Write(vec![0x0e, 0x04])));
    }
}
