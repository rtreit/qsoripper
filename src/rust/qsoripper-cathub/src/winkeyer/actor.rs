//! Physical WinKeyer actor and typed in-process broker handle.

use std::time::{Duration, Instant};
use std::{future::Future, io};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use super::broker::{
    BrokerCore, BrokerSnapshot, ClientId, JobId, PhysicalAction, SpeedMode, MAX_JOB_BYTES,
    MAX_QUEUED_JOBS,
};
use super::protocol::DeviceEvent;
use crate::ptt::{PttDenied, PttManager};

const REQUEST_CAPACITY: usize = 128;
const EVENT_CAPACITY: usize = 256;
const HOST_OPEN_TIMEOUT: Duration = Duration::from_millis(750);
const ACTOR_TICK: Duration = Duration::from_millis(50);
const SHORT_JOB_SETTLE: Duration = Duration::from_millis(250);
const STATUS_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WINKEYER_STATION_TX_OWNER: u64 = u64::MAX;
const RECONNECT_INITIAL: Duration = Duration::from_millis(250);
const RECONNECT_MAX: Duration = Duration::from_secs(5);

/// Errors returned by the broker handle.
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub(crate) enum BrokerError {
    #[error("WinKeyer broker is unavailable")]
    Unavailable,
    #[error("WinKeyer client is not registered")]
    UnknownClient,
    #[error("a primary WinKeyer client is already registered")]
    PrimaryExists,
    #[error("WinKeyer is not connected")]
    NotConnected,
    #[error("invalid WinKeyer request: {0}")]
    Invalid(String),
    #[error("WinKeyer transport: {0}")]
    Transport(String),
    #[error("station transmit is owned by another client")]
    TransmitBusy,
    #[error("WinKeyer maintenance is owned by another client or transmit work is pending")]
    MaintenanceBusy,
    #[error("WinKeyer transmit queue is full")]
    QueueFull,
}

/// Events emitted by the broker for virtual faces and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BrokerEvent {
    Connected { firmware_revision: u8 },
    SpeedPot { raw: u8, value: u8, wpm: u8 },
    Status { raw: u8 },
    Echo(u8),
    MaintenanceByte { client_id: ClientId, byte: u8 },
    Completed { job_id: JobId, client_id: ClientId },
    Canceled { job_id: JobId, client_id: ClientId },
    Error(String),
}

enum Request {
    Register {
        client_id: ClientId,
        primary: bool,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    Unregister {
        client_id: ClientId,
    },
    SetSpeed {
        client_id: ClientId,
        speed: SpeedMode,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    Enqueue {
        client_id: ClientId,
        bytes: Vec<u8>,
        speed: Option<SpeedMode>,
        stream: bool,
        reply: oneshot::Sender<Result<JobId, BrokerError>>,
    },
    Cancel {
        client_id: ClientId,
        include_active: bool,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    CancelJob {
        client_id: ClientId,
        job_id: JobId,
        reply: oneshot::Sender<Result<bool, BrokerError>>,
    },
    EmergencyStop {
        reason: String,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    Configure {
        client_id: ClientId,
        command: Vec<u8>,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    ActiveOwnerCommand {
        client_id: ClientId,
        command: Vec<u8>,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    MaintenanceCommand {
        client_id: ClientId,
        command: Vec<u8>,
        reply: oneshot::Sender<Result<(), BrokerError>>,
    },
    ReleaseMaintenance {
        client_id: ClientId,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

/// Cloneable command/status surface shared by gRPC and virtual COM faces.
#[derive(Clone)]
pub(crate) struct BrokerHandle {
    requests: mpsc::Sender<Request>,
    snapshots: watch::Receiver<BrokerSnapshot>,
    events: broadcast::Sender<BrokerEvent>,
}

impl BrokerHandle {
    pub(crate) async fn register(
        &self,
        client_id: ClientId,
        primary: bool,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Register {
            client_id,
            primary,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn unregister(&self, client_id: ClientId) -> Result<(), BrokerError> {
        self.send(Request::Unregister { client_id }).await
    }

    pub(crate) async fn set_speed(
        &self,
        client_id: ClientId,
        speed: SpeedMode,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::SetSpeed {
            client_id,
            speed,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn enqueue(
        &self,
        client_id: ClientId,
        bytes: Vec<u8>,
        speed: Option<SpeedMode>,
    ) -> Result<JobId, BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Enqueue {
            client_id,
            bytes,
            speed,
            stream: false,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn stream(
        &self,
        client_id: ClientId,
        bytes: Vec<u8>,
    ) -> Result<JobId, BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Enqueue {
            client_id,
            bytes,
            speed: None,
            stream: true,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn configure(
        &self,
        client_id: ClientId,
        command: Vec<u8>,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Configure {
            client_id,
            command,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn active_owner_command(
        &self,
        client_id: ClientId,
        command: Vec<u8>,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::ActiveOwnerCommand {
            client_id,
            command,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn maintenance_command(
        &self,
        client_id: ClientId,
        command: Vec<u8>,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::MaintenanceCommand {
            client_id,
            command,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn release_maintenance(&self, client_id: ClientId) -> Result<(), BrokerError> {
        self.send(Request::ReleaseMaintenance { client_id }).await
    }

    pub(crate) async fn cancel(
        &self,
        client_id: ClientId,
        include_active: bool,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Cancel {
            client_id,
            include_active,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn cancel_job(
        &self,
        client_id: ClientId,
        job_id: JobId,
    ) -> Result<bool, BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::CancelJob {
            client_id,
            job_id,
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) async fn emergency_stop(
        &self,
        reason: impl Into<String>,
    ) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::EmergencyStop {
            reason: reason.into(),
            reply,
        })
        .await?;
        response.await.map_err(|_| BrokerError::Unavailable)?
    }

    pub(crate) fn snapshot(&self) -> BrokerSnapshot {
        self.snapshots.borrow().clone()
    }

    pub(crate) fn subscribe(&self) -> broadcast::Receiver<BrokerEvent> {
        self.events.subscribe()
    }

    pub(crate) async fn shutdown(&self) -> Result<(), BrokerError> {
        let (reply, response) = oneshot::channel();
        self.send(Request::Shutdown { reply }).await?;
        response.await.map_err(|_| BrokerError::Unavailable)
    }

    async fn send(&self, request: Request) -> Result<(), BrokerError> {
        self.requests
            .send(request)
            .await
            .map_err(|_| BrokerError::Unavailable)
    }
}

/// Open a host session and spawn the physical actor over any async byte transport.
#[cfg(test)]
pub(crate) async fn spawn<T>(transport: T, max_tx: Duration) -> Result<BrokerHandle, BrokerError>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    spawn_inner(transport, max_tx, None, false, false, || async {
        Err(io::Error::other("reconnect disabled"))
    })
    .await
}

/// Spawn a broker integrated with the station-wide CAT/PTT ownership lease.
#[cfg(test)]
pub(crate) async fn spawn_with_ptt<T>(
    transport: T,
    max_tx: Duration,
    ptt: PttManager,
) -> Result<BrokerHandle, BrokerError>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    spawn_inner(transport, max_tx, Some(ptt), false, false, || async {
        Err(io::Error::other("reconnect disabled"))
    })
    .await
}

/// Spawn a broker whose physical transport is reopened with bounded backoff after failure.
pub(crate) async fn spawn_supervised<T, O, F>(
    transport: T,
    max_tx: Duration,
    ptt: PttManager,
    opener: O,
) -> Result<BrokerHandle, BrokerError>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    O: FnMut() -> F + Send + 'static,
    F: Future<Output = io::Result<T>> + Send + 'static,
{
    spawn_inner(transport, max_tx, Some(ptt), true, true, opener).await
}

async fn spawn_inner<T, O, F>(
    mut transport: T,
    max_tx: Duration,
    ptt: Option<PttManager>,
    reconnect: bool,
    zero_is_idle: bool,
    opener: O,
) -> Result<BrokerHandle, BrokerError>
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    O: FnMut() -> F + Send + 'static,
    F: Future<Output = io::Result<T>> + Send + 'static,
{
    let firmware_revision = initialize_transport(&mut transport, zero_is_idle).await?;

    let mut core = BrokerCore::new(max_tx);
    core.connect_physical(firmware_revision);
    let initial = core.snapshot();
    let (request_tx, request_rx) = mpsc::channel(REQUEST_CAPACITY);
    let (snapshot_tx, snapshot_rx) = watch::channel(initial);
    let (event_tx, _) = broadcast::channel(EVENT_CAPACITY);
    let handle = BrokerHandle {
        requests: request_tx,
        snapshots: snapshot_rx,
        events: event_tx.clone(),
    };
    let _ = event_tx.send(BrokerEvent::Connected { firmware_revision });
    tokio::spawn(run_actor(
        transport,
        core,
        request_rx,
        snapshot_tx,
        event_tx,
        ptt,
        reconnect,
        zero_is_idle,
        opener,
    ));
    Ok(handle)
}

async fn initialize_transport<T>(transport: &mut T, zero_is_idle: bool) -> Result<u8, BrokerError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    transport
        .write_all(&[0x00, 0x02])
        .await
        .map_err(transport_error)?;
    transport.flush().await.map_err(transport_error)?;
    let firmware_revision = tokio::time::timeout(
        HOST_OPEN_TIMEOUT,
        read_transport_byte(transport, zero_is_idle),
    )
    .await
    .map_err(|_| BrokerError::Transport("Host Open timed out".to_string()))?
    .map_err(transport_error)?;
    if firmware_revision == 0xff {
        return Err(BrokerError::Transport(
            "received 0xFF firmware response; verify baud rate".to_string(),
        ));
    }
    transport
        .write_all(&[0x0a, 0x0b, 0x00, 0x02, 0x00, 0x07, 0x15])
        .await
        .map_err(transport_error)?;
    transport.flush().await.map_err(transport_error)?;
    Ok(firmware_revision)
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)] // One supervised ownership loop.
async fn run_actor<T, O, F>(
    mut transport: T,
    mut core: BrokerCore,
    mut requests: mpsc::Receiver<Request>,
    snapshots: watch::Sender<BrokerSnapshot>,
    events: broadcast::Sender<BrokerEvent>,
    ptt: Option<PttManager>,
    reconnect: bool,
    zero_is_idle: bool,
    mut opener: O,
) where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    O: FnMut() -> F + Send + 'static,
    F: Future<Output = io::Result<T>> + Send + 'static,
{
    let mut maintenance_owner = None;
    let mut maintenance_reopen_pending = false;
    let mut reconnect_delay = RECONNECT_INITIAL;

    'actor: loop {
        let (mut reader, mut writer) = tokio::io::split(&mut transport);
        let mut tick = tokio::time::interval(ACTOR_TICK);
        let mut last_status_poll = Instant::now();
        let session_error = 'session: loop {
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { break 'actor };
                    match handle_request(
                        request,
                        &mut core,
                        &mut writer,
                        &events,
                    ptt.as_ref(),
                    &mut maintenance_owner,
                    &mut maintenance_reopen_pending,
                    ).await {
                        Ok(true) => {
                            publish_snapshot(&core, &snapshots);
                            break 'actor;
                        }
                        Ok(false) => {}
                        Err(error) => break 'session format!("request write failed: {error}"),
                    }
                    release_station_tx_if_idle(&core, ptt.as_ref());
                    publish_snapshot(&core, &snapshots);
                }
            byte = read_transport_byte(&mut reader, zero_is_idle) => {
                    match byte {
                    Ok(byte) => {
                        if maintenance_reopen_pending {
                            maintenance_reopen_pending = false;
                            if byte == 0xff {
                                break 'session "maintenance Host Open returned 0xFF".to_string();
                            }
                            core.connect_physical(byte);
                            let mut actions = vec![PhysicalAction::Write(vec![
                                0x0a, 0x0b, 0x00, 0x02, 0x00, 0x07, 0x15,
                            ])];
                            actions.extend(core.restore_foreground());
                            if apply_actions(actions, &mut writer, &events).await.is_err() {
                                break 'session "maintenance restore write failed".to_string();
                            }
                            let _ = events.send(BrokerEvent::Connected {
                                firmware_revision: byte,
                            });
                            publish_snapshot(&core, &snapshots);
                            continue;
                        }
                        if let Some(client_id) = maintenance_owner {
                                let _ = events.send(BrokerEvent::MaintenanceByte { client_id, byte });
                                continue;
                            }
                            let actions = match DeviceEvent::from_byte(byte) {
                            DeviceEvent::SpeedPot { raw, value } => {
                                core.observe_pot(value);
                                let wpm = core.snapshot().pot_wpm.unwrap_or(value);
                                let _ = events.send(BrokerEvent::SpeedPot { raw, value, wpm });
                                    Vec::new()
                                }
                            DeviceEvent::Status(status) => {
                                let _ = events.send(BrokerEvent::Status { raw: status.raw });
                                let physical_tx = status.busy || status.break_in || status.key_down;
                                if physical_tx && core.snapshot().active_job_id.is_none() {
                                    if let Some(ptt) = &ptt {
                                        if matches!(
                                            ptt.try_key(WINKEYER_STATION_TX_OWNER, true),
                                            Err(PttDenied::Busy)
                                        ) {
                                            let message = "physical WinKeyer paddle/key activity conflicted with CAT PTT ownership";
                                            core.record_safety_action(message);
                                            let _ = events.send(BrokerEvent::Error(message.to_string()));
                                        }
                                    }
                                }
                                core.observe_status(status)
                                }
                                DeviceEvent::Echo(byte) => {
                                    let _ = events.send(BrokerEvent::Echo(byte));
                                    Vec::new()
                                }
                            };
                            if apply_actions(actions, &mut writer, &events).await.is_err() {
                                break 'session "write failed".to_string();
                            }
                            publish_snapshot(&core, &snapshots);
                            release_station_tx_if_idle(&core, ptt.as_ref());
                        }
                        Err(error) => {
                            break 'session format!("read failed: {error}");
                        }
                    }
                }
                _ = tick.tick() => {
                    let now = Instant::now();
                    let mut actions = core.watchdog(now);
                    actions.extend(core.confirm_idle_after_settle(now, SHORT_JOB_SETTLE));
                    if core.snapshot().busy && now.duration_since(last_status_poll) >= STATUS_POLL_INTERVAL {
                        actions.push(PhysicalAction::Write(vec![0x15]));
                        last_status_poll = now;
                    }
                    if apply_actions(actions, &mut writer, &events).await.is_err() {
                        break 'session "write failed".to_string();
                    }
                    publish_snapshot(&core, &snapshots);
                    release_station_tx_if_idle(&core, ptt.as_ref());
                }
            }
        };

        drop(reader);
        drop(writer);
        maintenance_owner = None;
        maintenance_reopen_pending = false;
        let actions = core.physical_error(session_error.clone());
        let _ = events.send(BrokerEvent::Error(session_error));
        // Cancellations are notifications only after the transport has failed.
        let mut sink = tokio::io::sink();
        let _ = apply_actions(actions, &mut sink, &events).await;
        if let Some(ptt) = &ptt {
            ptt.unkey(WINKEYER_STATION_TX_OWNER);
        }
        publish_snapshot(&core, &snapshots);

        loop {
            let delay = if reconnect {
                reconnect_delay
            } else {
                Duration::from_secs(86_400)
            };
            tokio::select! {
                request = requests.recv() => {
                    let Some(request) = request else { break 'actor };
                    match handle_request(
                        request,
                        &mut core,
                        &mut sink,
                        &events,
                        ptt.as_ref(),
                        &mut maintenance_owner,
                        &mut maintenance_reopen_pending,
                    ).await {
                        Ok(true) => break 'actor,
                        Ok(false) => {}
                        Err(error) => {
                            let _ = events.send(BrokerEvent::Error(error.to_string()));
                        }
                    }
                    publish_snapshot(&core, &snapshots);
                }
                () = tokio::time::sleep(delay), if reconnect => {
                    match opener().await {
                        Ok(mut candidate) => match initialize_transport(&mut candidate, zero_is_idle).await {
                            Ok(firmware_revision) => {
                                transport = candidate;
                                core.connect_physical(firmware_revision);
                                let actions = core.restore_foreground();
                                if let Err(error) = apply_actions(actions, &mut transport, &events).await {
                                    let message = format!("reconnect restore failed: {error}");
                                    let _ = events.send(BrokerEvent::Error(message.clone()));
                                    core.physical_error(message);
                                    publish_snapshot(&core, &snapshots);
                                    reconnect_delay = (reconnect_delay * 2).min(RECONNECT_MAX);
                                    continue;
                                }
                                let _ = events.send(BrokerEvent::Connected { firmware_revision });
                                publish_snapshot(&core, &snapshots);
                                reconnect_delay = RECONNECT_INITIAL;
                                break;
                            }
                            Err(error) => {
                                let message = format!("reconnect initialization failed: {error}");
                                let _ = events.send(BrokerEvent::Error(message.clone()));
                                core.physical_error(message);
                                publish_snapshot(&core, &snapshots);
                            }
                        },
                        Err(error) => {
                            let message = format!("reconnect open failed: {error}");
                            let _ = events.send(BrokerEvent::Error(message.clone()));
                            core.physical_error(message);
                            publish_snapshot(&core, &snapshots);
                        }
                    }
                    reconnect_delay = (reconnect_delay * 2).min(RECONNECT_MAX);
                }
            }
        }
    }

    // Best effort. Clear buffer, force key-up, then return to standalone EEPROM settings.
    let _ = transport.write_all(&[0x0a, 0x0b, 0x00, 0x00, 0x03]).await;
    let _ = transport.flush().await;
    if let Some(ptt) = &ptt {
        ptt.unkey(WINKEYER_STATION_TX_OWNER);
    }
}

/// Windows serial drivers may report a zero-length asynchronous read for an idle timeout.
/// A byte stream EOF is meaningful for sockets/duplex tests, but not for an open COM port;
/// retry until a byte or a real I/O error arrives. Host Open applies its own deadline.
async fn read_transport_byte<T>(transport: &mut T, zero_is_idle: bool) -> io::Result<u8>
where
    T: AsyncRead + Unpin,
{
    let mut byte = [0_u8; 1];
    loop {
        match transport.read(&mut byte).await? {
            0 if zero_is_idle => tokio::time::sleep(Duration::from_millis(1)).await,
            0 => return Err(io::Error::from(io::ErrorKind::UnexpectedEof)),
            _ => return Ok(byte[0]),
        }
    }
}

#[allow(clippy::single_match_else, clippy::too_many_lines)] // Exhaustive request arbitration.
async fn handle_request<W>(
    request: Request,
    core: &mut BrokerCore,
    writer: &mut W,
    events: &broadcast::Sender<BrokerEvent>,
    ptt: Option<&PttManager>,
    maintenance_owner: &mut Option<ClientId>,
    maintenance_reopen_pending: &mut bool,
) -> Result<bool, BrokerError>
where
    W: AsyncWrite + Unpin,
{
    let actions = match request {
        Request::Register {
            client_id,
            primary,
            reply,
        } => {
            let result = if core.register_client(client_id, primary) {
                Ok(())
            } else {
                Err(BrokerError::PrimaryExists)
            };
            let _ = reply.send(result);
            Vec::new()
        }
        Request::Unregister { client_id } => {
            let mut actions = core.unregister_client(client_id);
            if *maintenance_owner == Some(client_id) {
                *maintenance_owner = None;
                *maintenance_reopen_pending = true;
                actions.insert(0, PhysicalAction::Write(vec![0x00, 0x02]));
            }
            actions
        }
        Request::SetSpeed {
            client_id,
            speed,
            reply,
        } => {
            if *maintenance_reopen_pending
                || maintenance_owner.is_some_and(|owner| owner != client_id)
            {
                let _ = reply.send(Err(BrokerError::MaintenanceBusy));
                return Ok(false);
            }
            let known = core.set_client_speed(client_id, speed);
            let result = if core.snapshot().connected {
                Ok(())
            } else {
                Err(BrokerError::NotConnected)
            };
            let _ = reply.send(result);
            known
        }
        Request::Enqueue {
            client_id,
            bytes,
            speed,
            stream,
            reply,
        } => {
            if maintenance_owner.is_some() || *maintenance_reopen_pending {
                let _ = reply.send(Err(BrokerError::MaintenanceBusy));
                return Ok(false);
            }
            let snapshot = core.snapshot();
            if bytes.is_empty() || bytes.len() > MAX_JOB_BYTES {
                let _ = reply.send(Err(BrokerError::Invalid(format!(
                    "job payload must contain 1 through {MAX_JOB_BYTES} bytes"
                ))));
                return Ok(false);
            }
            if snapshot.queued_jobs >= MAX_QUEUED_JOBS
                && !(stream && snapshot.active_client_id == Some(client_id))
            {
                let _ = reply.send(Err(BrokerError::QueueFull));
                return Ok(false);
            }
            if snapshot.active_job_id.is_none()
                && (snapshot.busy || snapshot.break_in || snapshot.key_down)
            {
                let _ = reply.send(Err(BrokerError::TransmitBusy));
                return Ok(false);
            }
            if snapshot.active_job_id.is_none() {
                if let Some(ptt) = ptt {
                    if matches!(
                        ptt.try_key(WINKEYER_STATION_TX_OWNER, true),
                        Err(PttDenied::Busy)
                    ) {
                        let _ = reply.send(Err(BrokerError::TransmitBusy));
                        return Ok(false);
                    }
                }
            }
            match core.enqueue(client_id, bytes, speed, stream, Instant::now()) {
                Some((job_id, actions)) => {
                    let _ = reply.send(Ok(job_id));
                    actions
                }
                None => {
                    let error = if core.snapshot().connected {
                        BrokerError::UnknownClient
                    } else {
                        BrokerError::NotConnected
                    };
                    let _ = reply.send(Err(error));
                    Vec::new()
                }
            }
        }
        Request::Cancel {
            client_id,
            include_active,
            reply,
        } => {
            let actions = core.cancel_client(client_id, include_active);
            let _ = reply.send(Ok(()));
            actions
        }
        Request::CancelJob {
            client_id,
            job_id,
            reply,
        } => {
            let (canceled, actions) = core.cancel_job(client_id, job_id);
            let _ = reply.send(Ok(canceled));
            actions
        }
        Request::EmergencyStop { reason, reply } => {
            *maintenance_owner = None;
            *maintenance_reopen_pending = false;
            let actions = core.emergency_stop(&reason);
            let _ = reply.send(Ok(()));
            actions
        }
        Request::Configure {
            client_id,
            command,
            reply,
        } => {
            if *maintenance_reopen_pending
                || maintenance_owner.is_some_and(|owner| owner != client_id)
            {
                let _ = reply.send(Err(BrokerError::MaintenanceBusy));
                Vec::new()
            } else {
                match core.set_client_command(client_id, command) {
                    Some(actions) => {
                        let _ = reply.send(Ok(()));
                        actions
                    }
                    None => {
                        let _ = reply.send(Err(BrokerError::UnknownClient));
                        Vec::new()
                    }
                }
            }
        }
        Request::MaintenanceCommand {
            client_id,
            command,
            reply,
        } => {
            let snapshot = core.snapshot();
            let owner_allowed = maintenance_owner.is_none_or(|owner| owner == client_id);
            if !owner_allowed || snapshot.active_job_id.is_some() || snapshot.queued_jobs != 0 {
                let _ = reply.send(Err(BrokerError::MaintenanceBusy));
                Vec::new()
            } else if !snapshot.connected {
                let _ = reply.send(Err(BrokerError::NotConnected));
                Vec::new()
            } else {
                let first_command = maintenance_owner.is_none();
                *maintenance_owner = Some(client_id);
                let _ = reply.send(Ok(()));
                let mut actions = Vec::new();
                if first_command {
                    actions.push(PhysicalAction::Write(vec![0x0a, 0x0b, 0x00, 0x03]));
                }
                actions.push(PhysicalAction::Write(command));
                actions
            }
        }
        Request::ReleaseMaintenance { client_id } => {
            if *maintenance_owner == Some(client_id) {
                *maintenance_owner = None;
                *maintenance_reopen_pending = true;
                vec![PhysicalAction::Write(vec![0x00, 0x02])]
            } else {
                Vec::new()
            }
        }
        Request::ActiveOwnerCommand {
            client_id,
            command,
            reply,
        } => {
            if maintenance_owner.is_some() || *maintenance_reopen_pending {
                let _ = reply.send(Err(BrokerError::MaintenanceBusy));
                Vec::new()
            } else {
                match core.active_owner_command(client_id, command) {
                    Some(actions) => {
                        let _ = reply.send(Ok(()));
                        actions
                    }
                    None => {
                        let _ = reply.send(Err(BrokerError::Invalid(
                            "command requires active or primary ownership".to_string(),
                        )));
                        Vec::new()
                    }
                }
            }
        }
        Request::Shutdown { reply } => {
            let actions = core.emergency_stop("WinKeyer broker shutdown");
            let _ = apply_actions(actions, writer, events).await;
            let _ = reply.send(());
            return Ok(true);
        }
    };
    apply_actions(actions, writer, events).await?;
    Ok(false)
}

async fn apply_actions<W>(
    actions: Vec<PhysicalAction>,
    writer: &mut W,
    events: &broadcast::Sender<BrokerEvent>,
) -> Result<(), BrokerError>
where
    W: AsyncWrite + Unpin,
{
    for action in actions {
        match action {
            PhysicalAction::Write(bytes) => {
                writer.write_all(&bytes).await.map_err(transport_error)?;
            }
            PhysicalAction::Completed { job_id, client_id } => {
                let _ = events.send(BrokerEvent::Completed { job_id, client_id });
            }
            PhysicalAction::Canceled { job_id, client_id } => {
                let _ = events.send(BrokerEvent::Canceled { job_id, client_id });
            }
        }
    }
    writer.flush().await.map_err(transport_error)
}

fn publish_snapshot(core: &BrokerCore, snapshots: &watch::Sender<BrokerSnapshot>) {
    snapshots.send_if_modified(|current| {
        let next = core.snapshot();
        if *current == next {
            false
        } else {
            *current = next;
            true
        }
    });
}

fn release_station_tx_if_idle(core: &BrokerCore, ptt: Option<&PttManager>) {
    let snapshot = core.snapshot();
    if snapshot.active_job_id.is_none()
        && !snapshot.busy
        && !snapshot.break_in
        && !snapshot.key_down
    {
        if let Some(ptt) = ptt {
            ptt.unkey(WINKEYER_STATION_TX_OWNER);
        }
    }
}

#[allow(clippy::needless_pass_by_value)]
fn transport_error(error: std::io::Error) -> BrokerError {
    BrokerError::Transport(error.to_string())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    async fn spawn_fake() -> (BrokerHandle, tokio::io::DuplexStream) {
        let (mut device, broker) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move { spawn(broker, Duration::from_secs(30)).await });
        let mut open = [0_u8; 2];
        device.read_exact(&mut open).await.expect("host open");
        assert_eq!(open, [0x00, 0x02]);
        device.write_u8(31).await.expect("firmware");
        let handle = task.await.expect("spawn task").expect("broker");
        let mut initialization = [0_u8; 7];
        device
            .read_exact(&mut initialization)
            .await
            .expect("initialization");
        assert_eq!(initialization, [0x0a, 0x0b, 0x00, 0x02, 0x00, 0x07, 0x15]);
        (handle, device)
    }

    #[tokio::test]
    async fn opens_once_and_exposes_firmware_status() {
        let (handle, _device) = spawn_fake().await;
        assert!(handle.snapshot().connected);
        assert_eq!(handle.snapshot().firmware_revision, Some(31));
    }

    #[tokio::test]
    async fn typed_job_writes_speed_text_and_status_without_interleaving() {
        let (handle, mut device) = spawn_fake().await;
        handle.register(10, true).await.expect("register");
        assert_eq!(
            handle
                .enqueue(10, b"TEST".to_vec(), Some(SpeedMode::Fixed(22)))
                .await,
            Ok(1)
        );
        let mut bytes = [0_u8; 8];
        device.read_exact(&mut bytes).await.expect("job");
        assert_eq!(bytes, [0x02, 22, b'T', b'E', b'S', b'T', 0x15, 0x15]);
    }

    #[tokio::test]
    async fn pot_and_completion_events_are_fanned_out() {
        let (handle, mut device) = spawn_fake().await;
        handle.register(10, true).await.expect("register");
        let mut events = handle.subscribe();
        device.write_u8(0x94).await.expect("pot");
        let event = tokio::time::timeout(Duration::from_secs(1), events.recv())
            .await
            .expect("timely")
            .expect("event");
        assert_eq!(
            event,
            BrokerEvent::SpeedPot {
                raw: 0x94,
                value: 20,
                wpm: 25,
            }
        );
        assert_eq!(handle.snapshot().pot_value, Some(20));
        assert_eq!(handle.snapshot().pot_wpm, Some(25));
    }

    #[tokio::test]
    async fn shutdown_clears_key_and_closes_host_session() {
        let (handle, mut device) = spawn_fake().await;
        handle.shutdown().await.expect("shutdown");
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), device.read_to_end(&mut bytes))
            .await
            .expect("close")
            .expect("read");
        assert!(bytes
            .windows(5)
            .any(|window| window == [0x0a, 0x0b, 0x00, 0x00, 0x03]));
    }

    #[tokio::test]
    async fn station_ptt_owner_blocks_conflicting_winkeyer_transmit() {
        let (mut device, physical) = tokio::io::duplex(4096);
        let ptt = PttManager::new(Duration::from_secs(30));
        ptt.try_key(7, true).expect("CAT owner");
        let task = tokio::spawn({
            let ptt = ptt.clone();
            async move { spawn_with_ptt(physical, Duration::from_secs(30), ptt).await }
        });
        let mut open = [0_u8; 2];
        device.read_exact(&mut open).await.expect("host open");
        device.write_u8(31).await.expect("firmware");
        let handle = task.await.expect("task").expect("broker");
        let mut initialization = [0_u8; 7];
        device
            .read_exact(&mut initialization)
            .await
            .expect("initialization");
        handle.register(10, false).await.expect("register");

        assert_eq!(
            handle.enqueue(10, b"TEST".to_vec(), None).await,
            Err(BrokerError::TransmitBusy)
        );
        assert_eq!(ptt.owner(), Some(7));
    }

    #[tokio::test]
    async fn winkeyer_station_lease_releases_when_job_completes() {
        let (mut device, physical) = tokio::io::duplex(4096);
        let ptt = PttManager::new(Duration::from_secs(30));
        let task = tokio::spawn({
            let ptt = ptt.clone();
            async move { spawn_with_ptt(physical, Duration::from_secs(30), ptt).await }
        });
        let mut open = [0_u8; 2];
        device.read_exact(&mut open).await.expect("host open");
        device.write_u8(31).await.expect("firmware");
        let handle = task.await.expect("task").expect("broker");
        let mut initialization = [0_u8; 7];
        device
            .read_exact(&mut initialization)
            .await
            .expect("initialization");
        handle.register(10, false).await.expect("register");
        handle.enqueue(10, b"E".to_vec(), None).await.expect("send");
        assert_eq!(ptt.owner(), Some(WINKEYER_STATION_TX_OWNER));
        let mut writes = [0_u8; 4];
        device.read_exact(&mut writes).await.expect("writes");
        device.write_u8(0xc4).await.expect("busy");
        device.write_u8(0xc0).await.expect("idle");
        for _ in 0..50 {
            if ptt.owner().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(ptt.owner(), None);
    }

    #[tokio::test]
    async fn physical_paddle_activity_acquires_station_transmit_lease() {
        let (mut device, physical) = tokio::io::duplex(4096);
        let ptt = PttManager::new(Duration::from_secs(30));
        let task = tokio::spawn({
            let ptt = ptt.clone();
            async move { spawn_with_ptt(physical, Duration::from_secs(30), ptt).await }
        });
        let mut open = [0_u8; 2];
        device.read_exact(&mut open).await.expect("host open");
        device.write_u8(31).await.expect("firmware");
        let handle = task.await.expect("task").expect("broker");
        let mut initialization = [0_u8; 7];
        device
            .read_exact(&mut initialization)
            .await
            .expect("initialization");

        device.write_u8(0xc6).await.expect("paddle busy status");
        for _ in 0..50 {
            if ptt.owner() == Some(WINKEYER_STATION_TX_OWNER) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(ptt.owner(), Some(WINKEYER_STATION_TX_OWNER));
        assert_eq!(ptt.try_key(7, true), Err(PttDenied::Busy));
        handle.register(10, false).await.expect("client");
        assert_eq!(
            handle.enqueue(10, b"WAIT".to_vec(), None).await,
            Err(BrokerError::TransmitBusy)
        );

        device.write_u8(0xc0).await.expect("paddle idle status");
        for _ in 0..50 {
            if ptt.owner().is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(ptt.owner(), None);
    }

    #[tokio::test]
    async fn exclusive_maintenance_routes_private_replies_and_blocks_transmit() {
        let (handle, mut device) = spawn_fake().await;
        handle.register(10, true).await.expect("maintenance client");
        handle.register(20, false).await.expect("ordinary client");
        let mut events = handle.subscribe();

        handle
            .maintenance_command(10, vec![0x00, 0x0c])
            .await
            .expect("acquire maintenance");
        let mut command = [0_u8; 6];
        device
            .read_exact(&mut command)
            .await
            .expect("maintenance write");
        assert_eq!(command, [0x0a, 0x0b, 0x00, 0x03, 0x00, 0x0c]);
        assert_eq!(
            handle.enqueue(20, b"NO".to_vec(), None).await,
            Err(BrokerError::MaintenanceBusy)
        );

        device.write_u8(0x42).await.expect("private reply");
        assert_eq!(
            events.recv().await.expect("maintenance event"),
            BrokerEvent::MaintenanceByte {
                client_id: 10,
                byte: 0x42,
            }
        );

        handle
            .release_maintenance(10)
            .await
            .expect("release maintenance");
        let mut host_open = [0_u8; 2];
        device
            .read_exact(&mut host_open)
            .await
            .expect("host reopen");
        assert_eq!(host_open, [0x00, 0x02]);
        assert_eq!(
            handle.enqueue(20, b"WAIT".to_vec(), None).await,
            Err(BrokerError::MaintenanceBusy)
        );
        device
            .write_u8(31)
            .await
            .expect("firmware after maintenance");
        let mut restore = [0_u8; 9];
        device.read_exact(&mut restore).await.expect("safe restore");
        assert_eq!(
            restore,
            [0x0a, 0x0b, 0x00, 0x02, 0x00, 0x07, 0x15, 0x02, 0x00]
        );
        assert!(handle.enqueue(20, b"OK".to_vec(), None).await.is_ok());
    }

    #[tokio::test]
    async fn supervised_transport_reconnects_without_restarting_broker() {
        let (mut first_device, first_transport) = tokio::io::duplex(4096);
        let (replacement_tx, replacement_rx) = mpsc::channel(1);
        let replacement_rx = Arc::new(Mutex::new(replacement_rx));
        let task = tokio::spawn({
            let replacement_rx = replacement_rx.clone();
            async move {
                spawn_inner(
                    first_transport,
                    Duration::from_secs(30),
                    Some(PttManager::new(Duration::from_secs(30))),
                    true,
                    false,
                    move || {
                        let replacement_rx = replacement_rx.clone();
                        async move {
                            replacement_rx
                                .lock()
                                .await
                                .recv()
                                .await
                                .ok_or_else(|| io::Error::other("no replacement"))
                        }
                    },
                )
                .await
            }
        });
        let mut open = [0_u8; 2];
        first_device
            .read_exact(&mut open)
            .await
            .expect("first open");
        first_device.write_u8(31).await.expect("first firmware");
        let handle = task.await.expect("spawn task").expect("broker");
        let mut initialization = [0_u8; 7];
        first_device
            .read_exact(&mut initialization)
            .await
            .expect("first initialization");
        drop(first_device);

        for _ in 0..100 {
            if !handle.snapshot().connected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!handle.snapshot().connected);

        let (mut second_device, second_transport) = tokio::io::duplex(4096);
        replacement_tx
            .send(second_transport)
            .await
            .expect("replacement transport");
        second_device
            .read_exact(&mut open)
            .await
            .expect("second open");
        second_device.write_u8(32).await.expect("second firmware");
        let mut reconnect_initialization = [0_u8; 9];
        second_device
            .read_exact(&mut reconnect_initialization)
            .await
            .expect("reconnect initialization and foreground restore");
        assert_eq!(
            reconnect_initialization,
            [0x0a, 0x0b, 0x00, 0x02, 0x00, 0x07, 0x15, 0x02, 0x00]
        );
        for _ in 0..100 {
            if handle.snapshot().connected {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(handle.snapshot().connected);
        assert_eq!(handle.snapshot().firmware_revision, Some(32));
    }
}
