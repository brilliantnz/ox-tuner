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
/// [`matches_signature`](Self::matches_signature) are required. The rest answer
/// the behavior the `MegaSquirt` dialect shares, so a family that differs in one
/// place overrides that one method.
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

    /// Whether this behavior claims a firmware signature.
    ///
    /// This answers family identity alone. It deliberately takes the signature
    /// text rather than the whole definition, because identifying a firmware is
    /// what a probe needs before any definition exists. A behavior that has no
    /// family identity of its own answers [`Match::No`] here and claims work
    /// through [`supports_compatible`](Self::supports_compatible) instead.
    fn matches_signature(&self, signature: &str) -> Match;

    /// The query byte this family answers, if it has one.
    ///
    /// The byte is family knowledge: it is what a probe sends before any
    /// definition exists. A scan asks one query at a time, so two families
    /// that answer the same byte share one probe and are told apart by the
    /// signature that comes back. A behavior with no family identity answers
    /// `None`.
    fn probe_query(&self) -> Option<&'static [u8]> {
        None
    }

    /// Whether this behavior claims a definition its signature did not name,
    /// because the declared message set is enough.
    ///
    /// Only the generic fallback answers `true`; a family claims work through
    /// [`matches_signature`](Self::matches_signature) alone. The two questions
    /// stay separate so a definition can be claimed compatibly without
    /// pretending to be a known firmware.
    fn supports_compatible(&self, _definition: &Definition) -> bool {
        false
    }

    /// Whether this behavior handles the definition.
    ///
    /// Combines the two questions above: identity first, then the compatible
    /// fallback. It stays on the trait so a definition-driven caller keeps one
    /// entry point.
    fn matches(&self, definition: &Definition) -> Match {
        match self.matches_signature(&definition.signature) {
            Match::No if self.supports_compatible(definition) => Match::Compatible,
            claimed => claimed,
        }
    }

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
    ///
    /// The shape is a family fact: the definition never states it, so the
    /// method takes no definition. One method means a session and a probe can
    /// never read two different shapes from the same family.
    fn reply_code(&self) -> ReplyCode {
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

/// Every behavior this build ships, in the order they are tried.
///
/// FOME is tried before rusEFI because its signature starts with `rusEFI` as
/// well, and Speeduino comes first so a board that keeps a `speeduino`
/// signature stays Speeduino. The fallback comes last and claims nothing
/// exactly. One list, so adding a family cannot reach one selection path and
/// miss the other.
const FAMILIES: [BehaviorKind; 4] = [
    BehaviorKind::Speeduino(SpeeduinoBehavior),
    BehaviorKind::Fome(FomeBehavior),
    BehaviorKind::Rusefi(RusefiBehavior),
    BehaviorKind::MsCompatible(MsCompatibleBehavior),
];

impl BehaviorKind {
    /// Picks a family from a signature alone, with no definition in hand.
    ///
    /// This is identity only: the generic fallback is never returned because
    /// it claims no signature. A probe uses it before any definition exists.
    pub fn for_signature(signature: &str) -> Option<Self> {
        FAMILIES
            .into_iter()
            .find(|candidate| candidate.as_dyn().matches_signature(signature) == Match::Exact)
    }

    /// Selects the first behavior that claims `definition`.
    ///
    /// `Exact` matches win over `Compatible` ones; see [`FAMILIES`] for the
    /// order.
    pub fn select(definition: &Definition) -> Option<Self> {
        // One pass: the first exact match wins outright, while the first
        // compatible one is only remembered in case nothing is exact.
        let mut compatible = None;
        for candidate in FAMILIES {
            match candidate.as_dyn().matches(definition) {
                Match::Exact => return Some(candidate),
                Match::Compatible => {
                    compatible.get_or_insert(candidate);
                }
                Match::No => {}
            }
        }

        compatible
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

/// The signature, trimmed and lowercased for a family check.
fn normalized_signature(signature: &str) -> String {
    signature.trim().to_ascii_lowercase()
}

/// Whether a normalized signature names FOME.
///
/// FOME reports itself inside a rusEFI signature (`rusEFI (FOME) <version>`),
/// so the marker sits after the family prefix rather than at its start.
fn is_fome(signature: &str) -> bool {
    signature.contains("fome")
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

    fn matches_signature(&self, signature: &str) -> Match {
        if normalized_signature(signature).starts_with("speeduino") {
            Match::Exact
        } else {
            Match::No
        }
    }

    fn probe_query(&self) -> Option<&'static [u8]> {
        Some(b"Q")
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

    fn matches_signature(&self, signature: &str) -> Match {
        if is_fome(&normalized_signature(signature)) {
            Match::Exact
        } else {
            Match::No
        }
    }

    fn probe_query(&self) -> Option<&'static [u8]> {
        Some(b"S")
    }

    fn reply_code(&self) -> ReplyCode {
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

    fn matches_signature(&self, signature: &str) -> Match {
        let signature = normalized_signature(signature);

        // A FOME build reports a `rusEFI (FOME)` signature, so it is claimed by
        // its own behavior instead of this one.
        if signature.starts_with("rusefi") && !is_fome(&signature) {
            Match::Exact
        } else {
            Match::No
        }
    }

    fn probe_query(&self) -> Option<&'static [u8]> {
        Some(b"S")
    }

    fn reply_code(&self) -> ReplyCode {
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

    fn matches_signature(&self, _signature: &str) -> Match {
        // A generic fallback has no family identity to claim.
        Match::No
    }

    fn supports_compatible(&self, definition: &Definition) -> bool {
        let has_pages = definition
            .pages
            .iter()
            .any(|page| page.read_command.is_some());
        let has_realtime =
            definition.comms.och_get_command.is_some() && definition.comms.och_block_size.is_some();

        has_pages && has_realtime
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
        assert_eq!(behavior.as_dyn().reply_code(), ReplyCode::Absent);
    }

    #[test]
    fn a_fome_signature_selects_the_fome_behavior() {
        let definition = definition("rusEFI (FOME) .2026.06.03.proteus_h7.3416487136");
        let behavior = BehaviorKind::select(&definition).expect("claimed");

        assert!(matches!(behavior, BehaviorKind::Fome(_)));
        assert_eq!(behavior.as_dyn().id(), "fome");
        assert_eq!(behavior.as_dyn().name(), "FOME");
        assert_eq!(behavior.as_dyn().reply_code(), ReplyCode::Absent);
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
    fn a_signature_alone_picks_a_family_without_a_definition() {
        assert!(matches!(
            BehaviorKind::for_signature("speeduino 202501"),
            Some(BehaviorKind::Speeduino(_))
        ));
        assert!(matches!(
            BehaviorKind::for_signature("rusEFI master.2026.10.21.f407-discovery"),
            Some(BehaviorKind::Rusefi(_))
        ));
        assert!(matches!(
            BehaviorKind::for_signature("rusEFI (FOME) .2026.06.03.proteus_h7"),
            Some(BehaviorKind::Fome(_))
        ));
    }

    #[test]
    fn a_signature_never_falls_back_to_the_compatible_behavior() {
        assert!(BehaviorKind::for_signature("something else").is_none());
        assert!(
            BehaviorKind::for_signature("  ").is_none(),
            "an empty signature has no family"
        );
    }

    #[test]
    fn every_probe_asks_what_its_family_answers() {
        for probe in crate::PROBES {
            assert_eq!(
                Some(probe.query),
                probe.behavior.as_dyn().probe_query(),
                "{} sends a byte its own family does not answer",
                probe.behavior_id()
            );
            assert!(
                FAMILIES.contains(&probe.behavior),
                "{} probes a family the selector does not know",
                probe.behavior_id()
            );
        }
    }

    #[test]
    fn every_family_is_reachable_by_a_probe() {
        // A probe is a question, a family is how the answer is classified:
        // two families may share one query byte and are told apart by the
        // signature that comes back. What must hold is that some probe sends
        // the family's byte with the family's reply shape.
        for family in FAMILIES {
            let Some(query) = family.as_dyn().probe_query() else {
                continue;
            };

            assert!(
                crate::PROBES.iter().any(|probe| probe.query == query
                    && probe.behavior.as_dyn().reply_code() == family.as_dyn().reply_code()),
                "{family:?} answers {query:?} but no probe asks for it"
            );
        }
    }

    #[test]
    fn identity_and_capability_are_separate_questions() {
        let definition = definition("something else");

        // The fallback has no family identity at all.
        assert_eq!(
            MsCompatibleBehavior.matches_signature("something else"),
            Match::No
        );
        assert!(MsCompatibleBehavior.supports_compatible(&definition));

        // A family answers identity and claims nothing by capability.
        assert_eq!(
            SpeeduinoBehavior.matches_signature("speeduino 202501"),
            Match::Exact
        );
        assert!(!SpeeduinoBehavior.supports_compatible(&definition));
        assert_eq!(SpeeduinoBehavior.matches(&definition), Match::No);

        // Combining both recovers the old verdict.
        assert_eq!(MsCompatibleBehavior.matches(&definition), Match::Compatible);
    }

    #[test]
    fn a_definition_without_pages_is_not_supported_by_the_fallback() {
        let definition = Definition::parse(
            "[MegaTune]\nsignature = \"something else\"\nqueryCommand = \"Q\"\n",
            &SettingsContext::new(),
        )
        .expect("parses");

        assert!(!MsCompatibleBehavior.supports_compatible(&definition));
        assert_eq!(MsCompatibleBehavior.matches(&definition), Match::No);
    }

    #[test]
    fn every_family_has_a_distinct_lowercase_id_and_a_label() {
        let families: [&dyn EcuBehavior; 4] = [
            &SpeeduinoBehavior,
            &FomeBehavior,
            &RusefiBehavior,
            &MsCompatibleBehavior,
        ];

        let ids: Vec<&str> = families.iter().map(|behavior| behavior.id()).collect();
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();

        assert_eq!(unique.len(), ids.len(), "ids must not collide: {ids:?}");

        for behavior in families {
            let id = behavior.id();
            assert!(
                id.chars().all(|c| c.is_ascii_lowercase() || c == '-'),
                "`{id}` is a machine key, so it stays lowercase"
            );
            assert!(!behavior.name().is_empty(), "`{id}` needs a label to show");
        }
    }

    #[test]
    fn a_rusefi_board_signature_is_not_mistaken_for_fome() {
        let rusefi = definition("rusEFI master.2026.10.21.f407-discovery.422007793");
        let fome = definition("rusEFI (FOME) .2026.06.03.proteus_h7.3416487136");

        assert!(!is_fome(&normalized_signature(&rusefi.signature)));
        assert!(is_fome(&normalized_signature(&fome.signature)));
    }

    #[test]
    fn the_shared_defaults_cover_the_family_facts() {
        let definition = definition("speeduino 202501");

        assert_eq!(SpeeduinoBehavior.reply_code(), ReplyCode::Leading);
        assert_eq!(FomeBehavior.reply_code(), ReplyCode::Absent);
        assert_eq!(RusefiBehavior.reply_code(), ReplyCode::Absent);
        assert_eq!(MsCompatibleBehavior.reply_code(), ReplyCode::Leading);

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
