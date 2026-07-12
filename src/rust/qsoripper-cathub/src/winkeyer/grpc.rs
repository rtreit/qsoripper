//! Loopback gRPC surface for typed QsoRipper WinKeyer clients.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::wrappers::TcpListenerStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{Request, Response, Status};

use super::actor::{BrokerError, BrokerEvent, BrokerHandle};
use super::broker::{BrokerSnapshot, ClientId, SpeedMode, MAX_JOB_BYTES, MAX_QUEUED_JOBS};

use crate::broker_proto::winkeyer_broker_service_server::{
    WinkeyerBrokerService, WinkeyerBrokerServiceServer,
};
use crate::broker_proto::{
    WinkeyerBrokerEventKind, WinkeyerBrokerServiceAbortClientRequest,
    WinkeyerBrokerServiceAbortClientResponse, WinkeyerBrokerServiceCancelJobRequest,
    WinkeyerBrokerServiceCancelJobResponse, WinkeyerBrokerServiceGetStatusRequest,
    WinkeyerBrokerServiceGetStatusResponse, WinkeyerBrokerServiceSendTextRequest,
    WinkeyerBrokerServiceSendTextResponse, WinkeyerBrokerServiceSetSpeedRequest,
    WinkeyerBrokerServiceSetSpeedResponse, WinkeyerBrokerServiceStreamEventsRequest,
    WinkeyerBrokerServiceStreamEventsResponse, WinkeyerBrokerStatus, WinkeyerSpeedMode,
};

const FIRST_TYPED_CLIENT_ID: u64 = 1_000_000;
const MAX_TYPED_CLIENTS: usize = 64;

#[derive(Clone)]
struct Service {
    broker: BrokerHandle,
    clients: Arc<Mutex<BTreeMap<String, ClientId>>>,
    next_client: Arc<AtomicU64>,
}

impl Service {
    fn new(broker: BrokerHandle) -> Self {
        Self {
            broker,
            clients: Arc::new(Mutex::new(BTreeMap::new())),
            next_client: Arc::new(AtomicU64::new(FIRST_TYPED_CLIENT_ID)),
        }
    }

    async fn client_id(&self, name: &str) -> Result<ClientId, Status> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            return Err(Status::invalid_argument(
                "client_name must contain 1 through 64 characters",
            ));
        }
        let mut clients = self.clients.lock().await;
        if let Some(id) = clients.get(name) {
            return Ok(*id);
        }
        if clients.len() >= MAX_TYPED_CLIENTS {
            return Err(Status::resource_exhausted(
                "maximum number of named WinKeyer clients reached",
            ));
        }
        let id = self.next_client.fetch_add(1, Ordering::SeqCst);
        self.broker.register(id, false).await.map_err(map_error)?;
        clients.insert(name.to_string(), id);
        Ok(id)
    }
}

type EventStream =
    Pin<Box<dyn Stream<Item = Result<WinkeyerBrokerServiceStreamEventsResponse, Status>> + Send>>;

#[tonic::async_trait]
impl WinkeyerBrokerService for Service {
    type StreamEventsStream = EventStream;

    async fn get_status(
        &self,
        request: Request<WinkeyerBrokerServiceGetStatusRequest>,
    ) -> Result<Response<WinkeyerBrokerServiceGetStatusResponse>, Status> {
        self.client_id(&request.get_ref().client_name).await?;
        Ok(Response::new(WinkeyerBrokerServiceGetStatusResponse {
            status: Some(status_message(&self.broker.snapshot())),
        }))
    }

    async fn send_text(
        &self,
        request: Request<WinkeyerBrokerServiceSendTextRequest>,
    ) -> Result<Response<WinkeyerBrokerServiceSendTextResponse>, Status> {
        let request = request.into_inner();
        let client_id = self.client_id(&request.client_name).await?;
        let text = validate_text(&request.text)?;
        let speed = optional_speed(request.speed_mode, request.speed_wpm)?;
        let job_id = self
            .broker
            .enqueue(client_id, text, speed)
            .await
            .map_err(map_error)?;
        Ok(Response::new(WinkeyerBrokerServiceSendTextResponse {
            job_id,
        }))
    }

    async fn cancel_job(
        &self,
        request: Request<WinkeyerBrokerServiceCancelJobRequest>,
    ) -> Result<Response<WinkeyerBrokerServiceCancelJobResponse>, Status> {
        let request = request.into_inner();
        let client_id = self.client_id(&request.client_name).await?;
        let canceled = self
            .broker
            .cancel_job(client_id, request.job_id)
            .await
            .map_err(map_error)?;
        Ok(Response::new(WinkeyerBrokerServiceCancelJobResponse {
            canceled,
        }))
    }

    async fn abort_client(
        &self,
        request: Request<WinkeyerBrokerServiceAbortClientRequest>,
    ) -> Result<Response<WinkeyerBrokerServiceAbortClientResponse>, Status> {
        let request = request.into_inner();
        let client_id = self.client_id(&request.client_name).await?;
        if request.emergency_station_stop {
            self.broker
                .emergency_stop(format!(
                    "emergency stop requested by {}",
                    request.client_name
                ))
                .await
                .map_err(map_error)?;
        } else {
            self.broker
                .cancel(client_id, true)
                .await
                .map_err(map_error)?;
        }
        Ok(Response::new(WinkeyerBrokerServiceAbortClientResponse {
            accepted: true,
        }))
    }

    async fn set_speed(
        &self,
        request: Request<WinkeyerBrokerServiceSetSpeedRequest>,
    ) -> Result<Response<WinkeyerBrokerServiceSetSpeedResponse>, Status> {
        let request = request.into_inner();
        let client_id = self.client_id(&request.client_name).await?;
        let speed = required_speed(request.speed_mode, request.speed_wpm)?;
        self.broker
            .set_speed(client_id, speed)
            .await
            .map_err(map_error)?;
        let (speed_mode, speed_wpm) = speed_fields(speed);
        Ok(Response::new(WinkeyerBrokerServiceSetSpeedResponse {
            speed_mode,
            speed_wpm,
        }))
    }

    async fn stream_events(
        &self,
        request: Request<WinkeyerBrokerServiceStreamEventsRequest>,
    ) -> Result<Response<Self::StreamEventsStream>, Status> {
        self.client_id(&request.get_ref().client_name).await?;
        let broker = self.broker.clone();
        let stream = BroadcastStream::new(self.broker.subscribe()).filter_map(move |event| {
            let broker = broker.clone();
            match event {
                Ok(BrokerEvent::MaintenanceByte { .. } | BrokerEvent::Echo(_)) => None,
                Ok(event) => Some(Ok(event_message(&event, &broker.snapshot()))),
                Err(_) => Some(Ok(WinkeyerBrokerServiceStreamEventsResponse {
                    kind: WinkeyerBrokerEventKind::Status as i32,
                    status: Some(status_message(&broker.snapshot())),
                    message: Some("event receiver lagged; status resynchronized".to_string()),
                    ..Default::default()
                })),
            }
        });
        Ok(Response::new(Box::pin(stream)))
    }
}

#[cfg(test)]
pub(crate) async fn run_server(
    bind: SocketAddr,
    broker: BrokerHandle,
) -> Result<(), tonic::transport::Error> {
    tonic::transport::Server::builder()
        .add_service(WinkeyerBrokerServiceServer::new(Service::new(broker)))
        .serve(bind)
        .await
}

/// Bind the loopback endpoint before returning so daemon startup fails loudly on conflicts.
pub(crate) async fn bind_server(
    bind: SocketAddr,
    broker: BrokerHandle,
) -> std::io::Result<tokio::task::JoinHandle<Result<(), tonic::transport::Error>>> {
    let listener = tokio::net::TcpListener::bind(bind).await?;
    Ok(tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(WinkeyerBrokerServiceServer::new(Service::new(broker)))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
    }))
}

#[allow(clippy::result_large_err)]
fn validate_text(text: &str) -> Result<Vec<u8>, Status> {
    if text.is_empty() {
        return Err(Status::invalid_argument("text must not be empty"));
    }
    if text.len() > MAX_JOB_BYTES {
        return Err(Status::invalid_argument(format!(
            "text must not exceed {MAX_JOB_BYTES} ASCII bytes"
        )));
    }
    if let Some(character) = text.chars().find(|character| {
        !character.is_ascii() || (character.is_ascii_control() && *character != '\t')
    }) {
        return Err(Status::invalid_argument(format!(
            "text contains unsupported character {character:?}"
        )));
    }
    Ok(text.to_ascii_uppercase().into_bytes())
}

#[allow(clippy::result_large_err)]
fn optional_speed(mode: i32, speed_wpm: Option<u32>) -> Result<Option<SpeedMode>, Status> {
    if WinkeyerSpeedMode::try_from(mode).unwrap_or_default() == WinkeyerSpeedMode::Unspecified {
        if speed_wpm.is_none() {
            return Ok(None);
        }
        return required_speed(WinkeyerSpeedMode::Fixed as i32, speed_wpm).map(Some);
    }
    required_speed(mode, speed_wpm).map(Some)
}

#[allow(clippy::result_large_err)]
fn required_speed(mode: i32, speed_wpm: Option<u32>) -> Result<SpeedMode, Status> {
    match WinkeyerSpeedMode::try_from(mode).unwrap_or_default() {
        WinkeyerSpeedMode::Pot => Ok(SpeedMode::Pot),
        WinkeyerSpeedMode::Fixed => {
            let speed = speed_wpm
                .ok_or_else(|| Status::invalid_argument("fixed speed mode requires speed_wpm"))?;
            let speed = u8::try_from(speed)
                .ok()
                .filter(|speed| (5..=99).contains(speed))
                .ok_or_else(|| Status::invalid_argument("speed_wpm must be between 5 and 99"))?;
            Ok(SpeedMode::Fixed(speed))
        }
        WinkeyerSpeedMode::Unspecified => {
            Err(Status::invalid_argument("speed_mode must be POT or FIXED"))
        }
    }
}

fn speed_fields(speed: SpeedMode) -> (i32, Option<u32>) {
    match speed {
        SpeedMode::Pot => (WinkeyerSpeedMode::Pot as i32, None),
        SpeedMode::Fixed(wpm) => (WinkeyerSpeedMode::Fixed as i32, Some(u32::from(wpm))),
    }
}

fn status_message(snapshot: &BrokerSnapshot) -> WinkeyerBrokerStatus {
    WinkeyerBrokerStatus {
        connected: snapshot.connected,
        firmware_revision: snapshot.firmware_revision.map(u32::from),
        busy: snapshot.busy,
        break_in: snapshot.break_in,
        key_down: snapshot.key_down,
        pot_wpm: snapshot.pot_wpm.map(u32::from),
        active_client_id: snapshot.active_client_id,
        active_job_id: snapshot.active_job_id,
        queued_jobs: u32::try_from(snapshot.queued_jobs).unwrap_or(u32::MAX),
        last_error: snapshot.last_error.clone(),
        last_safety_action: snapshot.last_safety_action.clone(),
        max_job_bytes: u32::try_from(MAX_JOB_BYTES).unwrap_or(u32::MAX),
        supports_speed_pot: true,
        supports_scoped_cancel: true,
        max_queued_jobs: u32::try_from(MAX_QUEUED_JOBS).unwrap_or(u32::MAX),
    }
}

fn event_message(
    event: &BrokerEvent,
    snapshot: &BrokerSnapshot,
) -> WinkeyerBrokerServiceStreamEventsResponse {
    let mut response = WinkeyerBrokerServiceStreamEventsResponse {
        status: Some(status_message(snapshot)),
        ..Default::default()
    };
    match event {
        BrokerEvent::Connected { firmware_revision } => {
            response.kind = WinkeyerBrokerEventKind::Connected as i32;
            response.raw_byte = Some(u32::from(*firmware_revision));
        }
        BrokerEvent::SpeedPot { raw, wpm, .. } => {
            response.kind = WinkeyerBrokerEventKind::SpeedPot as i32;
            response.raw_byte = Some(u32::from(*raw));
            response.speed_wpm = Some(u32::from(*wpm));
        }
        BrokerEvent::Status { raw } => {
            response.kind = WinkeyerBrokerEventKind::Status as i32;
            response.raw_byte = Some(u32::from(*raw));
        }
        BrokerEvent::Echo(byte) => {
            response.kind = WinkeyerBrokerEventKind::Echo as i32;
            response.raw_byte = Some(u32::from(*byte));
        }
        BrokerEvent::Completed { job_id, client_id } => {
            response.kind = WinkeyerBrokerEventKind::JobCompleted as i32;
            response.job_id = Some(*job_id);
            response.client_id = Some(*client_id);
        }
        BrokerEvent::Canceled { job_id, client_id } => {
            response.kind = WinkeyerBrokerEventKind::JobCanceled as i32;
            response.job_id = Some(*job_id);
            response.client_id = Some(*client_id);
        }
        BrokerEvent::Error(message) => {
            response.kind = WinkeyerBrokerEventKind::Error as i32;
            response.message = Some(message.clone());
        }
        BrokerEvent::MaintenanceByte { .. } => {
            debug_assert!(
                false,
                "private maintenance bytes must not reach typed streams"
            );
        }
    }
    response
}

#[allow(clippy::needless_pass_by_value)]
fn map_error(error: BrokerError) -> Status {
    match error {
        BrokerError::UnknownClient
        | BrokerError::Invalid(_)
        | BrokerError::TransmitBusy
        | BrokerError::MaintenanceBusy => Status::failed_precondition(error.to_string()),
        BrokerError::PrimaryExists => Status::already_exists(error.to_string()),
        BrokerError::QueueFull => Status::resource_exhausted(error.to_string()),
        BrokerError::NotConnected | BrokerError::Unavailable | BrokerError::Transport(_) => {
            Status::unavailable(error.to_string())
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn speed_validation_distinguishes_pot_fixed_and_unspecified() {
        assert_eq!(
            required_speed(WinkeyerSpeedMode::Pot as i32, None).expect("pot"),
            SpeedMode::Pot
        );
        assert_eq!(
            required_speed(WinkeyerSpeedMode::Fixed as i32, Some(25)).expect("fixed"),
            SpeedMode::Fixed(25)
        );
        assert!(required_speed(WinkeyerSpeedMode::Fixed as i32, Some(100)).is_err());
        assert_eq!(optional_speed(0, None).expect("optional"), None);
    }

    #[test]
    fn typed_text_is_uppercase_ascii_and_rejects_control_data() {
        assert_eq!(validate_text("cq test").expect("text"), b"CQ TEST".to_vec());
        assert!(validate_text("").is_err());
        assert!(validate_text("CQ\nTEST").is_err());
        assert!(validate_text("CQ é").is_err());
    }

    #[tokio::test]
    async fn grpc_surface_reports_and_queues_against_physical_actor() {
        let (mut device, physical) = tokio::io::duplex(4096);
        let actor_task = tokio::spawn(async move {
            super::super::actor::spawn(physical, Duration::from_secs(30)).await
        });
        let mut host_open = [0_u8; 2];
        device.read_exact(&mut host_open).await.expect("host open");
        device.write_u8(31).await.expect("revision");
        let broker = actor_task.await.expect("actor task").expect("actor");
        let mut initialization = [0_u8; 7];
        device
            .read_exact(&mut initialization)
            .await
            .expect("initialization");

        let reserved = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve port");
        let address = reserved.local_addr().expect("address");
        drop(reserved);
        let server = tokio::spawn(run_server(address, broker));
        let endpoint = format!("http://{address}");
        let mut client = loop {
            match crate::broker_proto::winkeyer_broker_service_client::WinkeyerBrokerServiceClient::connect(
                endpoint.clone(),
            )
            .await
            {
                Ok(client) => break client,
                Err(_) => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };

        let status = client
            .get_status(WinkeyerBrokerServiceGetStatusRequest {
                client_name: "integration".to_string(),
            })
            .await
            .expect("status")
            .into_inner()
            .status
            .expect("payload");
        assert!(status.connected);
        assert_eq!(status.firmware_revision, Some(31));

        let response = client
            .send_text(WinkeyerBrokerServiceSendTextRequest {
                client_name: "integration".to_string(),
                text: "test".to_string(),
                speed_mode: WinkeyerSpeedMode::Fixed as i32,
                speed_wpm: Some(21),
            })
            .await
            .expect("send")
            .into_inner();
        assert_eq!(response.job_id, 1);
        let mut physical_write = [0_u8; 7];
        device
            .read_exact(&mut physical_write)
            .await
            .expect("physical write");
        assert_eq!(physical_write, [0x02, 21, b'T', b'E', b'S', b'T', 0x15]);
        server.abort();
    }
}
