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
//! One struct per family, each answering only the questions it answers
//! differently. [`SpeeduinoBehavior`], [`FomeBehavior`] and [`RusefiBehavior`]
//! claim their own signatures exactly; [`MsCompatibleBehavior`] takes any other
//! definition whose message set fits as `Compatible`, never as a guess. All of
//! them consume the definition's own message set, so a new family is a new
//! implementation here and never a branch in the protocol.
//!
//! A definition carries no display name for its firmware, and the signatures
//! themselves are not written consistently. That is why a behavior answers two
//! identity questions: [`EcuBehavior::id`] is the stable lowercase key used in
//! logs, the journal and the connection summary, while [`EcuBehavior::name`] is
//! the label a person reads.

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
    /// Eight or more ASCII hex characters. Recognised so a definition is not
    /// misread, but this build has no read shape for it and refuses the page
    /// checksum instead of parsing the first four characters.
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
/// and FOME answer with the data alone (an empty payload for a successful
/// write). The definition cannot say which, so the behavior does.
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
///
/// The trait is public so a board-specific behavior can live outside this
/// crate. The shipped implementations are the ones this build selects on its
/// own; a downstream implementation is supported when it answers every
/// question from the definition alone, because the protocol never asks the
/// ECU for behavior.
///
/// Only [`id`](Self::id), [`name`](Self::name) and
/// [`matches`](Self::matches) are required. The rest answer the behavior the
/// `MegaSquirt` dialect shares, so a family that differs in one place overrides
/// that one method.
pub trait EcuBehavior: Send + Sync {
    /// A stable lowercase key for logs, the journal and the connection summary.
    ///
    /// It never carries display casing; [`name`](Self::name) is what a person
    /// reads.
    fn id(&self) -> &'static str;

    /// The family's display label, as the firmware writes it.
    ///
    /// Signatures spell their own name inconsistently, so the label is chosen
    /// here instead of being derived from `Definition::signature`.
    fn name(&self) -> &'static str;

    /// Whether this behavior handles the definition.
    fn matches(&self, definition: &Definition) -> Match;

    /// Commands to send once after the port opens, before the first read.
    ///
    /// An empty slice is the normal answer.
    fn handshake(&self, _definition: &Definition) -> Vec<oxtuner_ini::CommandTemplate> {
        Vec::new()
    }

    /// The page checksum variant, when the definition offers a command.
    fn crc(&self, definition: &Definition, page: u8) -> Option<Crc> {
        page_crc(definition, page)
    }

    /// How the firmware selects a page.
    fn switch_page(&self, _definition: &Definition, _page: u8) -> PageSwitch {
        // The page identifier rides in every read, write and burn command, so
        // there is no separate activation step.
        PageSwitch::None
    }

    /// What a burn means for this page.
    fn burn(&self, definition: &Definition, page: u8) -> BurnPlan {
        burn_plan(definition, page)
    }

    /// How a framed reply is shaped (see [`ReplyCode`]).
    fn reply_code(&self, _definition: &Definition) -> ReplyCode {
        ReplyCode::Leading
    }
}

/// The behaviors this build ships, tried in order.
///
/// A definition's signature decides which one claims it. The protocol then
/// talks to whatever the definition declares; nothing downstream knows the
/// family label except the connection summary.
#[expect(
    clippy::exhaustive_enums,
    reason = "the shipped behavior families are a closed set"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BehaviorKind {
    /// Speeduino, the first target (ADR-0009); the read-only path verified on
    /// the bench NO2C (P4).
    Speeduino(SpeeduinoBehavior),
    /// FOME, which reports a `rusEFI (FOME)` signature but answers with the
    /// data alone, like rusEFI.
    Fome(FomeBehavior),
    /// rusEFI.
    Rusefi(RusefiBehavior),
    /// Any other MegaSquirt-dialect firmware that names its own commands.
    MsCompatible(MsCompatibleBehavior),
}

impl BehaviorKind {
    /// Selects the first behavior that claims `definition`.
    ///
    /// `Exact` matches win over `Compatible` ones. FOME is tried before rusEFI
    /// because its signature starts with `rusEFI` as well, and Speeduino comes
    /// first so a board that keeps a `speeduino` signature stays Speeduino.
    pub fn select(definition: &Definition) -> Option<Self> {
        let candidates = [
            Self::Speeduino(SpeeduinoBehavior),
            Self::Fome(FomeBehavior),
            Self::Rusefi(RusefiBehavior),
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
            Self::Fome(behavior) => behavior,
            Self::Rusefi(behavior) => behavior,
            Self::MsCompatible(behavior) => behavior,
        }
    }
}

/// The definition's signature, trimmed and lowercased for a family check.
fn normalized_signature(definition: &Definition) -> String {
    definition.signature.trim().to_ascii_lowercase()
}

/// Speeduino (ADR-0009).
///
/// Speeduino speaks the `MegaSquirt` message set, puts the page in every
/// command, needs no wake handshake, and reports a page CRC-32 as four
/// big-endian bytes. It prefixes every framed reply with a return code.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SpeeduinoBehavior;

impl EcuBehavior for SpeeduinoBehavior {
    fn id(&self) -> &'static str {
        "speeduino"
    }

    fn name(&self) -> &'static str {
        "Speeduino"
    }

    fn matches(&self, definition: &Definition) -> Match {
        if normalized_signature(definition).starts_with("speeduino") {
            Match::Exact
        } else {
            Match::No
        }
    }
}

/// FOME, which builds on rusEFI and says so in its signature.
///
/// A FOME definition reports `rusEFI (FOME) <version>`, so the family is found
/// inside the signature rather than at its start. Like rusEFI it answers with
/// the data alone.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FomeBehavior;

impl EcuBehavior for FomeBehavior {
    fn id(&self) -> &'static str {
        "fome"
    }

    fn name(&self) -> &'static str {
        "FOME"
    }

    fn matches(&self, definition: &Definition) -> Match {
        if normalized_signature(definition).contains("fome") {
            Match::Exact
        } else {
            Match::No
        }
    }

    fn reply_code(&self, _definition: &Definition) -> ReplyCode {
        // Same reply shape as rusEFI: data alone, and an empty payload
        // acknowledges a write.
        ReplyCode::Absent
    }
}

/// rusEFI.
///
/// The message set is the definition's. rusEFI names its signature
/// `rusEFI <branch>.<date>.<board>.<hash>` and answers framed requests with the
/// data alone.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RusefiBehavior;

impl EcuBehavior for RusefiBehavior {
    fn id(&self) -> &'static str {
        "rusefi"
    }

    fn name(&self) -> &'static str {
        "rusEFI"
    }

    fn matches(&self, definition: &Definition) -> Match {
        let signature = normalized_signature(definition);

        // A FOME build reports a `rusEFI (FOME)` signature, so it is claimed by
        // its own behavior instead of this one.
        if signature.starts_with("rusefi") && !signature.contains("fome") {
            Match::Exact
        } else {
            Match::No
        }
    }

    fn reply_code(&self, _definition: &Definition) -> ReplyCode {
        // `sendResponse(TS_CRC, data, count)`: the payload is the data itself,
        // and an empty payload acknowledges a write.
        ReplyCode::Absent
    }
}

/// Every other MegaSquirt-dialect firmware with the same command set.
///
/// This behavior claims nothing exactly. It answers `Compatible` for a
/// definition that has page reads and a realtime block, which is what a
/// board-generated INI needs to work without a branch in the protocol.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct MsCompatibleBehavior;

impl EcuBehavior for MsCompatibleBehavior {
    fn id(&self) -> &'static str {
        "ms-compatible"
    }

    fn name(&self) -> &'static str {
        "MS-compatible"
    }

    fn matches(&self, definition: &Definition) -> Match {
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
        // These firmwares answer as soon as the port is open; the port's DTR
        // reset is covered by `delayAfterPortOpen`.
        Vec::new()
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
    fn a_rusefi_signature_selects_the_rusefi_behavior() {
        let definition = definition("rusEFI master.2026.10.21.f407-discovery.422007793");
        let behavior = BehaviorKind::select(&definition).expect("claimed");

        assert!(matches!(behavior, BehaviorKind::Rusefi(_)));
        assert_eq!(behavior.as_dyn().id(), "rusefi");
        assert_eq!(behavior.as_dyn().name(), "rusEFI");
        assert_eq!(behavior.as_dyn().reply_code(&definition), ReplyCode::Absent);
    }

    #[test]
    fn a_fome_signature_selects_the_fome_behavior() {
        let definition = definition("rusEFI (FOME) .2026.06.03.proteus_h7.3416487136");
        let behavior = BehaviorKind::select(&definition).expect("claimed");

        assert!(matches!(behavior, BehaviorKind::Fome(_)));
        assert_eq!(behavior.as_dyn().id(), "fome");
        assert_eq!(behavior.as_dyn().name(), "FOME");
        assert_eq!(behavior.as_dyn().reply_code(&definition), ReplyCode::Absent);
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
    fn a_definition_with_pages_and_realtime_but_no_family_is_compatible() {
        let definition = definition("something else");
        let behavior = BehaviorKind::select(&definition).expect("compatible");

        assert!(matches!(behavior, BehaviorKind::MsCompatible(_)));
        assert_eq!(behavior.as_dyn().matches(&definition), Match::Compatible);
        assert_eq!(behavior.as_dyn().id(), "ms-compatible");
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
    fn every_family_has_a_stable_id_and_a_readable_name() {
        let expected = [
            (SpeeduinoBehavior.id(), SpeeduinoBehavior.name()),
            (FomeBehavior.id(), FomeBehavior.name()),
            (RusefiBehavior.id(), RusefiBehavior.name()),
            (MsCompatibleBehavior.id(), MsCompatibleBehavior.name()),
        ];

        for (id, name) in expected {
            assert_eq!(id, id.to_ascii_lowercase(), "the id is a lowercase key");
            assert!(!name.is_empty(), "the name is a display label");
        }
    }

    #[test]
    fn the_shared_defaults_cover_the_family_facts() {
        let definition = definition("speeduino 202501");

        assert_eq!(
            SpeeduinoBehavior.reply_code(&definition),
            ReplyCode::Leading
        );
        assert_eq!(FomeBehavior.reply_code(&definition), ReplyCode::Absent);
        assert_eq!(RusefiBehavior.reply_code(&definition), ReplyCode::Absent);
        assert_eq!(
            MsCompatibleBehavior.reply_code(&definition),
            ReplyCode::Leading
        );

        let behaviors: [&dyn EcuBehavior; 4] = [
            &SpeeduinoBehavior,
            &FomeBehavior,
            &RusefiBehavior,
            &MsCompatibleBehavior,
        ];
        for behavior in behaviors {
            assert_eq!(behavior.switch_page(&definition, 1), PageSwitch::None);
            assert_eq!(behavior.handshake(&definition).len(), 0);
        }
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
