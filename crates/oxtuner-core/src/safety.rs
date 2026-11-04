//! Safety policy shared by every write path (ADR-0011).
//!
//! The UI can only ask; the core decides. These helpers are the decision
//! points, kept together so a new command cannot invent its own looser rule.

use oxtuner_tune::VariableSpec;

/// The global write switch and the halt latch.
#[derive(Debug, Clone)]
pub(crate) struct Safety {
    writes_enabled: bool,
    halted: Option<String>,
}

impl Safety {
    /// Starts with writes off unless the settings said otherwise (S8).
    pub(crate) fn new(writes_enabled: bool) -> Self {
        Self {
            writes_enabled,
            halted: None,
        }
    }

    /// Whether writes may run.
    pub(crate) fn writes_enabled(&self) -> bool {
        self.writes_enabled
    }

    /// Turns the switch on or off. Turning it on never clears a halt; the user
    /// has to review the failure first.
    pub(crate) fn set_writes_enabled(&mut self, enabled: bool) {
        self.writes_enabled = enabled;
    }

    /// Why the write path is halted, when it is.
    pub(crate) fn halted(&self) -> Option<&str> {
        self.halted.as_deref()
    }

    /// Stops every later write until the user acknowledges the failure.
    pub(crate) fn halt(&mut self, reason: impl Into<String>) {
        self.halted = Some(reason.into());
    }

    /// Clears the latch after review.
    pub(crate) fn clear_halt(&mut self) {
        self.halted = None;
    }

    /// The one gate every write or burn passes.
    ///
    /// `connected` is the live session, `signature_matches` its S1 verdict,
    /// `has_project` whether a tune is open, and `project_read_only` the
    /// project's own load gate. The returned text is shown verbatim.
    pub(crate) fn gate(
        &self,
        connected: bool,
        signature_matches: bool,
        has_project: bool,
        project_read_only: Option<&str>,
    ) -> Result<(), String> {
        if let Some(reason) = &self.halted {
            return Err(format!(
                "the write path is halted after a failed verification: {reason}. Review the \
                 tune, then acknowledge the failure before writing again"
            ));
        }
        if !self.writes_enabled {
            return Err(
                "writes are disabled (S8). Enable them from the write menu after confirming the \
                 engine is off and the ECU is at the bench"
                    .to_owned(),
            );
        }
        if !connected {
            return Err("connect to the ECU before writing".to_owned());
        }
        if !signature_matches {
            return Err(
                "the ECU signature does not match the loaded definition, so the session is \
                 read-only (S1)"
                    .to_owned(),
            );
        }
        if !has_project {
            return Err(
                "open a project before writing: a standalone definition has no tune image to send"
                    .to_owned(),
            );
        }
        if let Some(reason) = project_read_only {
            return Err(format!("the tune is read-only: {reason}"));
        }
        Ok(())
    }
}

/// Clamps a user value to the definition's declared range (S2).
///
/// Returns the value to store and whether it had to be clamped. A definition
/// without a finite min/max leaves the value alone; an inverted pair is treated
/// as absent rather than guessed.
#[expect(
    clippy::float_cmp,
    reason = "clamping compares exact stored values, including the signed-zero case"
)]
pub(crate) fn clamp_user(spec: &VariableSpec, value: f64) -> (f64, bool) {
    let (Some(minimum), Some(maximum)) = (spec.minimum, spec.maximum) else {
        return (value, false);
    };
    if !minimum.is_finite() || !maximum.is_finite() || minimum > maximum {
        return (value, false);
    }
    let clamped = value.clamp(minimum, maximum);
    // The wire has no signed zero, so `-0.0` and `0.0` are the same value.
    // Store the positive zero and compare numerically, otherwise a value that
    // moved to `-0.0` looks modified without having changed.
    let clamped = if clamped == 0.0 { 0.0 } else { clamped };
    (clamped, clamped != value)
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::{SettingsContext, parse_str};
    use oxtuner_tune::Layout;

    use super::*;

    fn spec(name: &str) -> VariableSpec {
        let definition = parse_str(
            "\
[MegaTune]
signature = \"speeduino 202501\"

[Constants]
nPages = 1
pageSize = 8
page = 1
fuel = scalar, U08, 0, \"ms\", 0.1, 0, 0, 25.5, 1
",
            &SettingsContext::new(),
        )
        .expect("parses");
        Layout::build(&definition, &|_| None)
            .variable(name)
            .expect("a variable")
            .clone()
    }

    #[test]
    fn clamping_stays_inside_the_declared_range() {
        let fuel = spec("fuel");

        assert_eq!(clamp_user(&fuel, 12.0), (12.0, false));
        assert_eq!(clamp_user(&fuel, 100.0), (25.5, true));
        assert_eq!(clamp_user(&fuel, -4.0), (0.0, true));
    }

    #[test]
    fn a_negative_zero_is_not_a_change() {
        let fuel = spec("fuel");

        let (value, changed) = clamp_user(&fuel, -0.0);
        assert!(!changed, "the sign of zero is not a new value");
        assert_eq!(value.to_bits(), 0.0_f64.to_bits());
    }

    #[test]
    fn writes_start_disabled_and_a_halt_survives_enabling() {
        let mut safety = Safety::new(false);
        assert!(safety.gate(true, true, true, None).is_err());
        assert!(safety.halted().is_none());

        safety.set_writes_enabled(true);
        assert!(safety.gate(true, true, true, None).is_ok());

        safety.halt("page 2 differs");
        assert!(safety.halted().is_some());
        safety.set_writes_enabled(false);
        safety.set_writes_enabled(true);
        assert!(
            safety.gate(true, true, true, None).is_err(),
            "enabling writes must not clear a halt"
        );

        safety.clear_halt();
        assert!(safety.gate(true, true, true, None).is_ok());
    }

    #[test]
    fn every_gate_has_a_reason() {
        let safety = Safety::new(true);

        assert!(safety.gate(false, true, true, None).is_err());
        assert!(safety.gate(true, false, true, None).is_err());
        assert!(safety.gate(true, true, false, None).is_err());
        assert!(safety.gate(true, true, true, Some("signature")).is_err());
    }
}
