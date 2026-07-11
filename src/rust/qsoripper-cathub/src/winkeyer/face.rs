//! Virtual WinKeyer serial faces for unmodified applications such as N1MM Logger+.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;

use super::actor::{BrokerEvent, BrokerHandle};
use super::broker::{ClientId, SpeedMode};
use super::protocol::{command_policy, ClientItem, ClientParser, CommandPolicy};

const MAX_READ_CHUNK: usize = 512;
const PARTIAL_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Permissions assigned to one virtual WinKeyer face.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct FacePermissions {
    pub(crate) status: bool,
    pub(crate) send: bool,
    pub(crate) control: bool,
    pub(crate) ptt: bool,
    pub(crate) config_write: bool,
}

impl FacePermissions {
    pub(crate) fn from_tokens(tokens: &[String]) -> Self {
        let mut result = Self::default();
        for token in tokens {
            match token.as_str() {
                "status" => result.status = true,
                "send" => result.send = true,
                "control" => result.control = true,
                "ptt" => result.ptt = true,
                "config_write" => result.config_write = true,
                _ => {}
            }
        }
        result
    }
}

/// Serve one virtual WinKeyer client until it disconnects or attempts a forbidden
/// maintenance operation.
#[cfg(test)]
pub(crate) async fn run_face<T>(
    transport: T,
    broker: BrokerHandle,
    client_id: ClientId,
    primary: bool,
    permissions: FacePermissions,
) where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    run_face_inner(transport, broker, client_id, primary, permissions, false).await;
}

/// Serve a real serial face, where a zero-byte read means idle rather than EOF.
pub(crate) async fn run_serial_face<T>(
    transport: T,
    broker: BrokerHandle,
    client_id: ClientId,
    primary: bool,
    permissions: FacePermissions,
) where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    run_face_inner(transport, broker, client_id, primary, permissions, true).await;
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_face_inner<T>(
    transport: T,
    broker: BrokerHandle,
    client_id: ClientId,
    primary: bool,
    permissions: FacePermissions,
    zero_is_idle: bool,
) where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    if let Err(error) = broker.register(client_id, primary).await {
        tracing::error!(client_id, %error, "cannot register virtual WinKeyer face");
        return;
    }

    let (mut reader, mut writer) = tokio::io::split(transport);
    let mut events = broker.subscribe();
    let mut parser = ClientParser::default();
    let mut opened = false;
    let mut maintenance_active = false;
    let mut session_mode = SessionMode::Wk1;
    let mut chunk = [0_u8; MAX_READ_CHUNK];
    let mut parser_tick = tokio::time::interval(PARTIAL_COMMAND_TIMEOUT);
    let mut last_client_byte = std::time::Instant::now();

    'session: loop {
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let count = match read {
                    Ok(0) if zero_is_idle => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                        continue 'session;
                    }
                    Ok(0) | Err(_) => break 'session,
                    Ok(count) => count,
                };
                let mut stream = Vec::new();
                for &byte in chunk.get(..count).unwrap_or(&[]) {
                    last_client_byte = std::time::Instant::now();
                    let Some(item) = parser.push(byte) else { continue };
                    match item {
                        ClientItem::Data(byte) => {
                            if opened && permissions.send {
                                stream.push(byte);
                            }
                        }
                        ClientItem::Command(command) if command.first() == Some(&0x0a) => {
                            // Clear Buffer is immediate. Bytes in this same OS read that precede
                            // it have not reached hardware yet, so dropping them matches the
                            // client's intent without briefly keying stale text.
                            stream.clear();
                            if permissions.send {
                                let _ = broker.cancel(client_id, true).await;
                            }
                        }
                        ClientItem::Command(command) if is_buffered(&command) => {
                            if opened && permissions.send && buffered_allowed(&command, permissions) {
                                stream.extend(command);
                            }
                        }
                        ClientItem::Command(command) => {
                            if !stream.is_empty() {
                                let bytes = std::mem::take(&mut stream);
                                if broker.stream(client_id, bytes).await.is_err() {
                                    break 'session;
                                }
                            }
                            match handle_command(
                                &command,
                                &broker,
                                client_id,
                                permissions,
                                &mut opened,
                                &mut maintenance_active,
                                &mut session_mode,
                                &mut writer,
                            ).await {
                                CommandResult::Continue => {}
                                CommandResult::Close => break 'session,
                            }
                        }
                    }
                }
                if !stream.is_empty() && broker.stream(client_id, stream).await.is_err() {
                    break 'session;
                }
            }
            _ = parser_tick.tick() => {
                if parser.is_partial() && last_client_byte.elapsed() >= PARTIAL_COMMAND_TIMEOUT {
                    tracing::warn!(client_id, "discarding timed-out partial WinKeyer command");
                    parser.reset();
                }
            }
            event = events.recv(), if permissions.status && (opened || maintenance_active) => {
                match event {
                    Ok(event) => {
                        if let Some(byte) = event_byte(
                            &event,
                            &broker,
                            client_id,
                            primary,
                            session_mode,
                        ) {
                            if writer.write_u8(byte).await.is_err() {
                                break 'session;
                            }
                            let _ = writer.flush().await;
                        }
                    }
                    Err(RecvError::Lagged(_)) => {
                        let status = synthesize_status(&broker, session_mode);
                        if writer.write_u8(status).await.is_err() {
                            break 'session;
                        }
                        if let Some(pot) = broker.snapshot().pot_value {
                            if writer.write_u8(0x80 | (pot & 0x3f)).await.is_err() {
                                break 'session;
                            }
                        }
                        let _ = writer.flush().await;
                    }
                    Err(RecvError::Closed) => break 'session,
                }
            }
        }
    }

    parser.reset();
    let _ = broker.release_maintenance(client_id).await;
    let _ = broker.unregister(client_id).await;
}

enum CommandResult {
    Continue,
    Close,
}

#[derive(Debug, Clone, Copy)]
enum SessionMode {
    Wk1,
    Wk2,
    Wk3,
}

#[allow(clippy::too_many_arguments)]
async fn handle_command<W>(
    command: &[u8],
    broker: &BrokerHandle,
    client_id: ClientId,
    permissions: FacePermissions,
    opened: &mut bool,
    maintenance_active: &mut bool,
    session_mode: &mut SessionMode,
    writer: &mut W,
) -> CommandResult
where
    W: AsyncWrite + Unpin,
{
    if command.first() == Some(&0x00) {
        return handle_admin(
            command,
            broker,
            client_id,
            permissions,
            opened,
            maintenance_active,
            session_mode,
            writer,
        )
        .await;
    }
    if !*opened {
        return CommandResult::Continue;
    }

    match command.first().copied() {
        Some(0x02) if permissions.control => {
            if let Some(speed) = command.get(1).copied().and_then(parse_speed) {
                let _ = broker.set_speed(client_id, speed).await;
            }
        }
        Some(0x07) if permissions.status => {
            let byte = 0x80 | (broker.snapshot().pot_value.unwrap_or(0) & 0x3f);
            if writer.write_u8(byte).await.is_err() {
                return CommandResult::Close;
            }
        }
        Some(0x15) if permissions.status => {
            if writer
                .write_u8(synthesize_status(broker, *session_mode))
                .await
                .is_err()
            {
                return CommandResult::Close;
            }
        }
        Some(_) => match command_policy(command) {
            CommandPolicy::Transient if permissions.control => {
                let _ = broker.configure(client_id, command.to_vec()).await;
            }
            CommandPolicy::ActiveOwner
                if permissions.control && (command.first() != Some(&0x0b) || permissions.ptt) =>
            {
                let _ = broker
                    .active_owner_command(client_id, command.to_vec())
                    .await;
            }
            CommandPolicy::Maintenance => {
                if !permissions.config_write
                    || broker
                        .maintenance_command(client_id, command.to_vec())
                        .await
                        .is_err()
                {
                    tracing::warn!(client_id, command = ?command, "denied WinKeyer maintenance command");
                    return CommandResult::Close;
                }
                *maintenance_active = true;
            }
            _ => {}
        },
        None => {}
    }
    let _ = writer.flush().await;
    CommandResult::Continue
}

#[allow(clippy::too_many_arguments)]
async fn handle_admin<W>(
    command: &[u8],
    broker: &BrokerHandle,
    client_id: ClientId,
    permissions: FacePermissions,
    opened: &mut bool,
    maintenance_active: &mut bool,
    session_mode: &mut SessionMode,
    writer: &mut W,
) -> CommandResult
where
    W: AsyncWrite + Unpin,
{
    let Some(subcommand) = command.get(1).copied() else {
        return CommandResult::Continue;
    };
    match subcommand {
        0x02 if permissions.status => {
            *opened = true;
            *session_mode = SessionMode::Wk1;
            let revision = broker.snapshot().firmware_revision.unwrap_or(0xff);
            if writer.write_u8(revision).await.is_err() {
                return CommandResult::Close;
            }
        }
        0x03 => {
            *opened = false;
            let _ = broker.release_maintenance(client_id).await;
            *maintenance_active = false;
        }
        0x04 if permissions.status => {
            if let Some(value) = command.get(2) {
                if writer.write_u8(*value).await.is_err() {
                    return CommandResult::Close;
                }
            }
        }
        0x05..=0x07 | 0x15 | 0x17..=0x18 if permissions.status => {
            if writer.write_u8(0).await.is_err() {
                return CommandResult::Close;
            }
        }
        0x09 if permissions.status => {
            if writer
                .write_u8(broker.snapshot().firmware_revision.unwrap_or(0xff))
                .await
                .is_err()
            {
                return CommandResult::Close;
            }
        }
        0x0a if permissions.control => *session_mode = SessionMode::Wk1,
        0x0b if permissions.control => *session_mode = SessionMode::Wk2,
        0x14 if permissions.control => *session_mode = SessionMode::Wk3,
        0x0f | 0x13 | 0x16 | 0x19 if permissions.control => {
            let _ = broker.configure(client_id, command.to_vec()).await;
        }
        _ => {
            if !permissions.config_write
                || broker
                    .maintenance_command(client_id, command.to_vec())
                    .await
                    .is_err()
            {
                tracing::warn!(command = ?command, "virtual face attempted exclusive WinKeyer admin operation");
                return CommandResult::Close;
            }
            *maintenance_active = true;
        }
    }
    let _ = writer.flush().await;
    CommandResult::Continue
}

fn parse_speed(value: u8) -> Option<SpeedMode> {
    match value {
        0 => Some(SpeedMode::Pot),
        5..=99 => Some(SpeedMode::Fixed(value)),
        _ => None,
    }
}

fn is_buffered(command: &[u8]) -> bool {
    command
        .first()
        .is_some_and(|opcode| (0x18..=0x1f).contains(opcode))
}

fn buffered_allowed(command: &[u8], permissions: FacePermissions) -> bool {
    !matches!(command.first(), Some(0x18 | 0x19)) || permissions.ptt
}

fn synthesize_status(broker: &BrokerHandle, session_mode: SessionMode) -> u8 {
    let snapshot = broker.snapshot();
    0xc0 | match session_mode {
        SessionMode::Wk1 => u8::from(snapshot.key_down) << 3,
        SessionMode::Wk2 | SessionMode::Wk3 => 0,
    } | u8::from(snapshot.busy) << 2
        | u8::from(snapshot.break_in) << 1
}

fn event_byte(
    event: &BrokerEvent,
    broker: &BrokerHandle,
    client_id: ClientId,
    primary: bool,
    session_mode: SessionMode,
) -> Option<u8> {
    match event {
        BrokerEvent::SpeedPot { raw, .. } => Some(*raw),
        BrokerEvent::Status { .. } => Some(synthesize_status(broker, session_mode)),
        BrokerEvent::Echo(byte) => {
            let snapshot = broker.snapshot();
            (snapshot.active_client_id == Some(client_id) || (primary && snapshot.break_in))
                .then_some(*byte)
        }
        BrokerEvent::MaintenanceByte {
            client_id: owner,
            byte,
        } => (*owner == client_id).then_some(*byte),
        _ => None,
    }
}

/// Open the hub side of a virtual WinKeyer pair as 8-N-2.
pub(crate) fn open_serial_face(
    port_name: &str,
    baud: u32,
) -> std::io::Result<serial2_tokio::SerialPort> {
    serial2_tokio::SerialPort::open(port_name, move |mut settings: serial2_tokio::Settings| {
        settings.set_raw();
        settings.set_baud_rate(baud)?;
        settings.set_char_size(serial2_tokio::CharSize::Bits8);
        settings.set_parity(serial2_tokio::Parity::None);
        settings.set_stop_bits(serial2_tokio::StopBits::Two);
        settings.set_flow_control(serial2_tokio::FlowControl::None);
        Ok(settings)
    })
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::DuplexStream;

    async fn setup() -> (BrokerHandle, DuplexStream, DuplexStream) {
        setup_with_permissions(FacePermissions {
            status: true,
            send: true,
            control: true,
            ptt: true,
            config_write: false,
        })
        .await
    }

    async fn setup_with_permissions(
        permissions: FacePermissions,
    ) -> (BrokerHandle, DuplexStream, DuplexStream) {
        let (mut device, physical) = tokio::io::duplex(4096);
        let broker_task = tokio::spawn(async move {
            super::super::actor::spawn(physical, Duration::from_secs(30)).await
        });
        let mut host_open = [0_u8; 2];
        device.read_exact(&mut host_open).await.expect("open");
        device.write_u8(31).await.expect("revision");
        let broker = broker_task.await.expect("task").expect("broker");
        let mut initialization = [0_u8; 7];
        device.read_exact(&mut initialization).await.expect("init");

        let (client, face) = tokio::io::duplex(4096);
        tokio::spawn(run_face(face, broker.clone(), 42, true, permissions));
        (broker, client, device)
    }

    #[tokio::test]
    async fn host_open_is_virtualized_without_reopening_physical_keyer() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("open");
        assert_eq!(client.read_u8().await.expect("revision"), 31);
        let mut byte = [0_u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(50), device.read(&mut byte))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn fixed_speed_and_text_flow_through_the_broker() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("open");
        let _ = client.read_u8().await.expect("revision");
        client
            .write_all(&[0x02, 25, b'T', b'E'])
            .await
            .expect("send");

        let mut bytes = [0_u8; 9];
        device
            .read_exact(&mut bytes)
            .await
            .expect("physical writes");
        assert!(bytes.windows(2).any(|window| window == [0x02, 25]));
        assert!(bytes.windows(2).any(|window| window == b"TE"));
    }

    #[tokio::test]
    async fn n1mm_golden_startup_send_abort_and_close_transcript() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("Host Open");
        assert_eq!(client.read_u8().await.expect("revision"), 31);

        let mut defaults = vec![0x0f];
        defaults.extend([25, 50, 0, 0, 10, 20, 0, 0, 0, 50, 0, 0, 0, 0, 0]);
        let mut startup = vec![0x00, 0x0b]; // N1MM selects WK2 logical status mode.
        startup.extend(defaults.clone());
        startup.extend([0x05, 10, 20, 0, 0x02, 0, 0x07, 0x15]);
        client.write_all(&startup).await.expect("N1MM startup");

        let mut startup_writes = vec![0_u8; 22];
        device
            .read_exact(&mut startup_writes)
            .await
            .expect("transient startup writes");
        let mut expected = defaults.clone();
        expected.extend([0x05, 10, 20, 0, 0x02, 0]);
        assert_eq!(startup_writes, expected);
        assert_eq!(client.read_u8().await.expect("pot reply"), 0x80);
        assert_eq!(client.read_u8().await.expect("status reply"), 0xc0);

        client.write_all(b"TEST").await.expect("N1MM text");
        let mut job = vec![0_u8; 27];
        device
            .read_exact(&mut job)
            .await
            .expect("atomic physical job");
        assert_eq!(&job[..4], &[0x05, 10, 20, 0]);
        assert_eq!(&job[4..20], defaults.as_slice());
        assert_eq!(&job[20..22], &[0x02, 0]);
        assert_eq!(&job[22..26], b"TEST");
        assert_eq!(job[26], 0x15);

        client.write_u8(0x0a).await.expect("N1MM Escape");
        let mut abort = vec![0_u8; 23];
        device.read_exact(&mut abort).await.expect("scoped abort");
        assert_eq!(abort[0], 0x0a);
        assert_eq!(&abort[1..5], &[0x05, 10, 20, 0]);
        assert_eq!(&abort[5..21], defaults.as_slice());
        assert_eq!(&abort[21..23], &[0x02, 0]);

        client.write_all(&[0x00, 0x03]).await.expect("Host Close");
        let mut unexpected = [0_u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(50), device.read(&mut unexpected))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn physical_pot_event_is_fanned_to_open_virtual_session() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("open");
        let _ = client.read_u8().await.expect("revision");
        device.write_u8(0x9b).await.expect("pot");
        assert_eq!(client.read_u8().await.expect("pot event"), 0x9b);
    }

    #[tokio::test]
    async fn maintenance_command_fails_closed_and_does_not_reach_device() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("open");
        let _ = client.read_u8().await.expect("revision");
        client.write_all(&[0x00, 0x01]).await.expect("reset");
        let mut ignored = Vec::new();
        tokio::time::timeout(Duration::from_secs(1), client.read_to_end(&mut ignored))
            .await
            .expect("face closes")
            .expect("read");
        let mut byte = [0_u8; 1];
        assert!(
            tokio::time::timeout(Duration::from_millis(50), device.read(&mut byte))
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn permitted_maintenance_is_exclusive_and_private_until_virtual_close() {
        let (_broker, mut client, mut device) = setup_with_permissions(FacePermissions {
            status: true,
            send: true,
            control: true,
            ptt: true,
            config_write: true,
        })
        .await;

        client.write_all(&[0x00, 0x0c]).await.expect("EEPROM dump");
        let mut physical = [0_u8; 6];
        device
            .read_exact(&mut physical)
            .await
            .expect("safe close and dump command");
        assert_eq!(physical, [0x0a, 0x0b, 0x00, 0x03, 0x00, 0x0c]);
        device
            .write_all(&[0x80, 0xc0, 0x42])
            .await
            .expect("dump bytes");
        let mut private = [0_u8; 3];
        client
            .read_exact(&mut private)
            .await
            .expect("private maintenance response");
        assert_eq!(private, [0x80, 0xc0, 0x42]);

        client
            .write_all(&[0x00, 0x03])
            .await
            .expect("virtual close");
        let mut reopen = [0_u8; 2];
        device
            .read_exact(&mut reopen)
            .await
            .expect("physical reopen");
        assert_eq!(reopen, [0x00, 0x02]);
        device.write_u8(31).await.expect("firmware");
        let mut restore = [0_u8; 9];
        device.read_exact(&mut restore).await.expect("safe restore");
        assert_eq!(
            restore,
            [0x0a, 0x0b, 0x00, 0x02, 0x00, 0x07, 0x15, 0x02, 0x00]
        );
    }
}
