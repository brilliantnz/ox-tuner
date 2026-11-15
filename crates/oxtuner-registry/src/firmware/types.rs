//! The data a future flash worker will need, and nothing it will do.

use crate::definition::Family;

/// The board a firmware image runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Board {
    /// Arduino Mega 2560, the stock NO2C controller.
    Mega2560,
    /// Teensy 3.5.
    Teensy35,
    /// Teensy 3.6.
    Teensy36,
    /// Teensy 4.1.
    Teensy41,
    /// STM32F407 in DFU mode.
    Stm32f407,
    /// Anything else. No tool knows it yet.
    Unknown,
}

impl Board {
    /// The stable lowercase key for logs and cache directories.
    pub fn id(&self) -> &'static str {
        match self {
            Self::Mega2560 => "mega2560",
            Self::Teensy35 => "teensy35",
            Self::Teensy36 => "teensy36",
            Self::Teensy41 => "teensy41",
            Self::Stm32f407 => "stm32f407",
            Self::Unknown => "unknown",
        }
    }
}

/// One firmware image: the bytes a board would run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareImage {
    /// Whose firmware this is.
    pub family: Family,
    /// The version text as the family lists it.
    pub version: String,
    /// What it runs on.
    pub board: Board,
    /// Where the bytes come from.
    pub url: String,
    /// The expected SHA-256, when the family publishes one.
    pub sha256: Option<[u8; 32]>,
    /// The expected size, when the family publishes one.
    pub size: Option<u64>,
}

/// The tool that would write an image, once a flasher exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FlashTool {
    /// AVR boards through avrdude.
    Avrdude,
    /// STM32 boards through dfu-util.
    DfuUtil,
    /// Teensy boards through the loader.
    TeensyLoader,
    /// No tool knows this board yet.
    Unsupported,
}

/// A flash the worker would perform: image, tool and port together.
///
/// This is a plan, not an action. Building one never touches a port; running
/// one stays out of scope until the flash worker is designed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlashPlan {
    /// What would be written.
    pub image: FirmwareImage,
    /// What would write it.
    pub tool: FlashTool,
    /// Where it would be written.
    pub port: String,
}

impl FlashPlan {
    /// Whether the plan names a tool that exists.
    pub fn is_supported(&self) -> bool {
        self.tool != FlashTool::Unsupported
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image() -> FirmwareImage {
        FirmwareImage {
            family: Family::Speeduino,
            version: "202501.7".to_owned(),
            board: Board::Mega2560,
            url: "https://speeduino.com/fw/bin/202501.7.hex".to_owned(),
            sha256: None,
            size: None,
        }
    }

    #[test]
    fn board_ids_are_stable_lowercase_keys() {
        for board in [
            Board::Mega2560,
            Board::Teensy35,
            Board::Teensy36,
            Board::Teensy41,
            Board::Stm32f407,
            Board::Unknown,
        ] {
            let id = board.id();

            assert!(
                id.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            );
        }
    }

    #[test]
    fn a_plan_reports_whether_a_tool_exists() {
        let supported = FlashPlan {
            image: image(),
            tool: FlashTool::Avrdude,
            port: "/dev/cu.usbmodem1".to_owned(),
        };
        let missing = FlashPlan {
            tool: FlashTool::Unsupported,
            ..supported.clone()
        };

        assert!(supported.is_supported());
        assert!(!missing.is_supported());
    }
}
