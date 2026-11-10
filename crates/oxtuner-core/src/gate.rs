//! Sandboxed evaluation of the visibility and enable expressions a definition
//! puts on dialogs and menu entries.
//!
//! The definition layer parses every `{ … }` field into an [`Expr`]; this module
//! supplies the values those expressions read. A gate that cannot be resolved is
//! reported as [`Gate::Unresolved`], and callers keep the gated item available:
//! a gap in the evaluator must never lock a user out of a setting.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::rc::Rc;

use oxtuner_ini::expr::{EvalContext, Expr};
use oxtuner_ini::{ChannelKind, Definition};
use oxtuner_tune::Tune;

use crate::contract::GateState;
use crate::realtime::RealtimeFrame;

/// How one gate expression resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Gate {
    /// The expression evaluated to a non-zero value.
    Open,
    /// The expression evaluated to zero.
    Closed,
    /// The expression or one of its values could not be resolved.
    Unresolved,
}

impl Gate {
    /// Whether the gated item stays available. An unresolved gate stays open.
    fn passes(self) -> bool {
        !matches!(self, Self::Closed)
    }

    /// Whether the gate could not be evaluated.
    fn is_unresolved(self) -> bool {
        matches!(self, Self::Unresolved)
    }
}

thread_local! {
    /// Parsed gate expressions, keyed by source text.
    ///
    /// Definitions repeat a gate on many rows (a panel gate is inherited by
    /// every row it embeds), and a dialog is read again after every edit.
    /// Parsing is the expensive part, so the cache follows the core thread
    /// instead of one summary build.
    static PARSED_GATES: RefCell<HashMap<String, Option<Rc<Expr>>>> =
        RefCell::new(HashMap::new());
}

/// How many distinct gate expressions the parser cache keeps.
const PARSED_GATE_LIMIT: usize = 4_096;

fn parse_gate(expression: &str) -> Option<Rc<Expr>> {
    PARSED_GATES.with_borrow_mut(|cache| {
        if let Some(parsed) = cache.get(expression) {
            return parsed.clone();
        }

        let parsed = Expr::parse(expression).ok().map(Rc::new);
        if cache.len() >= PARSED_GATE_LIMIT {
            cache.clear();
        }
        cache.insert(expression.to_owned(), parsed.clone());
        parsed
    })
}

/// Resolves definition gates against the working tune.
///
/// Values are looked up in this order: the newest realtime frame, computed
/// output channels (recursively), a continuous PC variable's channel, then the
/// working tune. The frame is copied once when the evaluator is built, so a
/// dialog snapshot always sees one coherent frame.
pub(crate) struct GateEvaluator<'tune> {
    tune: &'tune Tune,
    live: HashMap<String, f64>,
    computed: HashMap<String, Expr>,
    continuous: HashMap<String, String>,
    values: HashMap<String, Option<f64>>,
    resolving: HashSet<String>,
    arrays: HashMap<String, HashMap<u64, Option<f64>>>,
    unresolved: BTreeSet<String>,
}

impl<'tune> GateEvaluator<'tune> {
    /// Builds an evaluator for one definition and working tune.
    pub(crate) fn new(
        definition: &Definition,
        tune: &'tune Tune,
        live: Option<&RealtimeFrame>,
    ) -> Self {
        let mut computed = HashMap::new();
        for (name, channel) in definition.channels.iter() {
            if let ChannelKind::Computed(expression) = &channel.kind {
                computed.insert(name.to_ascii_lowercase(), expression.clone());
            }
        }

        let mut continuous = HashMap::new();
        for (name, variable) in definition.pc_variables.iter() {
            if let Some(channel) = &variable.continuous_channel_value {
                continuous.insert(name.to_ascii_lowercase(), channel.to_ascii_lowercase());
            }
        }

        let live = live.map_or_else(HashMap::new, |frame| {
            frame
                .values
                .iter()
                .map(|value| (value.name.to_ascii_lowercase(), value.value))
                .collect()
        });

        Self {
            tune,
            live,
            computed,
            continuous,
            values: HashMap::new(),
            resolving: HashSet::new(),
            arrays: HashMap::new(),
            unresolved: BTreeSet::new(),
        }
    }

    /// Evaluates one gate expression, caching the parse.
    pub(crate) fn gate(&mut self, expression: &str) -> Gate {
        let expression = expression.trim();
        if expression.is_empty() {
            return Gate::Open;
        }

        let Some(parsed) = parse_gate(expression) else {
            self.unresolved.insert(expression.to_owned());
            return Gate::Unresolved;
        };

        match parsed.evaluate_with(self) {
            Some(value) if is_true(value) => Gate::Open,
            Some(_) => Gate::Closed,
            None => {
                self.unresolved.insert(expression.to_owned());
                Gate::Unresolved
            }
        }
    }

    /// The gate expressions that did not resolve, in a stable order.
    pub(crate) fn unresolved(&self) -> impl Iterator<Item = &str> {
        self.unresolved.iter().map(String::as_str)
    }

    fn value(&mut self, name: &str) -> Option<f64> {
        if let Some(cached) = self.values.get(name) {
            return *cached;
        }

        let key = name.to_ascii_lowercase();
        if key != name
            && let Some(cached) = self.values.get(&key)
        {
            return *cached;
        }

        // A computed channel that depends on itself has no value; returning
        // early leaves the whole gate unresolved instead of recursing forever.
        if !self.resolving.insert(key.clone()) {
            return None;
        }

        let resolved = self.resolve(name, &key);
        self.resolving.remove(&key);
        self.values.insert(name.to_owned(), resolved);
        if key != name {
            self.values.insert(key, resolved);
        }
        resolved
    }

    fn resolve(&mut self, name: &str, key: &str) -> Option<f64> {
        if let Some(value) = self.live.get(name).or_else(|| self.live.get(key)) {
            return Some(*value);
        }

        if let Some(expression) = self
            .computed
            .get(name)
            .or_else(|| self.computed.get(key))
            .cloned()
        {
            return expression.evaluate_with(self);
        }

        if let Some(channel) = self
            .continuous
            .get(name)
            .or_else(|| self.continuous.get(key))
            .cloned()
            && let Some(value) = self.live.get(&channel)
        {
            return Some(*value);
        }

        self.tune.scalar_user(name).ok()
    }

    fn array_value(&mut self, name: &str, index: f64) -> Option<f64> {
        if !index.is_finite() {
            return None;
        }

        let name = array_name(name);
        let bits = index.to_bits();

        if let Some(cached) = self
            .arrays
            .get(name)
            .and_then(|by_index| by_index.get(&bits))
        {
            return *cached;
        }

        let resolved = self.interpolate(name, index);
        self.arrays
            .entry(name.to_owned())
            .or_default()
            .insert(bits, resolved);
        resolved
    }

    /// Reads one array element, interpolating a fractional index and clamping
    /// an out-of-range one, as the dialect defines.
    fn interpolate(&self, name: &str, index: f64) -> Option<f64> {
        let length = self.tune.value(name)?.len();
        if length == 0 {
            return None;
        }

        let last = length - 1;

        if index <= 0.0 {
            return self.tune.user_at(name, 0).ok();
        }

        let last_index = length_f64(last);
        if index >= last_index {
            return self.tune.user_at(name, last).ok();
        }

        let lower = floor_index(index);
        let upper = lower + 1;
        let fraction = index - length_f64(lower);
        let low = self.tune.user_at(name, lower).ok()?;
        let high = self.tune.user_at(name, upper).ok()?;

        Some(low + fraction * (high - low))
    }
}

impl EvalContext for GateEvaluator<'_> {
    fn value(&mut self, name: &str) -> Option<f64> {
        self.value(name)
    }

    fn array_value(&mut self, name: &str, index: f64) -> Option<f64> {
        self.array_value(name, index)
    }
}

/// Evaluates the visibility and enable gates one dialog summary row carries.
pub(crate) fn resolve_gates(
    evaluator: &mut GateEvaluator<'_>,
    visibility: Option<&str>,
    enable: Option<&str>,
) -> GateState {
    let visible = visibility.map_or(Gate::Open, |expression| evaluator.gate(expression));
    let enabled = enable.map_or(Gate::Open, |expression| evaluator.gate(expression));

    GateState {
        visible: visible.passes(),
        enabled: enabled.passes(),
        unresolved: visible.is_unresolved() || enabled.is_unresolved(),
    }
}

fn array_name(name: &str) -> &str {
    let name = name.trim();

    match name.get(..6) {
        Some(prefix) if prefix.eq_ignore_ascii_case("array.") => name.get(6..).unwrap_or(name),
        _ => name,
    }
}

fn is_true(value: f64) -> bool {
    value != 0.0
}

#[expect(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the index is finite and has already been clamped to the array bounds"
)]
fn floor_index(index: f64) -> usize {
    index.floor() as usize
}

#[expect(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "array lengths in a definition are far below 2^53"
)]
fn length_f64(length: usize) -> f64 {
    length as f64
}

#[cfg(test)]
mod tests {
    use oxtuner_ini::{SettingsContext, parse_bytes};
    use oxtuner_tune::Tune;

    use super::*;
    use crate::realtime::LiveValue;

    const DEFINITION: &str = r##"
[MegaTune]
signature = "gate test"

[Constants]
nPages = 1
pageSize = 16
page = 1
flag = scalar, U08, 0, "#", 1, 0, 0, 1, 0
zero = scalar, U08, 5, "#", 1, 0, 0, 1, 0
arr = array, U08, 1, [4], "#", 1, 0, 0, 255, 0

[PcVariables]
live = continuousChannelValue, rpm

[ConstantsExtensions]
defaultValue = flag, 1
defaultValue = arr, 10 20 30 40

[OutputChannels]
computed = { flag + 2 }
cycleA = { cycleB }
cycleB = { cycleA }
"##;

    fn tune() -> Tune {
        let definition = parse_bytes(DEFINITION.as_bytes(), &SettingsContext::new())
            .expect("the test definition parses");
        let mut tune = Tune::from_definition(&definition);

        for (index, value) in [10.0, 20.0, 30.0, 40.0].into_iter().enumerate() {
            tune.set_user_at("arr", index, value)
                .expect("the array element is writable");
        }

        tune
    }

    fn evaluator(tune: &Tune) -> GateEvaluator<'_> {
        let definition = parse_bytes(DEFINITION.as_bytes(), &SettingsContext::new())
            .expect("the test definition parses");

        GateEvaluator::new(&definition, tune, None)
    }

    #[test]
    fn constants_and_computed_channels_evaluate() {
        let tune = tune();
        let mut gates = evaluator(&tune);

        assert_eq!(gates.gate("flag"), Gate::Open);
        assert_eq!(gates.gate("zero"), Gate::Closed);
        assert_eq!(gates.gate("computed == 3"), Gate::Open);
        assert_eq!(gates.gate("computed != 3"), Gate::Closed);
    }

    #[test]
    fn an_unresolved_name_stays_open() {
        let tune = tune();
        let mut gates = evaluator(&tune);
        let gate = gates.gate("missing > 0");

        assert_eq!(gate, Gate::Unresolved);
        assert!(gate.passes(), "a gate we cannot read must not hide a row");
        assert_eq!(gates.unresolved().collect::<Vec<_>>(), vec!["missing > 0"]);
    }

    #[test]
    fn a_malformed_expression_stays_open() {
        let tune = tune();
        let mut gates = evaluator(&tune);

        assert_eq!(gates.gate("("), Gate::Unresolved);
    }

    #[test]
    fn a_computed_channel_cycle_is_unresolved() {
        let tune = tune();
        let mut gates = evaluator(&tune);

        assert_eq!(gates.gate("cycleA > 0"), Gate::Unresolved);
    }

    #[test]
    fn array_value_interpolates_and_clamps() {
        let tune = tune();
        let mut gates = evaluator(&tune);

        assert_eq!(gates.gate("arrayValue(array.arr, 1) == 20"), Gate::Open);
        assert_eq!(gates.gate("arrayValue(array.arr, 1.5) == 25"), Gate::Open);
        assert_eq!(gates.gate("arrayValue(array.arr, -2) == 10"), Gate::Open);
        assert_eq!(gates.gate("arrayValue(array.arr, 99) == 40"), Gate::Open);
        assert_eq!(gates.gate("arr[1] == 20"), Gate::Open);
        assert_eq!(gates.gate("arr[1.5] == 25"), Gate::Open);
    }

    #[test]
    fn an_array_without_a_value_is_unresolved() {
        let tune = tune();
        let mut gates = evaluator(&tune);

        assert_eq!(
            gates.gate("arrayValue(array.missing, 0) == 0"),
            Gate::Unresolved
        );
    }

    #[test]
    fn a_live_channel_wins_over_the_tune() {
        let tune = tune();
        let definition = parse_bytes(DEFINITION.as_bytes(), &SettingsContext::new())
            .expect("the test definition parses");
        let frame = RealtimeFrame {
            sequence: 1,
            values: vec![LiveValue {
                name: "rpm".to_owned(),
                value: 1_000.0,
                text: "1000".to_owned(),
                units: "rpm".to_owned(),
            }],
            indicators: Vec::new(),
        };
        let mut gates = GateEvaluator::new(&definition, &tune, Some(&frame));

        assert_eq!(gates.gate("rpm > 500"), Gate::Open);
        assert_eq!(gates.gate("live > 500"), Gate::Open);
    }

    #[test]
    fn resolve_gates_keeps_the_two_slots_apart() {
        let tune = tune();
        let mut gates = evaluator(&tune);

        let state = resolve_gates(&mut gates, Some("flag"), Some("zero"));
        assert!(state.visible);
        assert!(!state.enabled);
        assert!(!state.unresolved);

        let state = resolve_gates(&mut gates, Some("flag"), Some("missing"));
        assert!(state.visible);
        assert!(state.enabled, "an unresolved enable gate stays enabled");
        assert!(state.unresolved);
    }
}
