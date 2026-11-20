//! The expression builtin registry.
//!
//! One table names every function the dialect can write. Each entry declares
//! its arity, whether it folds to a number, and whether it is pure. Pure math
//! folds at parse time when every argument is a literal; string functions stay
//! symbolic because they re-resolve against presentation state, and runtime
//! helpers (history, lookup, channel introspection, time) stay deferred calls.
//! No builtin reads a file, a socket or the clock.
//!
//! Unknown names are still valid expressions. The parser keeps them as opaque
//! [`crate::expr::Expr::Call`] nodes and reports `fold-deferred`, so a name this
//! build does not know can never be mistaken for a number it computed.

use std::fmt;

/// How many arguments a builtin accepts.
#[expect(
    clippy::exhaustive_enums,
    reason = "the three arity shapes cover the function table"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arity {
    /// Exactly this many.
    Exact(usize),
    /// This many or more.
    AtLeast(usize),
    /// Between the two counts, inclusive.
    Between(usize, usize),
}

impl Arity {
    /// Whether a call with `count` arguments matches this builtin.
    pub fn accepts(self, count: usize) -> bool {
        match self {
            Self::Exact(expected) => count == expected,
            Self::AtLeast(minimum) => count >= minimum,
            Self::Between(minimum, maximum) => (minimum..=maximum).contains(&count),
        }
    }
}

impl fmt::Display for Arity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exact(count) => write!(formatter, "{count}"),
            Self::AtLeast(minimum) => write!(formatter, "{minimum}+"),
            Self::Between(minimum, maximum) => write!(formatter, "{minimum}..{maximum}"),
        }
    }
}

/// What a call to this builtin means to the parser.
#[expect(
    clippy::exhaustive_enums,
    reason = "the three kinds cover how a call can be treated"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinKind {
    /// Pure maths with a literal result, folded at parse time.
    PureMath,
    /// Produces text for a presentation attribute; stays symbolic.
    String,
    /// Needs runtime state (history, channels, time); stays a deferred call.
    Deferred,
}

/// One entry in the registry.
#[derive(Debug, Clone, Copy)]
pub struct Builtin {
    /// Name as the dialect writes it, without a leading `$`.
    pub name: &'static str,
    /// Accepted argument count.
    pub arity: Arity,
    /// How the parser treats a call.
    pub kind: BuiltinKind,
    /// Pure numeric evaluation. Never reads anything but its arguments.
    fold: fn(&[f64]) -> Option<f64>,
}

impl Builtin {
    /// Folds the call when this builtin is pure numeric and the arity matches.
    pub fn fold(&self, arguments: &[f64]) -> Option<f64> {
        if self.kind == BuiltinKind::PureMath && self.arity.accepts(arguments.len()) {
            (self.fold)(arguments)
        } else {
            None
        }
    }
}

macro_rules! math {
    ($name:literal, $arity:expr, $function:expr) => {
        Builtin {
            name: $name,
            arity: $arity,
            kind: BuiltinKind::PureMath,
            fold: |arguments: &[f64]| $function(arguments),
        }
    };
}

macro_rules! symbolic {
    ($name:literal, $arity:expr, $kind:expr) => {
        Builtin {
            name: $name,
            arity: $arity,
            kind: $kind,
            fold: |_arguments: &[f64]| None,
        }
    };
}

/// Every builtin the dialect defines. Kept in one place so arity and fold
/// behaviour cannot drift between evaluation and validation.
pub static BUILTINS: &[Builtin] = &[
    math!("abs", Arity::Exact(1), |a: &[f64]| Some(a.first()?.abs())),
    math!("sqrt", Arity::Exact(1), |a: &[f64]| Some(a.first()?.sqrt())),
    math!("round", Arity::Exact(1), |a: &[f64]| Some(
        a.first()?.round()
    )),
    math!("floor", Arity::Exact(1), |a: &[f64]| Some(
        a.first()?.floor()
    )),
    math!("ceil", Arity::Exact(1), |a: &[f64]| Some(a.first()?.ceil())),
    math!("exp", Arity::Exact(1), |a: &[f64]| Some(a.first()?.exp())),
    math!("log", Arity::Exact(1), |a: &[f64]| Some(a.first()?.ln())),
    math!("log10", Arity::Exact(1), |a: &[f64]| Some(
        a.first()?.log10()
    )),
    math!("recip", Arity::Exact(1), |a: &[f64]| Some(1.0 / a.first()?)),
    math!("pow", Arity::Exact(2), |a: &[f64]| {
        Some(a.first()?.powf(*a.get(1)?))
    }),
    math!("min", Arity::AtLeast(1), |a: &[f64]| {
        a.iter().copied().reduce(f64::min)
    }),
    math!("max", Arity::AtLeast(1), |a: &[f64]| {
        a.iter().copied().reduce(f64::max)
    }),
    math!("sin", Arity::Exact(1), |a: &[f64]| Some(a.first()?.sin())),
    math!("cos", Arity::Exact(1), |a: &[f64]| Some(a.first()?.cos())),
    math!("tan", Arity::Exact(1), |a: &[f64]| Some(a.first()?.tan())),
    math!("asin", Arity::Exact(1), |a: &[f64]| Some(a.first()?.asin())),
    math!("acos", Arity::Exact(1), |a: &[f64]| Some(a.first()?.acos())),
    math!("atan", Arity::Exact(1), |a: &[f64]| Some(a.first()?.atan())),
    math!("isnan", Arity::Exact(1), |a: &[f64]| {
        Some(f64::from(a.first()?.is_nan()))
    }),
    symbolic!("stringvalue", Arity::Exact(1), BuiltinKind::String),
    symbolic!("bitstringvalue", Arity::Exact(2), BuiltinKind::String),
    symbolic!("getprojectsdirpath", Arity::Exact(0), BuiltinKind::String),
    symbolic!("getworkingdirpath", Arity::Exact(0), BuiltinKind::String),
    symbolic!("arrayvalue", Arity::Exact(2), BuiltinKind::Deferred),
    symbolic!("table", Arity::Exact(2), BuiltinKind::Deferred),
    symbolic!("tablelookup", Arity::Between(4, 5), BuiltinKind::Deferred),
    symbolic!("lastvalue", Arity::Exact(1), BuiltinKind::Deferred),
    symbolic!("historicalvalue", Arity::Exact(2), BuiltinKind::Deferred),
    symbolic!("pastvalue", Arity::Exact(2), BuiltinKind::Deferred),
    // Real loggers call `maxValue(refTime)` as well as the two-argument form.
    symbolic!("maxvalue", Arity::Between(1, 2), BuiltinKind::Deferred),
    symbolic!("minvalue", Arity::Between(1, 2), BuiltinKind::Deferred),
    symbolic!("smoothbasic", Arity::Exact(2), BuiltinKind::Deferred),
    symbolic!("smoothfiltered", Arity::Exact(2), BuiltinKind::Deferred),
    symbolic!("accumulate", Arity::Exact(1), BuiltinKind::Deferred),
    symbolic!(
        "persistentaccumulate",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!("selectexpression", Arity::AtLeast(2), BuiltinKind::Deferred),
    symbolic!("accelhp", Arity::Exact(3), BuiltinKind::Deferred),
    symbolic!("aerodynamicdraghp", Arity::Exact(3), BuiltinKind::Deferred),
    symbolic!("rollingdraghp", Arity::Exact(3), BuiltinKind::Deferred),
    symbolic!(
        "getchannelvaluebyoffset",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "getchannelscalebyoffset",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "getchannetranslatebyoffset",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "getchanneldigitsbyoffset",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "getchannelminbyoffset",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "getchannelmaxbyoffset",
        Arity::Exact(1),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "getworkinglocalcanid",
        Arity::Exact(0),
        BuiltinKind::Deferred
    ),
    symbolic!("getlogtime", Arity::Exact(0), BuiltinKind::Deferred),
    symbolic!("isonline", Arity::Exact(0), BuiltinKind::Deferred),
    symbolic!(
        "isadvancedmathavailable",
        Arity::Exact(0),
        BuiltinKind::Deferred
    ),
    symbolic!(
        "highspeedrecordnumber",
        Arity::Exact(0),
        BuiltinKind::Deferred
    ),
];

/// Finds a builtin by name, accepting the `$` spelling used in path helpers.
pub fn lookup(name: &str) -> Option<&'static Builtin> {
    let name = name.strip_prefix('$').unwrap_or(name);
    BUILTINS
        .iter()
        .find(|builtin| builtin.name.eq_ignore_ascii_case(name))
}

/// Folds a call when the name is a known pure math builtin and the arity fits.
///
/// Everything else returns `None`; the caller keeps the call node rather than
/// substituting a value. The `arrayValue` helper is handled by the evaluator's
/// [`crate::expr::EvalContext`] because it needs an array lookup, not just its
/// numeric arguments.
pub fn fold(name: &str, arguments: &[f64]) -> Option<f64> {
    lookup(name)?.fold(arguments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_math_folds_with_its_declared_arity() {
        let minimum = lookup("MIN").expect("min is registered");
        assert_eq!(minimum.fold(&[3.0, 1.0]), Some(1.0));
        assert_eq!(fold("pow", &[2.0, 3.0]), Some(8.0));
        assert_eq!(fold("pow", &[2.0]), None, "wrong arity never folds");
    }

    #[test]
    fn string_and_runtime_builtins_never_fold() {
        let string_value = lookup("$stringValue").expect("the $ spelling is accepted");
        assert_eq!(string_value.kind, BuiltinKind::String);
        assert_eq!(fold("bitStringValue", &[1.0, 2.0]), None);
        assert_eq!(fold("pastValue", &[1.0, 2.0]), None);
        assert_eq!(fold("table", &[1.0, 2.0]), None);
    }

    #[test]
    fn the_history_helpers_accept_the_arities_real_files_write() {
        // Speeduino and FOME write `maxValue(refTime)`, rusEFI writes
        // `pastValue(time, 1)`.
        for call in [
            "maxValue(refTime)",
            "minValue(refTime)",
            "pastValue(time, 1)",
        ] {
            let expression = crate::expr::Expr::parse(call).expect("parses");
            for (name, arity) in expression.call_sites() {
                let builtin = lookup(name).expect("the helper is registered");
                assert!(
                    builtin.arity.accepts(arity),
                    "{call}: {name} rejects {arity} argument(s)"
                );
            }
        }
    }

    #[test]
    fn an_unknown_name_is_not_in_the_registry() {
        assert!(lookup("someFutureBuiltin").is_none());
        assert_eq!(fold("someFutureBuiltin", &[1.0]), None);
    }
}
