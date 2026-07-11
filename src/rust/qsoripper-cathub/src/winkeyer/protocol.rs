//! WinKeyer 3 host-protocol framing and device-event classification.
//!
//! Commands occupy byte values `0x00..=0x1f`; printable bytes are Morse data. Command
//! lengths are defined by the K1EL WinKeyer 3.1 interface manual. The parser retains a
//! partial command across serial reads and emits data immediately so a virtual face can
//! preserve the client's byte order without using timing to distinguish commands.

/// Maximum bytes in one WinKeyer command, including a 256-byte EEPROM payload.
const MAX_COMMAND_LEN: usize = 258;

/// One complete item received from a WinKeyer host client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientItem {
    /// One printable/prosign byte to place in the transmit stream.
    Data(u8),
    /// One complete immediate or buffered command, including its parameters.
    Command(Vec<u8>),
}

/// Incremental parser for bytes sent by a host application.
#[derive(Debug, Default)]
pub(crate) struct ClientParser {
    command: Vec<u8>,
    expected_len: Option<usize>,
}

impl ClientParser {
    /// Feed one byte and return an item when it becomes complete.
    pub(crate) fn push(&mut self, byte: u8) -> Option<ClientItem> {
        if self.command.is_empty() {
            if byte > 0x1f {
                return Some(ClientItem::Data(byte));
            }
            self.command.push(byte);
            self.expected_len = initial_command_len(byte);
        } else {
            self.command.push(byte);
            self.refine_variable_length();
        }

        if self.expected_len == Some(self.command.len()) {
            self.expected_len = None;
            let command = std::mem::take(&mut self.command);
            // N1MM Logger+ prefixes its buffered-speed command with an extra Admin byte.
            // Physical WinKeyer implementations tolerate this de facto wire sequence.
            // Normalize only this exact quirk; other invalid Admin commands remain intact
            // for the face policy to reject.
            let command = if command.starts_with(&[0x00, 0x1c]) {
                command.into_iter().skip(1).collect()
            } else {
                command
            };
            return Some(ClientItem::Command(command));
        }
        None
    }

    /// Discard an incomplete command after a client disconnect or malformed stream.
    pub(crate) fn reset(&mut self) {
        self.command.clear();
        self.expected_len = None;
    }

    pub(crate) fn is_partial(&self) -> bool {
        !self.command.is_empty()
    }

    fn refine_variable_length(&mut self) {
        let Some(&opcode) = self.command.first() else {
            return;
        };
        if opcode == 0x00 && self.command.len() == 2 {
            if let Some(admin) = self.command.get(1).copied() {
                self.expected_len = Some(if admin == 0x1c {
                    // N1MM's extra Admin prefix plus buffered-speed command and WPM.
                    3
                } else {
                    admin_command_len(admin)
                });
            }
        } else if opcode == 0x16 && self.command.len() == 2 && self.command.get(1) == Some(&0x03) {
            self.expected_len = Some(3);
        }
        debug_assert!(
            self.expected_len.unwrap_or(MAX_COMMAND_LEN) <= MAX_COMMAND_LEN,
            "WinKeyer command length must remain bounded"
        );
    }
}

fn initial_command_len(opcode: u8) -> Option<usize> {
    Some(match opcode {
        // Admin and Pointer are refined after their subcommand arrives.
        0x00..=0x03
        | 0x06
        | 0x09
        | 0x0b..=0x0e
        | 0x10..=0x12
        | 0x14
        | 0x16..=0x1a
        | 0x1c..=0x1d => 2,
        0x04 | 0x1b => 3,
        0x05 => 4,
        0x07..=0x08 | 0x0a | 0x13 | 0x15 | 0x1e..=0x1f => 1,
        0x0f => 16,
        _ => return None,
    })
}

fn admin_command_len(subcommand: u8) -> usize {
    match subcommand {
        0x00 | 0x04 | 0x0e..=0x0f | 0x16 | 0x19 => 3,
        0x0d => 258,
        0x13 => 4,
        _ => 2,
    }
}

/// A byte received from the physical WinKeyer, classified by its tag bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DeviceEvent {
    /// Speed-pot notification. The low six bits carry the reported speed value.
    SpeedPot { raw: u8, value: u8 },
    /// WinKeyer status notification or requested status reply.
    Status(DeviceStatus),
    /// Serial or paddle echo byte.
    Echo(u8),
}

impl DeviceEvent {
    /// Classify one byte from the physical keyer's transmit stream.
    pub(crate) fn from_byte(byte: u8) -> Self {
        match byte & 0xc0 {
            0x80 => Self::SpeedPot {
                raw: byte,
                value: byte & 0x3f,
            },
            0xc0 => Self::Status(DeviceStatus::from_byte(byte)),
            _ => Self::Echo(byte),
        }
    }
}

/// Decoded common bits of a WinKeyer status byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct DeviceStatus {
    /// Original byte, retained for protocol-compatible virtual faces.
    pub(crate) raw: u8,
    /// Timed wait in progress.
    pub(crate) waiting: bool,
    /// Key output asserted by tune/key-immediate.
    pub(crate) key_down: bool,
    /// Morse is being sent or remains buffered.
    pub(crate) busy: bool,
    /// Physical paddle break-in is active.
    pub(crate) break_in: bool,
    /// Input buffer is over two-thirds full.
    pub(crate) xoff: bool,
}

impl DeviceStatus {
    fn from_byte(raw: u8) -> Self {
        Self {
            raw,
            waiting: raw & 0x10 != 0,
            key_down: raw & 0x08 != 0,
            busy: raw & 0x04 != 0,
            break_in: raw & 0x02 != 0,
            xoff: raw & 0x01 != 0,
        }
    }
}

/// Operations which must not be forwarded from an ordinary virtual session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommandPolicy {
    /// The broker answers this command without touching the physical host lifecycle.
    Virtualized,
    /// Safe transient command, scoped to the client or its active transmit stream.
    Transient,
    /// Global buffer/keying operation requiring active-owner arbitration.
    ActiveOwner,
    /// Persistent or disruptive operation requiring an exclusive maintenance lease.
    Maintenance,
}

/// Classify a complete command for permission and ownership enforcement.
pub(crate) fn command_policy(command: &[u8]) -> CommandPolicy {
    let Some(&opcode) = command.first() else {
        return CommandPolicy::Maintenance;
    };
    if opcode == 0x00 {
        return match command.get(1).copied() {
            Some(0x02..=0x07 | 0x09..=0x0b | 0x14..=0x15 | 0x17..=0x18) => {
                CommandPolicy::Virtualized
            }
            _ => CommandPolicy::Maintenance,
        };
    }
    match opcode {
        0x0a..=0x0b | 0x14 | 0x18..=0x1a => CommandPolicy::ActiveOwner,
        _ => CommandPolicy::Transient,
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn parse(bytes: &[u8]) -> Vec<ClientItem> {
        let mut parser = ClientParser::default();
        bytes.iter().filter_map(|byte| parser.push(*byte)).collect()
    }

    #[test]
    fn printable_bytes_are_emitted_as_data_without_buffering() {
        assert_eq!(
            parse(b"CQ"),
            vec![ClientItem::Data(b'C'), ClientItem::Data(b'Q')]
        );
    }

    #[test]
    fn fixed_length_commands_wait_for_all_parameters() {
        let mut parser = ClientParser::default();
        assert_eq!(parser.push(0x04), None);
        assert_eq!(parser.push(0x01), None);
        assert_eq!(
            parser.push(0xa0),
            Some(ClientItem::Command(vec![0x04, 0x01, 0xa0]))
        );
    }

    #[test]
    fn load_defaults_consumes_exactly_fifteen_values() {
        let mut bytes = vec![0x0f];
        bytes.extend(1_u8..=15);
        bytes.push(b'K');
        assert_eq!(
            parse(&bytes),
            vec![
                ClientItem::Command(bytes[..16].to_vec()),
                ClientItem::Data(b'K')
            ]
        );
    }

    #[test]
    fn admin_eeprom_load_is_one_bounded_command() {
        let mut bytes = vec![0x00, 0x0d];
        bytes.extend((0_u16..256).map(|value| u8::try_from(value).expect("byte")));
        assert_eq!(parse(&bytes), vec![ClientItem::Command(bytes)]);
    }

    #[test]
    fn pointer_add_nulls_consumes_count_parameter() {
        assert_eq!(
            parse(&[0x16, 0x03, 0x20, b'A']),
            vec![
                ClientItem::Command(vec![0x16, 0x03, 0x20]),
                ClientItem::Data(b'A')
            ]
        );
    }

    #[test]
    fn normalizes_n1mm_extra_admin_prefix_before_buffered_speed() {
        assert_eq!(
            parse(&[0x00, 0x1c, 22, b'Q']),
            vec![ClientItem::Command(vec![0x1c, 22]), ClientItem::Data(b'Q')]
        );
    }

    #[test]
    fn reset_discards_partial_command() {
        let mut parser = ClientParser::default();
        assert_eq!(parser.push(0x02), None);
        parser.reset();
        assert_eq!(parser.push(b'A'), Some(ClientItem::Data(b'A')));
    }

    #[test]
    fn device_bytes_are_classified_by_tag_bits() {
        assert_eq!(
            DeviceEvent::from_byte(0x94),
            DeviceEvent::SpeedPot {
                raw: 0x94,
                value: 20
            }
        );
        assert_eq!(
            DeviceEvent::from_byte(0xd7),
            DeviceEvent::Status(DeviceStatus {
                raw: 0xd7,
                waiting: true,
                key_down: false,
                busy: true,
                break_in: true,
                xoff: true,
            })
        );
        assert_eq!(DeviceEvent::from_byte(b'A'), DeviceEvent::Echo(b'A'));
    }

    #[test]
    fn host_lifecycle_is_virtualized_and_eeprom_is_maintenance_only() {
        assert_eq!(command_policy(&[0x00, 0x02]), CommandPolicy::Virtualized);
        assert_eq!(command_policy(&[0x00, 0x03]), CommandPolicy::Virtualized);
        assert_eq!(command_policy(&[0x00, 0x0d]), CommandPolicy::Maintenance);
        assert_eq!(command_policy(&[0x00, 0x0c]), CommandPolicy::Maintenance);
        assert_eq!(command_policy(&[0x00, 0x10]), CommandPolicy::Maintenance);
        assert_eq!(command_policy(&[0x0a]), CommandPolicy::ActiveOwner);
        assert_eq!(command_policy(&[0x02, 20]), CommandPolicy::Transient);
    }
}
