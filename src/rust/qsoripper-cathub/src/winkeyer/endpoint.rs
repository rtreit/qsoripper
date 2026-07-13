//! Virtual WinKeyer serial endpoints for unmodified applications such as N1MM Logger+.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::broadcast::error::RecvError;

use super::actor::{BrokerEvent, BrokerHandle};
use super::broker::{ClientId, SpeedMode};
use super::protocol::{command_policy, ClientItem, ClientParser, CommandPolicy};

const MAX_READ_CHUNK: usize = 512;
const PARTIAL_COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// Permissions assigned to one virtual WinKeyer endpoint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct EndpointPermissions {
    pub(crate) status: bool,
    pub(crate) send: bool,
    pub(crate) control: bool,
    pub(crate) ptt: bool,
    pub(crate) config_write: bool,
}

impl EndpointPermissions {
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
pub(crate) async fn run_endpoint_session<T>(
    transport: T,
    broker: BrokerHandle,
    client_id: ClientId,
    primary: bool,
    permissions: EndpointPermissions,
) where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    run_endpoint_session_inner(transport, broker, client_id, primary, permissions, false).await;
}

/// Serve a real serial endpoint, where a zero-byte read means idle rather than EOF.
pub(crate) async fn run_serial_endpoint<T>(
    transport: T,
    broker: BrokerHandle,
    client_id: ClientId,
    primary: bool,
    permissions: EndpointPermissions,
) where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    run_endpoint_session_inner(transport, broker, client_id, primary, permissions, true).await;
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_endpoint_session_inner<T>(
    transport: T,
    broker: BrokerHandle,
    client_id: ClientId,
    primary: bool,
    permissions: EndpointPermissions,
    zero_is_idle: bool,
) where
    T: AsyncRead + AsyncWrite + Send + 'static,
{
    if let Err(error) = broker.register(client_id, primary).await {
        tracing::error!(client_id, %error, "cannot register virtual WinKeyer endpoint");
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
                    Ok(0) => {
                        tracing::debug!(client_id, "virtual WinKeyer endpoint reached EOF");
                        break 'session;
                    }
                    Err(error) => {
                        tracing::warn!(client_id, %error, "virtual WinKeyer endpoint read failed");
                        break 'session;
                    }
                    Ok(count) => count,
                };
                let bytes = chunk.get(..count).unwrap_or(&[]);
                tracing::trace!(client_id, bytes = ?bytes, "WinKeyer virtual rx");
                let mut buffered_prefix = Vec::new();
                let mut intended_text = Vec::new();
                for &byte in bytes {
                    last_client_byte = std::time::Instant::now();
                    let Some(item) = parser.push(byte) else { continue };
                    match item {
                        ClientItem::Data(byte) => {
                            if opened && permissions.send {
                                tracing::trace!(client_id, byte, "WinKeyer virtual data byte");
                                intended_text.push(byte);
                            }
                        }
                        ClientItem::Command(command) if command.first() == Some(&0x0a) => {
                            tracing::trace!(client_id, command = ?command, "WinKeyer virtual clear-buffer command");
                            // Clear Buffer is immediate. Bytes in this same OS read that precede
                            // it have not reached hardware yet, so dropping them matches the
                            // client's intent without briefly keying stale text.
                            buffered_prefix.clear();
                            intended_text.clear();
                            if permissions.send {
                                let _ = broker.cancel(client_id, true).await;
                            }
                        }
                        ClientItem::Command(command) if is_buffered(&command) => {
                            tracing::trace!(client_id, command = ?command, "WinKeyer virtual buffered command");
                            if opened && permissions.send && buffered_allowed(&command, permissions) {
                                if !intended_text.is_empty()
                                    && flush_stream(
                                        &broker,
                                        client_id,
                                        &mut buffered_prefix,
                                        &mut intended_text,
                                    )
                                    .await
                                    .is_err()
                                {
                                    break 'session;
                                }
                                buffered_prefix.extend(command);
                            }
                        }
                        ClientItem::Command(command) => {
                            tracing::trace!(client_id, command = ?command, "WinKeyer virtual command");
                            if flush_stream(
                                &broker,
                                client_id,
                                &mut buffered_prefix,
                                &mut intended_text,
                            )
                            .await
                            .is_err()
                            {
                                break 'session;
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
                if flush_stream(
                    &broker,
                    client_id,
                    &mut buffered_prefix,
                    &mut intended_text,
                )
                .await
                .is_err()
                {
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

async fn flush_stream(
    broker: &BrokerHandle,
    client_id: ClientId,
    buffered_prefix: &mut Vec<u8>,
    intended_text: &mut Vec<u8>,
) -> Result<(), ()> {
    if buffered_prefix.is_empty() && intended_text.is_empty() {
        return Ok(());
    }
    let control_prefix = std::mem::take(buffered_prefix);
    let text = std::mem::take(intended_text);
    tracing::trace!(
        client_id,
        intended_text = %String::from_utf8_lossy(&text),
        control_prefix = ?control_prefix,
        "WinKeyer virtual stream flush"
    );
    broker
        .stream(client_id, control_prefix, text)
        .await
        .map(|_| ())
        .map_err(|_| ())
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
    permissions: EndpointPermissions,
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
    permissions: EndpointPermissions,
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
                tracing::warn!(command = ?command, "virtual endpoint attempted exclusive WinKeyer admin operation");
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

fn buffered_allowed(command: &[u8], permissions: EndpointPermissions) -> bool {
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
pub(crate) fn open_serial_endpoint(
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
        setup_with_permissions(EndpointPermissions {
            status: true,
            send: true,
            control: true,
            ptt: true,
            config_write: false,
        })
        .await
    }

    async fn setup_with_permissions(
        permissions: EndpointPermissions,
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

        let (client, server) = tokio::io::duplex(4096);
        tokio::spawn(run_endpoint_session(
            server,
            broker.clone(),
            42,
            true,
            permissions,
        ));
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
    async fn n1mm_buffer_pointer_commands_are_not_replayed_before_keyboard_text() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("Host Open");
        let _ = client.read_u8().await.expect("revision");

        // Captured from N1MM when Ctrl+K opens: clear, pointer start, pointer move,
        // then its buffered-speed command. The pointer operations must reach the keyer
        // exactly once and must not become persistent profile entries.
        client
            .write_all(&[0x0a, 0x16, 0x00, 0x16, 0x02, 0x00, 0x1c, 23])
            .await
            .expect("N1MM keyboard CW prefix");

        let mut prefix = [0_u8; 9];
        device
            .read_exact(&mut prefix)
            .await
            .expect("physical keyboard prefix");
        assert_eq!(prefix, [0x0a, 0x16, 0x00, 0x16, 0x02, 0x00, 0x1c, 23, 0x15]);

        // Let the speed-only stream reach its idle boundary, as it did in the live trace,
        // then type one T. A replayed 0x16 here was the corruption regression.
        device.write_u8(0xc0).await.expect("idle status");
        tokio::time::sleep(Duration::from_millis(150)).await;
        let mut pending = [0_u8; 16];
        // Drain any pending buffered data with a bounded timeout to avoid spinning forever.
        let drain_start = tokio::time::Instant::now();
        while drain_start.elapsed() < Duration::from_millis(100) {
            match tokio::time::timeout(Duration::from_millis(10), device.read(&mut pending)).await {
                Ok(Ok(_)) => {}
                _ => break,
            }
        }
        client.write_u8(b'T').await.expect("keyboard T");

        // A foreground-speed restore can race with this new byte at the idle boundary.
        // The regression contract is that no buffered-pointer command is replayed before
        // the keyboard text, regardless of which side of that boundary accepts the T.
        let mut before_text = Vec::new();
        let mut saw_text = false;
        for _ in 0..8 {
            let byte = tokio::time::timeout(Duration::from_secs(1), device.read_u8())
                .await
                .expect("physical keyboard text timed out")
                .expect("physical keyboard text");
            if byte == b'T' {
                saw_text = true;
                break;
            }
            before_text.push(byte);
        }
        assert!(saw_text, "keyboard text was not forwarded: {before_text:?}");
        assert!(
            !before_text.contains(&0x16),
            "buffer pointer was replayed before keyboard text: {before_text:?}"
        );
    }

    #[tokio::test]
    async fn n1mm_function_key_message_reaches_physical_keyer_byte_for_byte() {
        let (_broker, mut client, mut device) = setup().await;
        client.write_all(&[0x00, 0x02]).await.expect("Host Open");
        let _ = client.read_u8().await.expect("revision");

        let message = b"TEST DE KC7AVA";
        let mut captured = vec![0x0a, 0x16, 0x00, 0x16, 0x02, 0x00, 0x1c, 23];
        captured.extend_from_slice(message);
        client
            .write_all(&captured)
            .await
            .expect("N1MM F-key message");

        let mut expected = captured;
        expected.push(0x15); // Broker status poll after the complete message.
        let mut physical = vec![0_u8; expected.len()];
        device
            .read_exact(&mut physical)
            .await
            .expect("physical F-key message");
        assert_eq!(physical, expected);
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

        client
            .write_all(&[
                0x16, 0x00, 0x16, 0x02, 0x00, 0x1c, 22, b'T', b'E', b'S', b'T',
            ])
            .await
            .expect("N1MM buffered speed and text");
        let mut job = vec![0_u8; 12];
        device
            .read_exact(&mut job)
            .await
            .expect("atomic physical job");
        assert_eq!(&job[..5], &[0x16, 0x00, 0x16, 0x02, 0x00]);
        assert_eq!(&job[5..7], &[0x1c, 22]);
        assert_eq!(&job[7..11], b"TEST");
        assert_eq!(job[11], 0x15);

        client.write_u8(0x0a).await.expect("N1MM Escape");
        let mut abort = vec![0_u8; 3];
        device.read_exact(&mut abort).await.expect("scoped abort");
        assert_eq!(abort[0], 0x0a);
        assert_eq!(&abort[1..3], &[0x02, 0]);

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
            .expect("session closes")
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
        let (_broker, mut client, mut device) = setup_with_permissions(EndpointPermissions {
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
