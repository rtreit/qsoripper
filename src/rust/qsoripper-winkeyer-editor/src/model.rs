//! `WinKeyer` `WK3tools` EEPROM image model.

use std::fmt;
use std::fs;
use std::path::Path;

use thiserror::Error;

pub(crate) const EEPROM_LEN: usize = 300;
const PROFILE_LEN: usize = 16;

#[derive(Debug, Error)]
pub(crate) enum ImageError {
    #[error("expected a {EEPROM_LEN}-byte WK3tools .eep file, but found {0} bytes")]
    InvalidLength(usize),
    #[error("{field} must be between {min} and {max}")]
    OutOfRange {
        field: &'static str,
        min: u16,
        max: u16,
    },
    #[error("{field} must be 0 or between {min} and {max}")]
    ZeroOrRange {
        field: &'static str,
        min: u16,
        max: u16,
    },
    #[error("raw byte offset must be between 0 and {}", EEPROM_LEN - 1)]
    InvalidOffset,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WinKeyerImage {
    bytes: [u8; EEPROM_LEN],
}

impl WinKeyerImage {
    pub(crate) fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = fs::read(path)?;
        Ok(Self::parse(&bytes)?)
    }

    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, ImageError> {
        let bytes: [u8; EEPROM_LEN] = bytes
            .try_into()
            .map_err(|_: std::array::TryFromSliceError| ImageError::InvalidLength(bytes.len()))?;
        Ok(Self { bytes })
    }

    pub(crate) fn save(&self, path: &Path) -> anyhow::Result<()> {
        fs::write(path, self.bytes)?;
        Ok(())
    }

    pub(crate) fn profile(&self, index: ProfileIndex) -> Profile<'_> {
        Profile {
            bytes: &self.bytes,
            offset: index.offset(),
        }
    }

    pub(crate) fn profile_mut(&mut self, index: ProfileIndex) -> ProfileMut<'_> {
        ProfileMut {
            bytes: &mut self.bytes,
            offset: index.offset(),
        }
    }

    pub(crate) fn first_extension_ms(&self) -> u8 {
        self.bytes[32]
    }

    pub(crate) fn set_first_extension_ms(&mut self, value: u16) -> Result<(), ImageError> {
        self.bytes[32] = checked_u8("first extension", value, 0, 99)?;
        Ok(())
    }

    pub(crate) fn rtty_p1(&self) -> u8 {
        self.bytes[48]
    }

    pub(crate) fn rtty_p2(&self) -> u8 {
        self.bytes[49]
    }

    pub(crate) fn raw(&self, offset: usize) -> Result<u8, ImageError> {
        self.bytes
            .get(offset)
            .copied()
            .ok_or(ImageError::InvalidOffset)
    }

    pub(crate) fn set_raw(&mut self, offset: usize, value: u8) -> Result<(), ImageError> {
        let byte = self
            .bytes
            .get_mut(offset)
            .ok_or(ImageError::InvalidOffset)?;
        *byte = value;
        Ok(())
    }

    pub(crate) fn validate(&self) -> Vec<String> {
        [ProfileIndex::User1, ProfileIndex::User2]
            .into_iter()
            .flat_map(|index| self.profile(index).validate(index.label()))
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ProfileIndex {
    User1,
    User2,
}

impl ProfileIndex {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::User1 => "User 1",
            Self::User2 => "User 2",
        }
    }

    fn offset(self) -> usize {
        match self {
            Self::User1 => 0,
            Self::User2 => PROFILE_LEN,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum KeyerMode {
    IambicB,
    IambicA,
    Ultimatic,
    Bug,
}

impl KeyerMode {
    fn from_bits(value: u8) -> Self {
        match value & 0x03 {
            0 => Self::IambicB,
            1 => Self::IambicA,
            2 => Self::Ultimatic,
            _ => Self::Bug,
        }
    }

    fn bits(self) -> u8 {
        match self {
            Self::IambicB => 0,
            Self::IambicA => 1,
            Self::Ultimatic => 2,
            Self::Bug => 3,
        }
    }

    pub(crate) fn next(self) -> Self {
        match self {
            Self::IambicB => Self::IambicA,
            Self::IambicA => Self::Ultimatic,
            Self::Ultimatic => Self::Bug,
            Self::Bug => Self::IambicB,
        }
    }
}

impl fmt::Display for KeyerMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::IambicB => "Iambic B",
            Self::IambicA => "Iambic A",
            Self::Ultimatic => "Ultimatic",
            Self::Bug => "Bug / straight",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OutputPort {
    None,
    Port1,
    Port2,
    Both,
}

impl OutputPort {
    pub(crate) fn next(self) -> Self {
        match self {
            Self::None => Self::Port1,
            Self::Port1 => Self::Port2,
            Self::Port2 => Self::Both,
            Self::Both => Self::None,
        }
    }
}

impl fmt::Display for OutputPort {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::None => "none",
            Self::Port1 => "port 1",
            Self::Port2 => "port 2",
            Self::Both => "ports 1+2",
        })
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Profile<'a> {
    bytes: &'a [u8; EEPROM_LEN],
    offset: usize,
}

impl Profile<'_> {
    pub(crate) fn mode_register(self) -> u8 {
        self.read(0)
    }

    pub(crate) fn speed_wpm(self) -> u8 {
        self.read(1)
    }

    pub(crate) fn sidetone_divisor(self) -> u8 {
        self.read(2)
    }

    pub(crate) fn sidetone_hz(self) -> u16 {
        let divisor = self.sidetone_divisor();
        if divisor == 0 {
            0
        } else {
            ((62_500_u32 + (u32::from(divisor) / 2)) / u32::from(divisor))
                .try_into()
                .unwrap_or(0)
        }
    }

    pub(crate) fn weight_percent(self) -> u8 {
        self.read(3)
    }

    pub(crate) fn ptt_lead(self) -> u8 {
        self.read(4)
    }

    pub(crate) fn ptt_tail(self) -> u8 {
        self.read(5)
    }

    pub(crate) fn min_wpm(self) -> u8 {
        self.read(6)
    }

    pub(crate) fn speed_range(self) -> u8 {
        self.read(7)
    }

    pub(crate) fn max_wpm(self) -> u8 {
        self.min_wpm().saturating_add(self.speed_range())
    }

    pub(crate) fn x2_mode(self) -> u8 {
        self.read(8)
    }

    pub(crate) fn key_comp_ms(self) -> u8 {
        self.read(9)
    }

    pub(crate) fn farnsworth_wpm(self) -> u8 {
        self.read(10)
    }

    pub(crate) fn paddle_sample_percent(self) -> u8 {
        self.read(11)
    }

    pub(crate) fn dit_dah_ratio_setting(self) -> u8 {
        self.read(12)
    }

    pub(crate) fn dah_ratio_tenths(self) -> u16 {
        (u16::from(self.dit_dah_ratio_setting()) * 30) / 50
    }

    pub(crate) fn pin_config(self) -> u8 {
        self.read(13)
    }

    pub(crate) fn x1_mode(self) -> u8 {
        self.read(14)
    }

    pub(crate) fn command_wpm(self) -> u8 {
        self.read(15)
    }

    pub(crate) fn keyer_mode(self) -> KeyerMode {
        KeyerMode::from_bits(self.mode_register() >> 4)
    }

    pub(crate) fn contest_spacing(self) -> bool {
        self.mode_flag(0)
    }

    pub(crate) fn autospace(self) -> bool {
        self.mode_flag(1)
    }

    pub(crate) fn paddle_swap(self) -> bool {
        self.mode_flag(3)
    }

    pub(crate) fn ptt_enabled(self) -> bool {
        self.pin_flag(0)
    }

    pub(crate) fn sidetone_enabled(self) -> bool {
        self.pin_flag(1)
    }

    pub(crate) fn output_port(self) -> OutputPort {
        match (self.pin_flag(2), self.pin_flag(3)) {
            (false, false) => OutputPort::None,
            (true, false) => OutputPort::Port1,
            (false, true) => OutputPort::Port2,
            (true, true) => OutputPort::Both,
        }
    }

    pub(crate) fn paddle_hang(self) -> u8 {
        (self.pin_config() >> 4) & 0x03
    }

    pub(crate) fn paddle_status(self) -> bool {
        self.x2_flag(7)
    }

    pub(crate) fn fast_command(self) -> bool {
        self.x2_flag(6)
    }

    pub(crate) fn cut_nine(self) -> bool {
        self.x2_flag(5)
    }

    pub(crate) fn cut_zero(self) -> bool {
        self.x2_flag(4)
    }

    pub(crate) fn paddle_only_sidetone(self) -> bool {
        self.x2_flag(3)
    }

    pub(crate) fn so2r(self) -> bool {
        self.x2_flag(2)
    }

    pub(crate) fn paddle_mute(self) -> bool {
        self.x2_flag(1)
    }

    pub(crate) fn letterspace_percent(self) -> u8 {
        (self.x1_mode() & 0x1f) * 2
    }

    pub(crate) fn tune_50(self) -> bool {
        self.x1_flag(5)
    }

    pub(crate) fn message_bank_2(self) -> bool {
        self.x1_flag(6)
    }

    pub(crate) fn selected_user_2(self) -> bool {
        self.x1_flag(7)
    }

    fn validate(self, label: &str) -> Vec<String> {
        let mut errors = Vec::new();
        if self.speed_wpm() != 0 && !(5..=99).contains(&self.speed_wpm()) {
            errors.push(format!(
                "{label} speed is {}; expected 0 or 5-99",
                self.speed_wpm()
            ));
        }
        if !(10..=90).contains(&self.weight_percent()) {
            errors.push(format!(
                "{label} weight is {}; expected 10-90",
                self.weight_percent()
            ));
        }
        if !(5..=99).contains(&self.min_wpm()) || self.max_wpm() > 99 {
            errors.push(format!(
                "{label} speed pot is {}-{}; expected 5-99",
                self.min_wpm(),
                self.max_wpm()
            ));
        }
        if self.farnsworth_wpm() != 0 && !(10..=99).contains(&self.farnsworth_wpm()) {
            errors.push(format!(
                "{label} Farnsworth is {}; expected 0 or 10-99",
                self.farnsworth_wpm()
            ));
        }
        if self.paddle_sample_percent() > 90 {
            errors.push(format!(
                "{label} paddle sample is {}; expected 0-90",
                self.paddle_sample_percent()
            ));
        }
        if !(33..=66).contains(&self.dit_dah_ratio_setting()) {
            errors.push(format!(
                "{label} ratio setting is {}; expected 33-66",
                self.dit_dah_ratio_setting()
            ));
        }
        errors
    }

    fn read(self, relative: usize) -> u8 {
        self.bytes.get(self.offset + relative).copied().unwrap_or(0)
    }

    fn mode_flag(self, bit: u8) -> bool {
        flag(self.mode_register(), bit)
    }

    fn pin_flag(self, bit: u8) -> bool {
        flag(self.pin_config(), bit)
    }

    fn x1_flag(self, bit: u8) -> bool {
        flag(self.x1_mode(), bit)
    }

    fn x2_flag(self, bit: u8) -> bool {
        flag(self.x2_mode(), bit)
    }
}

pub(crate) struct ProfileMut<'a> {
    bytes: &'a mut [u8; EEPROM_LEN],
    offset: usize,
}

impl ProfileMut<'_> {
    pub(crate) fn set_speed_wpm(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(1, checked_zero_or_range("speed", value, 5, 99)?);
        Ok(())
    }

    pub(crate) fn set_command_wpm(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(15, checked_u8("command speed", value, 5, 99)?);
        Ok(())
    }

    pub(crate) fn set_sidetone_hz(&mut self, value: u16) -> Result<(), ImageError> {
        let checked = checked_u8("sidetone", value, 500, 4_000)?;
        let divisor = (62_500_u32 + (u32::from(checked) / 2)) / u32::from(checked);
        self.write(2, u8::try_from(divisor).unwrap_or(u8::MAX).max(1));
        Ok(())
    }

    pub(crate) fn set_weight_percent(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(3, checked_u8("weight", value, 10, 90)?);
        Ok(())
    }

    pub(crate) fn set_ptt_lead(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(4, checked_u8("PTT lead", value, 0, 250)?);
        Ok(())
    }

    pub(crate) fn set_ptt_tail(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(5, checked_u8("PTT tail", value, 0, 250)?);
        Ok(())
    }

    pub(crate) fn set_min_wpm(&mut self, value: u16) -> Result<(), ImageError> {
        let old_max = self.profile().max_wpm();
        self.write(6, checked_u8("minimum WPM", value, 5, 99)?);
        self.set_max_wpm(u16::from(old_max))?;
        Ok(())
    }

    pub(crate) fn set_max_wpm(&mut self, value: u16) -> Result<(), ImageError> {
        let min = self.profile().min_wpm();
        if value < u16::from(min) || value > 99 {
            return Err(ImageError::OutOfRange {
                field: "maximum WPM",
                min: u16::from(min),
                max: 99,
            });
        }
        self.write(7, u8::try_from(value - u16::from(min)).unwrap_or(0));
        Ok(())
    }

    pub(crate) fn set_key_comp_ms(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(9, checked_u8("key compensation", value, 0, 250)?);
        Ok(())
    }

    pub(crate) fn set_farnsworth_wpm(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(10, checked_zero_or_range("Farnsworth", value, 10, 99)?);
        Ok(())
    }

    pub(crate) fn set_paddle_sample_percent(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(11, checked_u8("paddle sample", value, 0, 90)?);
        Ok(())
    }

    pub(crate) fn set_dit_dah_ratio(&mut self, value: u16) -> Result<(), ImageError> {
        self.write(12, checked_u8("dit/dah ratio", value, 33, 66)?);
        Ok(())
    }

    pub(crate) fn cycle_keyer_mode(&mut self) {
        let next = self.profile().keyer_mode().next();
        self.set_keyer_mode(next);
    }

    pub(crate) fn set_keyer_mode(&mut self, mode: KeyerMode) {
        let value = (self.profile().mode_register() & 0xcf) | (mode.bits() << 4);
        self.write(0, value);
    }

    pub(crate) fn cycle_output_port(&mut self) {
        let next = self.profile().output_port().next();
        self.set_output_port(next);
    }

    pub(crate) fn set_output_port(&mut self, port: OutputPort) {
        self.set_pin_flag(2, matches!(port, OutputPort::Port1 | OutputPort::Both));
        self.set_pin_flag(3, matches!(port, OutputPort::Port2 | OutputPort::Both));
    }

    pub(crate) fn set_bool(&mut self, field: BoolField, value: bool) {
        match field {
            BoolField::Autospace => self.set_mode_flag(1, value),
            BoolField::ContestSpacing => self.set_mode_flag(0, value),
            BoolField::PaddleSwap => self.set_mode_flag(3, value),
            BoolField::PttEnabled => self.set_pin_flag(0, value),
            BoolField::SidetoneEnabled => self.set_pin_flag(1, value),
            BoolField::PaddleOnlySidetone => self.set_x2_flag(3, value),
            BoolField::PaddleMute => self.set_x2_flag(1, value),
            BoolField::So2r => self.set_x2_flag(2, value),
            BoolField::FastCommand => self.set_x2_flag(6, value),
            BoolField::CutZero => self.set_x2_flag(4, value),
            BoolField::CutNine => self.set_x2_flag(5, value),
            BoolField::Tune50 => self.set_x1_flag(5, value),
        }
    }

    pub(crate) fn set_paddle_hang(&mut self, value: u16) -> Result<(), ImageError> {
        let value = checked_u8("paddle hang", value, 0, 3)?;
        let pin = (self.profile().pin_config() & 0xcf) | (value << 4);
        self.write(13, pin);
        Ok(())
    }

    pub(crate) fn set_letterspace_percent(&mut self, value: u16) -> Result<(), ImageError> {
        if value > 62 {
            return Err(ImageError::OutOfRange {
                field: "letterspace",
                min: 0,
                max: 62,
            });
        }
        let value = u8::try_from(value / 2).unwrap_or(31);
        let x1 = (self.profile().x1_mode() & 0xe0) | (value & 0x1f);
        self.write(14, x1);
        Ok(())
    }

    fn profile(&self) -> Profile<'_> {
        Profile {
            bytes: self.bytes,
            offset: self.offset,
        }
    }

    fn write(&mut self, relative: usize, value: u8) {
        if let Some(byte) = self.bytes.get_mut(self.offset + relative) {
            *byte = value;
        }
    }

    fn set_mode_flag(&mut self, bit: u8, value: bool) {
        self.write(0, set_flag(self.profile().mode_register(), bit, value));
    }

    fn set_pin_flag(&mut self, bit: u8, value: bool) {
        self.write(13, set_flag(self.profile().pin_config(), bit, value));
    }

    fn set_x1_flag(&mut self, bit: u8, value: bool) {
        self.write(14, set_flag(self.profile().x1_mode(), bit, value));
    }

    fn set_x2_flag(&mut self, bit: u8, value: bool) {
        self.write(8, set_flag(self.profile().x2_mode(), bit, value));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BoolField {
    Autospace,
    ContestSpacing,
    PaddleSwap,
    PttEnabled,
    SidetoneEnabled,
    PaddleOnlySidetone,
    PaddleMute,
    So2r,
    FastCommand,
    CutZero,
    CutNine,
    Tune50,
}

fn checked_u8(field: &'static str, value: u16, min: u16, max: u16) -> Result<u8, ImageError> {
    if value < min || value > max {
        Err(ImageError::OutOfRange { field, min, max })
    } else {
        Ok(u8::try_from(value).unwrap_or(u8::MAX))
    }
}

fn checked_zero_or_range(
    field: &'static str,
    value: u16,
    min: u16,
    max: u16,
) -> Result<u8, ImageError> {
    if value == 0 {
        Ok(0)
    } else if value < min || value > max {
        Err(ImageError::ZeroOrRange { field, min, max })
    } else {
        Ok(u8::try_from(value).unwrap_or(u8::MAX))
    }
}

fn flag(value: u8, bit: u8) -> bool {
    value & (1 << bit) != 0
}

fn set_flag(source: u8, bit: u8, value: bool) -> u8 {
    if value {
        source | (1 << bit)
    } else {
        source & !(1 << bit)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_observed_wk3tools_defaults() -> Result<(), Box<dyn std::error::Error>> {
        let image = WinKeyerImage::parse(&observed_bytes())?;
        let user1 = image.profile(ProfileIndex::User1);

        assert_eq!(user1.keyer_mode(), KeyerMode::IambicB);
        assert_eq!(user1.speed_wpm(), 15);
        assert_eq!(user1.command_wpm(), 15);
        assert_eq!(user1.sidetone_hz(), 801);
        assert_eq!(user1.weight_percent(), 50);
        assert_eq!(user1.min_wpm(), 5);
        assert_eq!(user1.max_wpm(), 35);
        assert_eq!(user1.output_port(), OutputPort::Port1);
        assert!(user1.ptt_enabled());
        assert!(user1.sidetone_enabled());
        assert_eq!(image.first_extension_ms(), 20);
        assert!(image.validate().is_empty());
        Ok(())
    }

    #[test]
    fn edits_typed_and_raw_fields() -> Result<(), Box<dyn std::error::Error>> {
        let mut image = WinKeyerImage::parse(&observed_bytes())?;
        {
            let mut user1 = image.profile_mut(ProfileIndex::User1);
            user1.set_speed_wpm(22)?;
            user1.set_keyer_mode(KeyerMode::IambicA);
            user1.set_bool(BoolField::Autospace, true);
            user1.set_output_port(OutputPort::Port2);
        }
        image.set_first_extension_ms(12)?;
        image.set_raw(0x12, 0x21)?;

        let user1 = image.profile(ProfileIndex::User1);
        assert_eq!(user1.speed_wpm(), 22);
        assert_eq!(user1.keyer_mode(), KeyerMode::IambicA);
        assert!(user1.autospace());
        assert_eq!(user1.output_port(), OutputPort::Port2);
        assert_eq!(image.first_extension_ms(), 12);
        assert_eq!(image.raw(0x12)?, 0x21);
        Ok(())
    }

    #[test]
    fn rejects_unexpected_length() {
        assert!(matches!(
            WinKeyerImage::parse(&[0, 1]),
            Err(ImageError::InvalidLength(2))
        ));
    }

    #[test]
    fn save_round_trips_image() -> Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::NamedTempFile::new()?;
        let path = temp.path().to_owned();
        let mut image = WinKeyerImage::parse(&observed_bytes())?;
        image
            .profile_mut(ProfileIndex::User2)
            .set_bool(BoolField::CutZero, true);

        image.save(&path)?;
        let reloaded = WinKeyerImage::load(&path)?;

        assert!(reloaded.profile(ProfileIndex::User2).cut_zero());
        assert_eq!(reloaded.raw(EEPROM_LEN - 1)?, 0xff);
        Ok(())
    }

    fn observed_bytes() -> [u8; EEPROM_LEN] {
        let mut bytes = [0xff; EEPROM_LEN];
        let prefix = [
            0x00, 0x0f, 0x4e, 0x32, 0x00, 0x00, 0x05, 0x1e, 0x00, 0x00, 0x00, 0x32, 0x32, 0x07,
            0x00, 0x0f, 0x00, 0x0f, 0x4e, 0x32, 0x00, 0x00, 0x05, 0x1e, 0x00, 0x00, 0x00, 0x32,
            0x32, 0x07, 0x00, 0x0f, 0x14, 0x0f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x02, 0x08, 0x03, 0x9c,
        ];
        for (index, value) in prefix.into_iter().enumerate() {
            if let Some(byte) = bytes.get_mut(index) {
                *byte = value;
            }
        }
        bytes[0x123] = 0x04;
        bytes[0x129] = 0x04;
        bytes
    }
}
