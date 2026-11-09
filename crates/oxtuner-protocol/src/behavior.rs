//! What an INI cannot say, and the behaviors that fill those gaps.
//!
//! ADR-0006 decision 3 makes this a contract, not a convenience hook: if the
//! definition can express something, it must not be on [`EcuBehavior`]. The
//! surface is limited to the things a definition file cannot state — which
//! firmware this is, a wake handshake, the CRC variant, the page-switch rule
//! and what a missing burn command means.
//!
//! Deliberately absent: retries, timeouts, pacing and anything that could write
//! without the guardrails seeing it. A behavior may say "this page is RAM-only";
//! it may never decide to skip a read-back (ADR-0011).
//!
//! # Families
//!
//! [`SpeeduinoBehavior`] claims Speeduino. [`MsCompatibleBehavior`] covers the
//! other MegaSquirt-dialect firmwares — rusEFI, FOME and any board-generated
//! INI that uses the same commands — as `Compatible`, not as a guess. Both
//! consume the definition's own message set; a new family is a new
//! implementation here, never a branch in the protocol.

use oxtuner_ini::Definition;

/// How well a behavior claims a definition.
#[expect(
    clippy::exhaustive_enums,
    reason = "a behavior either matches exactly, compatibly, or not at all"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    /// The definition describes exactly this firmware.
    Exact,
    /// The definition uses a compatible message set, but names another
    /// firmware family.
    Compatible,
    /// Not this behavior.
    No,
}

/// The CRC algorithm a firmware's page checksum uses.
#[expect(
    clippy::exhaustive_enums,
    reason = "every firmware this build supports uses the same CRC variant"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Crc32Variant {
    /// CRC-32/ISO-HDLC with the standard polynomial.
    IsoHdlc,
}

/// How a CRC reply is encoded on the wire.
#[expect(
    clippy::exhaustive_enums,
    reason = "the dialect defines a fixed set of CRC reply encodings"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcReply {
    /// Four raw bytes, most significant first.
    RawBigEndian,
    /// Four raw bytes, least significant first.
    RawLittleEndian,
    /// Eight or more ASCII hex characters.
    HexText,
}

/// A page checksum the firmware can produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crc {
    /// The algorithm.
    pub algorithm: Crc32Variant,
    /// The reply encoding.
    pub reply: CrcReply,
}

/// How framed replies are shaped.
///
/// Both families use the same `msEnvelope_1.0` frame, but not the same payload:
/// Speeduino puts a one-byte return code in front of every reply, while rusEFI
/// answers with the data alone (an empty payload for a successful write). The
/// definition cannot say which, so the behavior does.
#[expect(
    clippy::exhaustive_enums,
    reason = "a reply either leads with a code or carries only data"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyCode {
    /// The payload starts with a one-byte return code (`0x00` ok).
    Leading,
    /// The payload is the data itself; a successful write has no payload.
    Absent,
}

/// What the firmware needs before a page can be used.
#[expect(
    clippy::exhaustive_enums,
    reason = "the dialect defines a fixed set of page switch requirements"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSwitch {
    /// No switch: the page is named in every command.
    None,
    /// A read of the target page both selects it and validates it.
    ReadBeforeUse,
    /// Bytes known only locally must be re-burned before the switch.
    BurnBeforeSwitch,
}

/// What a burn would do for one page.
#[expect(
    clippy::exhaustive_enums,
    reason = "a page either burns immediately or cannot be burned"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BurnPlan {
    /// The definition declares a burn command for the page.
    Immediate,
    /// The page has no burn command: its contents are RAM-only.
    NotBurnable {
        /// Why, in text the UI can show.
        reason: &'static str,
    },
}

/// Firmware knowledge the definition cannot carry.
pub trait EcuBehavior: Send + Sync {
    /// A stable name for logs and the connection summary.
    fn name(&self) -> &'static str;

    /// Whether this behavior handles the definition.
    fn matches(&self, definition: &Definition) -> Match;

    /// Commands to send once after the port opens, before the first read.
    ///
    /// An empty slice is the normal answer.
    fn handshake(&self, definition: &Definition) -> Vec<oxtuner_ini::CommandTemplate>;

    /// The page checksum variant, when the definition offers a command.
    fn crc(&self, definition: &Definition, page: u8) -> Option<Crc>;

    /// How the firmware selects a page.
    fn switch_page(&self, definition: &Definition, page: u8) -> PageSwitch;

    /// What a burn means for this page.
    fn burn(&self, definition: &Definition, page: u8) -> BurnPlan;

    /// How a framed reply is shaped (see [`ReplyCode`]).
    fn reply_code(&self, definition: &Definition) -> ReplyCode;
}

/// The behaviors this build ships, tried in order.
///
/// A definition's signature decides which one claims it. The protocol then
/// talks to whatever the definition declares; nothing downstream knows the
/// family name except the connection summary.
#[expect(
    clippy::exhaustive_enums,
    reason = "the shipped behavior families are a closed set"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorKind {
    /// Speeduino, the first target (ADR-0009); the read-only path verified on
    /// the bench NO2C (P4).
    Speeduino(SpeeduinoBehavior),
    /// Any other MegaSquirt-dialect firmware that names its own commands.
    MsCompatible(MsCompatibleBehavior),
}

impl BehaviorKind {
    /// Selects the first behavior that claims `definition`.
    ///
    /// `Exact` matches win over `Compatible` ones, so a Speeduino-derived file
    /// with a board name still gets the Speeduino behavior when its signature
    /// says Speeduino.
    pub fn select(definition: &Definition) -> Option<Self> {
        let candidates = [
            Self::Speeduino(SpeeduinoBehavior),
            Self::MsCompatible(MsCompatibleBehavior),
        ];
        candidates
            .into_iter()
            .find(|behavior| behavior.as_dyn().matches(definition) == Match::Exact)
            .or_else(|| {
                candidates
                    .into_iter()
                    .find(|behavior| behavior.as_dyn().matches(definition) == Match::Compatible)
            })
    }

    /// The behavior as the trait object the protocol consumes.
    pub fn as_dyn(&self) -> &dyn EcuBehavior {
        match self {
            Self::Speeduino(behavior) => behavior,
            Self::MsCompatible(behavior) => behavior,
        }
    }
}

/// Speeduino (ADR-0009).
///
/// Speeduino speaks the `MegaSquirt` message set, puts the page in every command,
/// needs no wake handshake, and reports a page CRC-32 as four big-endian bytes.
/// Its legacy burn command is accepted; a page with an empty `burnCommand` is
/// RAM-only and says so.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpeeduinoBehavior;

impl EcuBehavior for SpeeduinoBehavior {
    fn name(&self) -> &'static str {
        "speeduino"
    }

    fn matches(&self, definition: &Definition) -> Match {
        if definition
            .signature
            .trim()
            .to_ascii_lowercase()
            .starts_with("speeduino")
        {
            Match::Exact
        } else {
            Match::No
        }
    }

    fn handshake(&self, _definition: &Definition) -> Vec<oxtuner_ini::CommandTemplate> {
        Vec::new()
    }

    fn crc(&self, definition: &Definition, page: u8) -> Option<Crc> {
        page_crc(definition, page)
    }

    fn switch_page(&self, _definition: &Definition, _page: u8) -> PageSwitch {
        // The page identifier rides in every read, write and burn command, so
        // there is no separate activation step.
        PageSwitch::None
    }

    fn burn(&self, definition: &Definition, page: u8) -> BurnPlan {
        burn_plan(definition, page)
    }

    fn reply_code(&self, _definition: &Definition) -> ReplyCode {
        // `sendReturnCodeMsg` / `serialPayload[0] = SERIAL_RC_OK` in every
        // handler: the reply code is part of this firmware's payloads.
        ReplyCode::Leading
    }
}

/// Every other MegaSquirt-dialect firmware: rusEFI, FOME and board-generated
/// INIs with the same command set.
///
/// The message set is the definition's; this behavior only claims it and
/// supplies the answers an INI cannot. rusEFI names its signature `rusEFI
/// <board>` and FOME keeps `speeduino`-style text with a FOME suffix, so both
/// are `Exact` matches; anything else with pages and a realtime block is
/// `Compatible`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MsCompatibleBehavior;

impl EcuBehavior for MsCompatibleBehavior {
    fn name(&self) -> &'static str {
        "ms-compatible"
    }

    fn matches(&self, definition: &Definition) -> Match {
        let signature = definition.signature.trim().to_ascii_lowercase();
        if signature.starts_with("rusefi") || signature.contains("fome") {
            return Match::Exact;
        }

        let has_pages = definition
            .pages
            .iter()
            .any(|page| page.read_command.is_some());
        let has_realtime =
            definition.comms.och_get_command.is_some() && definition.comms.och_block_size.is_some();
        if has_pages && has_realtime {
            Match::Compatible
        } else {
            Match::No
        }
    }

    fn handshake(&self, _definition: &Definition) -> Vec<oxtuner_ini::CommandTemplate> {
        // rusEFI and FOME answer as soon as the port is open; the port's DTR
        // reset is covered by `delayAfterPortOpen`.
        Vec::new()
    }

    fn crc(&self, definition: &Definition, page: u8) -> Option<Crc> {
        page_crc(definition, page)
    }

    fn switch_page(&self, _definition: &Definition, _page: u8) -> PageSwitch {
        PageSwitch::None
    }

    fn burn(&self, definition: &Definition, page: u8) -> BurnPlan {
        burn_plan(definition, page)
    }

    fn reply_code(&self, definition: &Definition) -> ReplyCode {
        if definition
            .signature
            .trim()
            .to_ascii_lowercase()
            .starts_with("rusefi")
        {
            // rusEFI answers with the data alone (`sendResponse(TS_CRC, data,
            // count)`), and an empty payload acknowledges a write.
            ReplyCode::Absent
        } else {
            ReplyCode::Leading
        }
    }
}

/// The shared CRC answer: the dialect's command, and the encoding the
/// `MegaSquirt` family uses (four raw bytes, most significant first — rusEFI
/// swaps the word before sending it, which is the same bytes).
fn page_crc(definition: &Definition, page: u8) -> Option<Crc> {
    definition
        .page(page)
        .and_then(|page| page.crc_command.as_ref())
        .filter(|command| !command.is_empty())
        .map(|_| Crc {
            algorithm: Crc32Variant::IsoHdlc,
            reply: CrcReply::RawBigEndian,
        })
}

/// The shared burn answer.
fn burn_plan(definition: &Definition, page: u8) -> BurnPlan {
    match definition.page(page) {
        Some(entry) if entry.is_burnable() => BurnPlan::Immediate,
        _ => BurnPlan::NotBurnable {
            reason: "this page has no burn command in the definition, so it lives in RAM only",
        },
    }
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::SettingsContext;

    use super::*;

    fn definition(signature: &str) -> Definition {
        Definition::parse(
            &format!(
                "\
[MegaTune]
signature = \"{signature}\"
queryCommand = \"Q\"

[Constants]
nPages = 1
pageSize = 8
page = 1
reqFuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
pageReadCommand = \"p%2i%2o%2c\"
burnCommand = \"b%2i\"

[OutputChannels]
ochGetCommand = \"r%2o%2c\"
ochBlockSize = 8
rpm = scalar, U16, 0, \"rpm\", 1.0, 0.0
"
            ),
            &SettingsContext::new(),
        )
        .expect("parses")
    }

    #[test]
    fn a_speeduino_signature_selects_the_speeduino_behavior() {
        let definition = definition("speeduino 202501");

        assert!(matches!(
            BehaviorKind::select(&definition),
            Some(BehaviorKind::Speeduino(_))
        ));
    }

    #[test]
    fn rusefi_and_fome_are_claimed_exactly_by_the_generic_behavior() {
        for signature in ["rusEFI v20240101", "fome 202401"] {
            let definition = definition(signature);
            let behavior = BehaviorKind::select(&definition).expect("claimed");

            assert_eq!(behavior.as_dyn().matches(&definition), Match::Exact);
            assert_eq!(behavior.as_dyn().name(), "ms-compatible");
        }
    }

    #[test]
    fn a_fome_build_that_keeps_the_speeduino_signature_stays_speeduino() {
        let definition = definition("speeduino fome 202401");

        assert!(matches!(
            BehaviorKind::select(&definition),
            Some(BehaviorKind::Speeduino(_))
        ));
    }

    #[test]
    fn an_unrelated_definition_is_not_claimed() {
        let definition = Definition::parse(
            "[MegaTune]\nsignature = \"something else\"\n",
            &SettingsContext::new(),
        )
        .expect("parses");

        assert!(BehaviorKind::select(&definition).is_none());
    }

    #[test]
    fn a_page_without_a_burn_command_is_ram_only() {
        let definition = definition("speeduino 202501");
        let behavior = SpeeduinoBehavior;

        assert_eq!(behavior.burn(&definition, 1), BurnPlan::Immediate);
        assert!(matches!(
            behavior.burn(&definition, 9),
            BurnPlan::NotBurnable { .. }
        ));
    }

    #[test]
    fn the_crc_variant_follows_the_definition_command() {
        let definition = definition("speeduino 202501");

        assert!(SpeeduinoBehavior.crc(&definition, 1).is_none());
    }
}
