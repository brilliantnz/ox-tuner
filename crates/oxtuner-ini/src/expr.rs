//! The expression language used in `{ … }` fields.
//!
//! Two evaluation models matter (ADR-0006, reference §Expressions):
//!
//! - **memory-mapping** attributes (offset, shape) resolve at load time;
//! - **presentation** attributes (scale, limits, digits) re-evaluate at runtime
//!   as settings change, so they must not be baked into numbers here.
//!
//! The parser therefore keeps every expression as an AST. [`Expr::evaluate`]
//! exists for the cases the definition layer may safely fold (literal
//! arithmetic, and references to constants whose values are known), and returns
//! `None` otherwise — never a guess.
//!
//! Parsing never panics and never loops: the tokenizer walks the input once and
//! recursive descent is bounded by the input length.

use std::fmt;

/// An expression from a `{ … }` field.
#[expect(
    clippy::exhaustive_enums,
    reason = "the expression grammar is a closed set defined by the INI dialect"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub enum Expr {
    /// A numeric literal.
    Number(f64),
    /// A reference to a constant, PC variable or output channel.
    Ident(String),
    /// A prefix operator.
    Unary(UnaryOp, Box<Expr>),
    /// An infix operator.
    Binary(BinaryOp, Box<Expr>, Box<Expr>),
    /// `condition ? when_true : when_false`.
    Ternary(Box<Expr>, Box<Expr>, Box<Expr>),
    /// A function call, kept unresolved: real INIs call functions this build
    /// does not implement, and failing to *parse* them would break the file.
    Call(String, Vec<Expr>),
    /// One array element written as `constant[index]`, as Speeduino gates do.
    Index(Box<Expr>, Box<Expr>),
}

/// Prefix operators.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines exactly these prefix operators"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    /// Arithmetic negation.
    Negate,
    /// Logical `!`.
    Not,
    /// Bitwise `~`.
    BitNot,
    /// Unary `+`, kept for round-tripping.
    Plus,
}

/// Infix operators, loosest binding first in the parser.
#[expect(
    clippy::exhaustive_enums,
    reason = "the INI dialect defines exactly these infix operators"
)]
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryOp {
    /// `||`
    Or,
    /// `&&`
    And,
    /// `|`
    BitOr,
    /// `^`
    BitXor,
    /// `&`
    BitAnd,
    /// `==`
    Equal,
    /// `!=`
    NotEqual,
    /// `<`
    Less,
    /// `<=`
    LessEqual,
    /// `>`
    Greater,
    /// `>=`
    GreaterEqual,
    /// `<<`
    ShiftLeft,
    /// `>>`
    ShiftRight,
    /// `+`
    Add,
    /// `-`
    Subtract,
    /// `*`
    Multiply,
    /// `/`
    Divide,
    /// `%`
    Modulo,
}

impl fmt::Display for BinaryOp {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Or => "||",
            Self::And => "&&",
            Self::BitOr => "|",
            Self::BitXor => "^",
            Self::BitAnd => "&",
            Self::Equal => "==",
            Self::NotEqual => "!=",
            Self::Less => "<",
            Self::LessEqual => "<=",
            Self::Greater => ">",
            Self::GreaterEqual => ">=",
            Self::ShiftLeft => "<<",
            Self::ShiftRight => ">>",
            Self::Add => "+",
            Self::Subtract => "-",
            Self::Multiply => "*",
            Self::Divide => "/",
            Self::Modulo => "%",
        })
    }
}

/// The values an expression can read while it is evaluated.
///
/// The definition layer parses every expression into [`Expr`]; the caller
/// supplies the values because they depend on where the expression is used
/// (a tune constant, a computed output channel, or a live frame).
pub trait EvalContext {
    /// Resolves one identifier, case-sensitively as the definition wrote it.
    fn value(&mut self, name: &str) -> Option<f64>;

    /// Resolves one `arrayValue(array.name, index)` lookup.
    ///
    /// `name` includes the `array.` prefix the dialect requires. The default
    /// returns `None`, so an evaluator without array support leaves the call
    /// unresolved instead of guessing.
    fn array_value(&mut self, name: &str, index: f64) -> Option<f64> {
        let _ = (name, index);
        None
    }

    /// Resolves one element written as `name[index]`.
    ///
    /// The dialect uses this form in menus and dialogs (`outputPin[0]`) beside
    /// the `arrayValue()` helper. The default forwards to
    /// [`Self::array_value`]; a context that supports arrays gets both forms.
    fn element(&mut self, name: &str, index: f64) -> Option<f64> {
        self.array_value(name, index)
    }

    /// Resolves a definition-defined function the pure evaluator does not
    /// implement, such as `getChannelScaleByOffset`.
    ///
    /// The default returns `None`, so an evaluator without definition context
    /// leaves the call unresolved instead of guessing.
    fn function(&mut self, name: &str, arguments: &[f64]) -> Option<f64> {
        let _ = (name, arguments);
        None
    }
}

/// An [`EvalContext`] over a plain identifier lookup.
struct Lookup<'a>(&'a dyn Fn(&str) -> Option<f64>);

impl EvalContext for Lookup<'_> {
    fn value(&mut self, name: &str) -> Option<f64> {
        (self.0)(name)
    }
}

impl Expr {
    /// Parses the body of an expression field (without braces).
    ///
    /// # Errors
    ///
    /// Returns [`ExprError`] with the offending position when the text is not
    /// a complete expression. An expression nested deeper than the parser's
    /// nesting cap is rejected the same way, so hostile input cannot exhaust
    /// the thread stack.
    pub fn parse(text: &str) -> Result<Self, ExprError> {
        let tokens = tokenize(text)?;
        let mut parser = Parser {
            tokens,
            index: 0,
            depth: 0,
        };
        let expression = parser.ternary()?;
        if parser.index != parser.tokens.len() {
            return Err(ExprError {
                message: format!("unexpected trailing input at {}", parser.describe_here()),
            });
        }
        Ok(expression)
    }

    /// Evaluates the expression with `lookup` supplying identifiers.
    ///
    /// Returns `None` for anything that cannot be folded exactly: an unresolved
    /// identifier, a divide by zero, an unknown function, a non-finite result.
    /// Callers must treat `None` as "leave it dynamic", never as zero.
    pub fn evaluate(&self, lookup: &dyn Fn(&str) -> Option<f64>) -> Option<f64> {
        self.evaluate_with(&mut Lookup(lookup))
    }

    /// Evaluates the expression against a full [`EvalContext`].
    ///
    /// This is [`Self::evaluate`] plus `arrayValue`: the context resolves both
    /// identifiers and array elements. Everything unresolved still yields
    /// `None`; an unknown function never becomes a guess. An expression whose
    /// tree is deeper than the traversal cap also yields `None`.
    pub fn evaluate_with(&self, context: &mut dyn EvalContext) -> Option<f64> {
        self.evaluate_at(context, 0)
    }

    /// Evaluates at `depth`, refusing to recurse past [`MAX_AST_DEPTH`].
    ///
    /// A flat expression such as `1+1+1+…` can be longer than the parser's
    /// nesting cap while still building a deep tree. The traversal cap keeps
    /// that tree from exhausting the stack; past it the value cannot be folded,
    /// so it stays dynamic.
    fn evaluate_at(&self, context: &mut dyn EvalContext, depth: u32) -> Option<f64> {
        if depth > MAX_AST_DEPTH {
            return None;
        }
        let next = depth + 1;

        let value = match self {
            Self::Number(number) => *number,
            Self::Ident(name) => context.value(name)?,
            Self::Unary(operator, operand) => {
                let operand = operand.evaluate_at(context, next)?;
                match operator {
                    UnaryOp::Negate => -operand,
                    UnaryOp::Plus => operand,
                    UnaryOp::Not => f64::from(operand == 0.0),
                    UnaryOp::BitNot => bit_float(!bit_integer(operand)),
                }
            }
            Self::Binary(operator, left, right) => {
                let left = left.evaluate_at(context, next)?;
                let right = right.evaluate_at(context, next)?;
                apply_binary(*operator, left, right)?
            }
            Self::Ternary(condition, when_true, when_false) => {
                if condition.evaluate_at(context, next)? == 0.0 {
                    when_false.evaluate_at(context, next)?
                } else {
                    when_true.evaluate_at(context, next)?
                }
            }
            Self::Index(base, index) => {
                let Self::Ident(name) = base.as_ref() else {
                    return None;
                };
                let index = index.evaluate_at(context, next)?;
                context.element(name, index)?
            }
            Self::Call(name, arguments) if name.eq_ignore_ascii_case("arrayvalue") => {
                let [array, index] = arguments.as_slice() else {
                    return None;
                };
                let Self::Ident(array_name) = array else {
                    return None;
                };
                let index = index.evaluate_at(context, next)?;
                context.array_value(array_name, index)?
            }
            Self::Call(name, arguments) => {
                let values: Vec<f64> = arguments
                    .iter()
                    .map(|argument| argument.evaluate_at(context, next))
                    .collect::<Option<Vec<f64>>>()?;
                evaluate_call(name, &values).or_else(|| context.function(name, &values))?
            }
        };

        value.is_finite().then_some(value)
    }

    /// Every identifier the expression reads, for validation and dependency
    /// tracking.
    ///
    /// The walk is iterative, so a long flat expression cannot exhaust the
    /// stack through this path.
    pub fn identifiers(&self) -> Vec<&str> {
        let mut found = Vec::new();
        let mut pending = vec![self];

        while let Some(expression) = pending.pop() {
            match expression {
                Self::Ident(name) => found.push(name.as_str()),
                Self::Number(_) => {}
                Self::Unary(_, operand) => pending.push(operand),
                Self::Binary(_, left, right) => {
                    pending.push(right);
                    pending.push(left);
                }
                Self::Ternary(condition, when_true, when_false) => {
                    pending.push(when_false);
                    pending.push(when_true);
                    pending.push(condition);
                }
                Self::Call(_, arguments) => pending.extend(arguments.iter().rev()),
                Self::Index(base, index) => {
                    pending.push(index);
                    pending.push(base);
                }
            }
        }

        found
    }
}

/// A value in the 64-bit integer view the bitwise operators work on.
#[expect(
    clippy::cast_possible_truncation,
    clippy::as_conversions,
    reason = "the INI expression language defines bitwise operators on a 64-bit integer view"
)]
fn bit_integer(value: f64) -> i64 {
    value as i64
}

/// A bitwise result back in the expression language's float type.
#[expect(
    clippy::cast_precision_loss,
    clippy::as_conversions,
    reason = "the INI expression language stores every value as f64"
)]
fn bit_float(value: i64) -> f64 {
    value as f64
}

#[expect(
    clippy::float_cmp,
    reason = "the INI expression language compares values exactly, as the firmware does"
)]
fn apply_binary(operator: BinaryOp, left: f64, right: f64) -> Option<f64> {
    let boolean = |value: bool| Some(f64::from(value));
    match operator {
        BinaryOp::Or => boolean(left != 0.0 || right != 0.0),
        BinaryOp::And => boolean(left != 0.0 && right != 0.0),
        BinaryOp::BitOr => Some(bit_float(bit_integer(left) | bit_integer(right))),
        BinaryOp::BitXor => Some(bit_float(bit_integer(left) ^ bit_integer(right))),
        BinaryOp::BitAnd => Some(bit_float(bit_integer(left) & bit_integer(right))),
        BinaryOp::Equal => boolean(left == right),
        BinaryOp::NotEqual => boolean(left != right),
        BinaryOp::Less => boolean(left < right),
        BinaryOp::LessEqual => boolean(left <= right),
        BinaryOp::Greater => boolean(left > right),
        BinaryOp::GreaterEqual => boolean(left >= right),
        BinaryOp::ShiftLeft => {
            let amount = u32::try_from(bit_integer(right) & 63).unwrap_or(0);
            Some(bit_float(bit_integer(left) << amount))
        }
        BinaryOp::ShiftRight => {
            let amount = u32::try_from(bit_integer(right) & 63).unwrap_or(0);
            Some(bit_float(bit_integer(left) >> amount))
        }
        BinaryOp::Add => Some(left + right),
        BinaryOp::Subtract => Some(left - right),
        BinaryOp::Multiply => Some(left * right),
        BinaryOp::Divide => (right != 0.0).then_some(left / right),
        BinaryOp::Modulo => (right != 0.0).then(|| left % right),
    }
}

/// The pure functions the evaluator implements. Everything else stays a
/// [`Expr::Call`] and simply does not fold.
fn evaluate_call(name: &str, arguments: &[f64]) -> Option<f64> {
    let lowered = name.to_ascii_lowercase();
    match lowered.as_str() {
        "abs" => Some(arguments.first()?.abs()),
        "sqrt" => Some(arguments.first()?.sqrt()),
        "round" => Some(arguments.first()?.round()),
        "floor" => Some(arguments.first()?.floor()),
        "ceil" => Some(arguments.first()?.ceil()),
        "exp" => Some(arguments.first()?.exp()),
        "log" => Some(arguments.first()?.ln()),
        "log10" => Some(arguments.first()?.log10()),
        "recip" => Some(1.0 / arguments.first()?),
        "pow" => Some(arguments.first()?.powf(*arguments.get(1)?)),
        "min" => arguments.iter().copied().reduce(f64::min),
        "max" => arguments.iter().copied().reduce(f64::max),
        _ => None,
    }
}

/// Why an expression could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExprError {
    /// What went wrong, with the offending position.
    pub message: String,
}

impl fmt::Display for ExprError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExprError {}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Number(f64),
    Ident(String),
    /// A punctuation or operator. Every symbol is ASCII, so the token borrows
    /// a static string instead of allocating one per operator.
    Symbol(&'static str),
}

fn tokenize(text: &str) -> Result<Vec<Token>, ExprError> {
    let characters: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut index = 0usize;

    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }

        if character.is_ascii_digit() || character == '.' {
            let start = index;
            if character == '0' && matches!(characters.get(index + 1), Some('x' | 'X')) {
                index += 2;
                while index < characters.len() && characters[index].is_ascii_hexdigit() {
                    index += 1;
                }
                let literal: String = characters[start + 2..index].iter().collect();
                let value = u32::from_str_radix(&literal, 16).map_err(|_| ExprError {
                    message: format!("bad hex literal at {start}"),
                })?;
                tokens.push(Token::Number(f64::from(value)));
                continue;
            }
            while index < characters.len()
                && (characters[index].is_ascii_digit() || characters[index] == '.')
            {
                index += 1;
            }
            let literal: String = characters[start..index].iter().collect();
            let value = literal.parse::<f64>().map_err(|_| ExprError {
                message: format!("bad number {literal:?} at {start}"),
            })?;
            tokens.push(Token::Number(value));
            continue;
        }

        if character.is_alphabetic() || character == '_' || character == '$' {
            let start = index;
            while index < characters.len()
                && (characters[index].is_alphanumeric()
                    || matches!(characters[index], '_' | '$' | '.'))
            {
                index += 1;
            }
            let name: String = characters[start..index].iter().collect();
            tokens.push(Token::Ident(name));
            continue;
        }

        if let Some(symbol) = two_char_symbol(character, characters.get(index + 1).copied()) {
            tokens.push(Token::Symbol(symbol));
            index += 2;
            continue;
        }

        if let Some(symbol) = one_char_symbol(character) {
            tokens.push(Token::Symbol(symbol));
            index += 1;
            continue;
        }

        return Err(ExprError {
            message: format!("unexpected character {character:?} at {index}"),
        });
    }

    Ok(tokens)
}

/// The two-character operator starting with these characters, if any.
///
/// Symbol tokens borrow static text, so both helpers are plain matches instead
/// of allocating a string for every operator in every expression.
fn two_char_symbol(character: char, next: Option<char>) -> Option<&'static str> {
    match (character, next) {
        ('&', Some('&')) => Some("&&"),
        ('|', Some('|')) => Some("||"),
        ('=', Some('=')) => Some("=="),
        ('!', Some('=')) => Some("!="),
        ('<', Some('=')) => Some("<="),
        ('>', Some('=')) => Some(">="),
        ('<', Some('<')) => Some("<<"),
        ('>', Some('>')) => Some(">>"),
        _ => None,
    }
}

/// The single-character punctuation the grammar accepts.
fn one_char_symbol(character: char) -> Option<&'static str> {
    Some(match character {
        '+' => "+",
        '-' => "-",
        '*' => "*",
        '/' => "/",
        '%' => "%",
        '(' => "(",
        ')' => ")",
        ',' => ",",
        '?' => "?",
        ':' => ":",
        '<' => "<",
        '>' => ">",
        '&' => "&",
        '|' => "|",
        '^' => "^",
        '~' => "~",
        '!' => "!",
        '[' => "[",
        ']' => "]",
        _ => return None,
    })
}

/// How deeply parentheses, calls and prefix operators may nest.
///
/// The parser is recursive descent, so hostile input must stop before it can
/// exhaust the thread stack. Depth is counted per expression, and the parser
/// is discarded on error, so a failed nested parse never has to unwind it.
const MAX_NESTING: u32 = 128;

/// How deep [`Expr::evaluate_with`] may recurse into a parsed tree.
///
/// The parser's [`MAX_NESTING`] counts parentheses and operators while it
/// parses; a flat chain such as `1+1+1+…` builds a tree as deep as its length
/// without ever nesting. This cap bounds that traversal too, and a value past
/// it stays dynamic instead of overflowing the stack.
const MAX_AST_DEPTH: u32 = 1024;

struct Parser {
    tokens: Vec<Token>,
    index: usize,
    depth: u32,
}

impl Parser {
    fn enter(&mut self) -> Result<(), ExprError> {
        self.depth += 1;
        if self.depth > MAX_NESTING {
            return Err(ExprError {
                message: format!("expression nests too deeply at token {}", self.index),
            });
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn peek_symbol(&self) -> Option<&str> {
        match self.tokens.get(self.index) {
            Some(Token::Symbol(symbol)) => Some(symbol),
            _ => None,
        }
    }

    fn describe_here(&self) -> String {
        match self.tokens.get(self.index) {
            None => "end of expression".to_owned(),
            Some(Token::Number(number)) => format!("number {number}"),
            Some(Token::Ident(name)) => format!("identifier {name}"),
            Some(Token::Symbol(symbol)) => format!("{symbol:?}"),
        }
    }

    fn eat(&mut self, symbol: &str) -> bool {
        if self.peek_symbol() == Some(symbol) {
            self.index += 1;
            return true;
        }
        false
    }

    fn expect(&mut self, symbol: &str) -> Result<(), ExprError> {
        if self.eat(symbol) {
            Ok(())
        } else {
            Err(ExprError {
                message: format!("expected {symbol:?} before {}", self.describe_here()),
            })
        }
    }

    fn ternary(&mut self) -> Result<Expr, ExprError> {
        let condition = self.binary(0)?;
        if self.eat("?") {
            self.enter()?;
            let when_true = self.ternary()?;
            self.leave();
            self.expect(":")?;

            self.enter()?;
            let when_false = self.ternary()?;
            self.leave();

            return Ok(Expr::Ternary(
                Box::new(condition),
                Box::new(when_true),
                Box::new(when_false),
            ));
        }
        Ok(condition)
    }

    /// Precedence climbing over [`BinaryOp`] levels.
    fn binary(&mut self, level: usize) -> Result<Expr, ExprError> {
        const LEVELS: &[&[(&str, BinaryOp)]] = &[
            &[("||", BinaryOp::Or)],
            &[("&&", BinaryOp::And)],
            &[("|", BinaryOp::BitOr)],
            &[("^", BinaryOp::BitXor)],
            &[("&", BinaryOp::BitAnd)],
            &[
                ("==", BinaryOp::Equal),
                ("!=", BinaryOp::NotEqual),
                ("<=", BinaryOp::LessEqual),
                (">=", BinaryOp::GreaterEqual),
                ("<", BinaryOp::Less),
                (">", BinaryOp::Greater),
            ],
            &[("<<", BinaryOp::ShiftLeft), (">>", BinaryOp::ShiftRight)],
            &[("+", BinaryOp::Add), ("-", BinaryOp::Subtract)],
            &[
                ("*", BinaryOp::Multiply),
                ("/", BinaryOp::Divide),
                ("%", BinaryOp::Modulo),
            ],
        ];

        if level == LEVELS.len() {
            return self.unary();
        }

        let mut left = self.binary(level + 1)?;
        loop {
            let mut matched = None;
            if let Some(symbol) = self.peek_symbol() {
                matched = LEVELS[level]
                    .iter()
                    .find(|(text, _)| *text == symbol)
                    .map(|(_, operator)| *operator);
            }
            let Some(operator) = matched else {
                return Ok(left);
            };
            self.index += 1;
            let right = self.binary(level + 1)?;
            left = Expr::Binary(operator, Box::new(left), Box::new(right));
        }
    }

    fn unary(&mut self) -> Result<Expr, ExprError> {
        let operator = match self.peek_symbol() {
            Some("-") => Some(UnaryOp::Negate),
            Some("+") => Some(UnaryOp::Plus),
            Some("!") => Some(UnaryOp::Not),
            Some("~") => Some(UnaryOp::BitNot),
            _ => None,
        };
        if let Some(operator) = operator {
            self.index += 1;

            self.enter()?;
            let operand = self.unary()?;
            self.leave();

            return Ok(Expr::Unary(operator, Box::new(operand)));
        }
        self.primary()
    }

    fn primary(&mut self) -> Result<Expr, ExprError> {
        match self.tokens.get(self.index).cloned() {
            Some(Token::Number(number)) => {
                self.index += 1;
                Ok(Expr::Number(number))
            }
            Some(Token::Ident(name)) => {
                self.index += 1;
                if self.eat("(") {
                    self.enter()?;
                    let mut arguments = Vec::new();
                    if !self.eat(")") {
                        loop {
                            arguments.push(self.ternary()?);
                            if self.eat(")") {
                                break;
                            }
                            self.expect(",")?;
                        }
                    }
                    self.leave();

                    return Ok(Expr::Call(name, arguments));
                }

                let base = Expr::Ident(name);
                if self.eat("[") {
                    self.enter()?;
                    let index = self.ternary()?;
                    self.leave();
                    self.expect("]")?;

                    return Ok(Expr::Index(Box::new(base), Box::new(index)));
                }
                Ok(base)
            }
            Some(Token::Symbol("(")) => {
                self.index += 1;

                self.enter()?;
                let inner = self.ternary()?;
                self.leave();
                self.expect(")")?;

                Ok(inner)
            }
            _ => Err(ExprError {
                message: format!("expected a value before {}", self.describe_here()),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_lookup(_: &str) -> Option<f64> {
        None
    }

    fn nesting_cap() -> usize {
        usize::try_from(MAX_NESTING).expect("the nesting cap fits in usize")
    }

    #[test]
    fn arithmetic_folds_when_every_value_is_literal() {
        let expression = Expr::parse("(7 / 4) * 2 + 1").expect("parses");

        assert_eq!(expression.evaluate(&no_lookup), Some(4.5));
    }

    #[test]
    fn an_unresolved_identifier_keeps_the_expression_dynamic() {
        let expression = Expr::parse("stoich * 2").expect("parses");

        assert_eq!(expression.evaluate(&no_lookup), None);
        assert_eq!(expression.identifiers(), vec!["stoich"]);
    }

    #[test]
    fn division_by_zero_is_not_folded_into_infinity() {
        let expression = Expr::parse("1 / 0").expect("parses");

        assert_eq!(expression.evaluate(&no_lookup), None);
    }

    #[test]
    fn a_ternary_selects_a_branch() {
        let expression = Expr::parse("flag ? 10 : 20").expect("parses");

        let value = expression.evaluate(&|name| (name == "flag").then_some(1.0));

        assert_eq!(value, Some(10.0));
    }

    #[test]
    fn an_unknown_function_still_parses_but_does_not_fold() {
        // Real INIs call functions this build does not implement; the file must
        // still load.
        let expression = Expr::parse("bitStringValue(algorithmUnits, algorithm)").expect("parses");

        assert_eq!(expression.evaluate(&no_lookup), None);
        assert_eq!(
            expression.identifiers(),
            vec!["algorithmUnits", "algorithm"]
        );
    }

    #[test]
    fn a_context_supplied_function_folds_after_the_builtins() {
        let expression =
            Expr::parse("getChannelScaleByOffset(9) * 2").expect("the expression parses");
        let mut context = TestContext::default();

        // Without a context hook the call stays unresolved.
        assert_eq!(expression.evaluate(&no_lookup), None);

        context.function_name = "getChannelScaleByOffset";
        context.function_result = Some(0.5);

        assert_eq!(expression.evaluate_with(&mut context), Some(1.0));
    }

    #[derive(Default)]
    struct TestContext {
        values: std::collections::HashMap<&'static str, f64>,
        arrays: std::collections::HashMap<&'static str, Vec<f64>>,
        function_name: &'static str,
        function_result: Option<f64>,
    }

    impl EvalContext for TestContext {
        fn value(&mut self, name: &str) -> Option<f64> {
            self.values.get(name).copied()
        }

        #[expect(
            clippy::as_conversions,
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "the test supplies small non-negative array indexes"
        )]
        fn array_value(&mut self, name: &str, index: f64) -> Option<f64> {
            let index = index.trunc() as usize;
            self.arrays.get(name)?.get(index).copied()
        }

        fn function(&mut self, name: &str, arguments: &[f64]) -> Option<f64> {
            let _ = arguments;
            (name == self.function_name)
                .then_some(self.function_result)
                .flatten()
        }
    }

    #[test]
    fn array_value_resolves_through_the_context() {
        let expression = Expr::parse("arrayValue(array.trim, 1) + 1").expect("parses");
        let mut context = TestContext::default();
        context.arrays.insert("array.trim", vec![10.0, 20.0]);

        assert_eq!(expression.evaluate_with(&mut context), Some(21.0));
    }

    #[test]
    fn array_value_needs_a_constant_name_and_two_arguments() {
        let mut context = TestContext::default();
        context.arrays.insert("array.trim", vec![10.0, 20.0]);

        for source in [
            "arrayValue(array.trim)",
            "arrayValue(array.trim, 0, 1)",
            "arrayValue(1 + 2, 0)",
        ] {
            let expression = Expr::parse(source).expect(source);

            assert_eq!(
                expression.evaluate_with(&mut context),
                None,
                "{source} must not fold"
            );
        }
    }

    #[test]
    fn an_indexed_constant_resolves_through_the_context() {
        let expression = Expr::parse("outputPin[index] + 1").expect("parses");
        let mut context = TestContext::default();
        context.values.insert("index", 1.0);
        context.arrays.insert("outputPin", vec![10.0, 20.0]);

        assert_eq!(expression.evaluate_with(&mut context), Some(21.0));
        assert_eq!(expression.identifiers(), vec!["outputPin", "index"]);
    }

    #[test]
    fn an_indexed_expression_needs_an_identifier_base() {
        let error = Expr::parse("(1 + 2)[0]").expect_err("only a constant name can be indexed");

        assert_ne!(error.message, "");
    }

    #[test]
    fn array_value_without_a_context_value_stays_unresolved() {
        let expression = Expr::parse("arrayValue(array.missing, 0)").expect("parses");
        let mut context = TestContext::default();

        assert_eq!(expression.evaluate_with(&mut context), None);
    }

    #[test]
    fn arithmetic_comparison_and_bitwise_operators_evaluate() {
        let cases = [
            ("1 + 2 * 3", 7.0),
            ("1 << 4", 16.0),
            ("16 >> 2", 4.0),
            ("5 & 3", 1.0),
            ("5 | 3", 7.0),
            ("5 ^ 3", 6.0),
            ("!0", 1.0),
            ("~0", -1.0),
            ("2 < 3 && 3 != 4", 1.0),
            ("0 || 0", 0.0),
            ("min(3, 1, 2)", 1.0),
            ("max(3, 1, 2)", 3.0),
        ];

        for (source, expected) in cases {
            let expression = Expr::parse(source).expect(source);
            assert_eq!(expression.evaluate(&no_lookup), Some(expected), "{source}");
        }
    }

    #[test]
    fn a_malformed_expression_reports_a_position_instead_of_panicking() {
        for source in ["1 +", "(", "a ? b", "1 ~ 2", "foo(", "1 2", "arr["] {
            let error = Expr::parse(source).expect_err(source);
            assert!(!error.message.is_empty(), "{source}");
        }
    }

    #[test]
    fn a_deeply_nested_expression_is_rejected_at_the_depth_cap() {
        let source = format!("{}1{}", "(".repeat(10_000), ")".repeat(10_000));

        let error = Expr::parse(&source).expect_err("must stop at the depth cap");

        assert!(error.message.contains("nests too deeply"));
    }

    #[test]
    fn an_expression_at_the_nesting_cap_parses() {
        let source = format!(
            "{}1{}",
            "(".repeat(nesting_cap()),
            ")".repeat(nesting_cap())
        );

        Expr::parse(&source).expect("the cap itself is allowed");
    }

    #[test]
    fn one_level_past_the_nesting_cap_is_rejected() {
        let source = format!(
            "{}1{}",
            "(".repeat(nesting_cap() + 1),
            ")".repeat(nesting_cap() + 1)
        );

        Expr::parse(&source).expect_err("one past the cap is too deep");
    }

    #[test]
    fn a_long_prefix_chain_is_rejected_at_the_depth_cap() {
        let source = format!("{}1", "!".repeat(10_000));

        Expr::parse(&source).expect_err("must stop at the depth cap");
    }

    #[test]
    fn sibling_nestings_do_not_accumulate_depth() {
        let branch = format!("{}1{}", "(".repeat(100), ")".repeat(100));

        Expr::parse(&format!("{branch} + {branch}")).expect("operands restore depth");
        Expr::parse(&format!("f({branch}, {branch})")).expect("arguments restore depth");
        Expr::parse(&format!("1 ? {branch} : {branch}")).expect("branches restore depth");
    }

    #[test]
    fn a_long_binary_chain_evaluates_without_unbounded_recursion() {
        let short = vec!["1"; 100].join("+");
        let expression = Expr::parse(&short).expect("parses");
        assert_eq!(expression.evaluate(&no_lookup), Some(100.0));

        let long = vec!["1"; 2_000].join("+");
        let expression = Expr::parse(&long).expect("parses flat");
        assert_eq!(
            expression.evaluate(&no_lookup),
            None,
            "past the traversal cap the value stays dynamic"
        );
    }

    #[test]
    fn identifiers_walk_a_long_expression_in_source_order() {
        let source = (0..2_000)
            .map(|index| format!("a{index}"))
            .collect::<Vec<_>>()
            .join("+");

        let expression = Expr::parse(&source).expect("parses");
        let names = expression.identifiers();

        assert_eq!(names.len(), 2_000);
        assert_eq!(names.first(), Some(&"a0"));
        assert_eq!(names.last(), Some(&"a1999"));
    }

    #[test]
    fn dollar_prefixed_names_parse_as_identifiers() {
        let expression = Expr::parse("$getProjectsDirPath()").expect("parses");

        assert!(matches!(expression, Expr::Call(_, _)));
    }
}
