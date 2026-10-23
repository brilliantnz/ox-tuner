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
#[derive(Debug, Clone, PartialEq)]
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
}

/// Prefix operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

impl Expr {
    /// Parses the body of an expression field (without braces).
    pub fn parse(text: &str) -> Result<Self, ExprError> {
        let tokens = tokenize(text)?;
        let mut parser = Parser { tokens, index: 0 };
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
        let value = match self {
            Self::Number(number) => *number,
            Self::Ident(name) => lookup(name)?,
            Self::Unary(operator, operand) => {
                let operand = operand.evaluate(lookup)?;
                match operator {
                    UnaryOp::Negate => -operand,
                    UnaryOp::Plus => operand,
                    UnaryOp::Not => f64::from(operand == 0.0),
                    UnaryOp::BitNot => (!(operand as i64)) as f64,
                }
            }
            Self::Binary(operator, left, right) => {
                let left = left.evaluate(lookup)?;
                let right = right.evaluate(lookup)?;
                apply_binary(*operator, left, right)?
            }
            Self::Ternary(condition, when_true, when_false) => {
                if condition.evaluate(lookup)? != 0.0 {
                    when_true.evaluate(lookup)?
                } else {
                    when_false.evaluate(lookup)?
                }
            }
            Self::Call(name, arguments) => {
                let values: Vec<f64> = arguments
                    .iter()
                    .map(|argument| argument.evaluate(lookup))
                    .collect::<Option<Vec<f64>>>()?;
                evaluate_call(name, &values)?
            }
        };

        value.is_finite().then_some(value)
    }

    /// Every identifier the expression reads, for validation and dependency
    /// tracking.
    pub fn identifiers(&self) -> Vec<&str> {
        let mut found = Vec::new();
        self.collect_identifiers(&mut found);
        found
    }

    fn collect_identifiers<'a>(&'a self, found: &mut Vec<&'a str>) {
        match self {
            Self::Ident(name) => found.push(name),
            Self::Number(_) => {}
            Self::Unary(_, operand) => operand.collect_identifiers(found),
            Self::Binary(_, left, right) => {
                left.collect_identifiers(found);
                right.collect_identifiers(found);
            }
            Self::Ternary(condition, when_true, when_false) => {
                condition.collect_identifiers(found);
                when_true.collect_identifiers(found);
                when_false.collect_identifiers(found);
            }
            Self::Call(_, arguments) => {
                for argument in arguments {
                    argument.collect_identifiers(found);
                }
            }
        }
    }
}

fn apply_binary(operator: BinaryOp, left: f64, right: f64) -> Option<f64> {
    let boolean = |value: bool| Some(f64::from(value));
    match operator {
        BinaryOp::Or => boolean(left != 0.0 || right != 0.0),
        BinaryOp::And => boolean(left != 0.0 && right != 0.0),
        BinaryOp::BitOr => Some(((left as i64) | (right as i64)) as f64),
        BinaryOp::BitXor => Some(((left as i64) ^ (right as i64)) as f64),
        BinaryOp::BitAnd => Some(((left as i64) & (right as i64)) as f64),
        BinaryOp::Equal => boolean(left == right),
        BinaryOp::NotEqual => boolean(left != right),
        BinaryOp::Less => boolean(left < right),
        BinaryOp::LessEqual => boolean(left <= right),
        BinaryOp::Greater => boolean(left > right),
        BinaryOp::GreaterEqual => boolean(left >= right),
        BinaryOp::ShiftLeft => Some(((left as i64) << (right as u32 & 63)) as f64),
        BinaryOp::ShiftRight => Some(((left as i64) >> (right as u32 & 63)) as f64),
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
        _ => return None,
    })
}

struct Parser {
    tokens: Vec<Token>,
    index: usize,
}

impl Parser {
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
            let when_true = self.ternary()?;
            self.expect(":")?;
            let when_false = self.ternary()?;
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
            let operand = self.unary()?;
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
                    return Ok(Expr::Call(name, arguments));
                }
                Ok(Expr::Ident(name))
            }
            Some(Token::Symbol("(")) => {
                self.index += 1;
                let inner = self.ternary()?;
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
        for source in ["1 +", "(", "a ? b", "1 ~ 2", "foo(", "1 2"] {
            let error = Expr::parse(source).expect_err(source);
            assert!(!error.message.is_empty(), "{source}");
        }
    }

    #[test]
    fn dollar_prefixed_names_parse_as_identifiers() {
        let expression = Expr::parse("$getProjectsDirPath()").expect("parses");

        assert!(matches!(expression, Expr::Call(_, _)));
    }
}
