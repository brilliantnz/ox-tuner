//! Fault injection: the failure modes the guardrails and the session must
//! survive (ADR-0012).
//!
//! A fault is armed for one kind of operation and fires a set number of times.
//! Single-shot is the default because the interesting test is usually "fail
//! once, then recover": a session that cannot get past the first fault hides
//! the recovery path the user actually needs.

use std::time::Duration;

/// The operation a fault is attached to.
#[expect(
    clippy::exhaustive_enums,
    reason = "the simulator models a fixed set of protocol operations"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SimOp {
    /// The signature query.
    Signature,
    /// A page read.
    ReadPage,
    /// A page write.
    WritePage,
    /// A burn.
    Burn,
    /// A realtime block read.
    Realtime,
    /// A page CRC read.
    Crc,
    /// The next operation of any kind.
    Any,
}

impl SimOp {
    /// A stable name for scenario files and logs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::ReadPage => "read-page",
            Self::WritePage => "write-page",
            Self::Burn => "burn",
            Self::Realtime => "realtime",
            Self::Crc => "crc",
            Self::Any => "any",
        }
    }

    /// Parses the scenario spelling.
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text.trim().to_ascii_lowercase().as_str() {
            "signature" => Self::Signature,
            "read-page" | "read" => Self::ReadPage,
            "write-page" | "write" => Self::WritePage,
            "burn" => Self::Burn,
            "realtime" => Self::Realtime,
            "crc" => Self::Crc,
            "any" => Self::Any,
            _ => return None,
        })
    }
}

/// What goes wrong.
#[expect(
    clippy::exhaustive_enums,
    reason = "the simulator defines a closed set of injected faults"
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    /// No reply at all; the host sees its timeout.
    DropReply,
    /// The reply stops after this many bytes.
    Truncate {
        /// Bytes that still arrive.
        keep: usize,
    },
    /// One reply byte is inverted.
    Corrupt {
        /// Index of the byte to flip.
        at: usize,
    },
    /// The reply arrives late.
    Delay {
        /// How late.
        delay: Duration,
    },
    /// The link dies before the reply.
    Disconnect,
    /// The burn is not applied. In framed mode the ECU also reports an error.
    RefuseBurn,
    /// The framed reply's CRC is wrong.
    BadCrc,
    /// Only this many bytes of a write are applied.
    PartialWrite {
        /// Bytes that land.
        bytes: usize,
    },
}

impl Fault {
    /// A stable name for scenario files and logs.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::DropReply => "drop",
            Self::Truncate { .. } => "truncate",
            Self::Corrupt { .. } => "corrupt",
            Self::Delay { .. } => "delay",
            Self::Disconnect => "disconnect",
            Self::RefuseBurn => "refuse-burn",
            Self::BadCrc => "bad-crc",
            Self::PartialWrite { .. } => "partial-write",
        }
    }
}

/// One armed fault.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FaultRule {
    /// The operation it applies to.
    pub operation: SimOp,
    /// What happens.
    pub fault: Fault,
    /// How many operations it still affects.
    pub remaining: u32,
}

impl FaultRule {
    /// A rule that fires `times` times.
    pub fn new(operation: SimOp, fault: Fault, times: u32) -> Self {
        Self {
            operation,
            fault,
            remaining: times.max(1),
        }
    }

    /// A single-shot rule.
    pub fn once(operation: SimOp, fault: Fault) -> Self {
        Self::new(operation, fault, 1)
    }
}

/// The armed rules, consumed as operations happen.
#[derive(Debug, Clone, Default)]
pub(crate) struct Faults {
    rules: Vec<FaultRule>,
}

impl Faults {
    /// No faults.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Adds a rule.
    pub(crate) fn arm(&mut self, rule: FaultRule) {
        self.rules.push(rule);
    }

    /// Takes the first rule that applies to `operation`.
    ///
    /// `Any` rules are considered after the specific ones, so a test can arm a
    /// general failure and still target one operation precisely.
    pub(crate) fn take(&mut self, operation: SimOp) -> Option<Fault> {
        let index = self
            .rules
            .iter()
            .position(|rule| rule.operation == operation)
            .or_else(|| {
                self.rules
                    .iter()
                    .position(|rule| rule.operation == SimOp::Any)
            })?;
        let rule = &mut self.rules[index];
        let fault = rule.fault.clone();
        rule.remaining = rule.remaining.saturating_sub(1);
        if rule.remaining == 0 {
            self.rules.remove(index);
        }
        Some(fault)
    }

    /// Whether anything is armed.
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_single_shot_fault_fires_once() {
        let mut faults = Faults::new();
        faults.arm(FaultRule::once(SimOp::Burn, Fault::RefuseBurn));

        assert_eq!(faults.take(SimOp::Burn), Some(Fault::RefuseBurn));
        assert_eq!(faults.take(SimOp::Burn), None);
        assert!(faults.is_empty());
    }

    #[test]
    fn a_specific_rule_is_preferred_over_any() {
        let mut faults = Faults::new();
        faults.arm(FaultRule::once(SimOp::Any, Fault::DropReply));
        faults.arm(FaultRule::once(SimOp::ReadPage, Fault::Disconnect));

        assert_eq!(faults.take(SimOp::ReadPage), Some(Fault::Disconnect));
        assert_eq!(faults.take(SimOp::Burn), Some(Fault::DropReply));
    }
}
