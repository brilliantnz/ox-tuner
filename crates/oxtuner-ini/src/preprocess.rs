//! Pass 1: text → logical lines.
//!
//! One logical line is one entry, after this pass has:
//!
//! - dropped `;` comments (quote-aware),
//! - joined continuation lines (the dialect's multi-line `pageSize`, bins and
//!   `gridOrient` lists),
//! - evaluated `#if` / `#elif` / `#else` / `#endif` against the settings
//!   context,
//! - recorded `#set` / `#unset` defaults and `#define` string lists,
//! - inlined `#include` files when a resolver is supplied.
//!
//! The output keeps 1-based line numbers so every later diagnostic can point at
//! the line the user will find in their editor.

use std::borrow::Cow;
use std::collections::BTreeMap;

use crate::error::{Diagnostic, IniError, Severity};
use crate::expr::bump_depth;
use crate::fields::{assignment_index, has_assignment, split_fields, strip_comment};
use crate::settings::SettingsContext;

/// One entry after preprocessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogicalLine {
    /// 1-based line where the entry started.
    pub line: u32,
    /// The entry with comments removed, continuations joined and whitespace
    /// collapsed at the join points.
    pub text: String,
}

/// Everything pass 1 produced.
#[derive(Debug, Clone, Default)]
pub struct Preprocessed {
    /// Logical lines in document order, conditionals already applied.
    pub lines: Vec<LogicalLine>,
    /// `#define NAME = …` string lists, keyed lowercase.
    pub defines: BTreeMap<String, Vec<String>>,
    /// The settings context after the file's own `#set` / `#unset` declarations
    /// were folded in.
    pub settings: SettingsContext,
    /// Problems that did not stop preprocessing.
    pub diagnostics: Vec<Diagnostic>,
    /// Prose comment lines that begin with `#` (rusEFI writes `# Digital outputs`).
    pub hash_comments: usize,
}

impl Preprocessed {
    /// An expander over this file's defines.
    ///
    /// Building one resolves every define, so a caller that expands more than
    /// a few lines should build it once and reuse it rather than call
    /// [`Preprocessed::expand_fields`] in a loop.
    pub fn expander(&self) -> Expander<'_> {
        Expander::new(&self.defines)
    }

    /// Expands a field list: a field that is exactly `$name` is replaced by that
    /// define's fields.
    pub fn expand_fields(&self, fields: &[String]) -> Vec<String> {
        let expander = self.expander();
        let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
        expander.expand(&fields).iter().map(str::to_owned).collect()
    }

    /// [`Preprocessed::expand_fields`], also returning undefined references.
    pub fn expand_reporting(&self, fields: &[String]) -> (Vec<String>, Vec<String>) {
        let expander = self.expander();
        let fields: Vec<&str> = fields.iter().map(String::as_str).collect();
        let (values, undefined) = expander.expand_reporting(&fields);
        (values.iter().map(str::to_owned).collect(), undefined)
    }
}

/// An expanded field list: literal fields plus an optional borrowed tail.
///
/// A trailing `$define` stays a borrowed slice of the define's resolved fields
/// instead of being copied into the list. Speeduino's CAN address define has
/// 65 536 fields and nineteen entries reference it, so copying it into every
/// entry cost nineteen megabytes of allocation per parse.
#[derive(Debug)]
pub struct Fields<'a> {
    prefix: Vec<Cow<'a, str>>,
    tail: Option<&'a [&'a str]>,
}

impl Fields<'_> {
    /// Number of fields.
    pub fn len(&self) -> usize {
        self.prefix.len() + self.tail.map_or(0, <[&str]>::len)
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// One field by position, borrowed.
    pub fn get(&self, index: usize) -> Option<&str> {
        if index < self.prefix.len() {
            return self.prefix.get(index).map(AsRef::as_ref);
        }
        let offset = index.checked_sub(self.prefix.len())?;
        self.tail?.get(offset).copied()
    }

    /// The fields from `index` on, in order.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.prefix
            .iter()
            .map(AsRef::as_ref)
            .chain(self.tail.unwrap_or_default().iter().copied())
    }
}

#[cfg(test)]
impl Fields<'static> {
    /// A list made only of owned fields, for tests that build the expected
    /// expansion by hand.
    pub fn from_owned(prefix: Vec<Cow<'static, str>>) -> Self {
        Self { prefix, tail: None }
    }
}

/// Expands `#define` string lists inside field lists.
///
/// A field that is exactly `$name` becomes that define's fields, recursively
/// (Speeduino composes `invalid_x16` from two `invalid_x8`s). Per-index
/// overrides (`$PINLIST, 1="INVALID"`) replace one element. A cycle or a
/// missing name is left as written and reported, never looped on.
///
/// Every define is resolved **once**, when the expander is built, and the
/// results borrow from the defines themselves. That matters because a define
/// list can be very large — Speeduino spells out all 65 536 CAN addresses —
/// and dozens of entries reference the same one; expanding on demand would
/// copy that list for every reference.
#[derive(Debug)]
pub struct Expander<'a> {
    /// Each define's fully expanded fields, keyed lowercase.
    resolved: BTreeMap<String, Vec<&'a str>>,
}

impl<'a> Expander<'a> {
    /// An expander over one file's defines.
    ///
    /// Every define is resolved here, and each one only once: a definition can
    /// reference the same list from a dozen entries, and a list can be tens of
    /// thousands of fields wide (Speeduino's CAN address table is). Resolving on
    /// demand and copying the result into every reference made `[Constants]`
    /// dominate the whole parse.
    pub fn new(defines: &'a BTreeMap<String, Vec<String>>) -> Self {
        let mut resolved: BTreeMap<String, Vec<&'a str>> = BTreeMap::new();
        let mut stack = Vec::new();
        for name in defines.keys() {
            stack.clear();
            resolve(defines, &mut resolved, name, &mut stack, 0);
        }
        Self { resolved }
    }

    /// Expanded fields.
    pub fn expand<'s>(&'s self, fields: &'s [&'s str]) -> Fields<'s> {
        self.expand_reporting(fields).0
    }

    /// Expanded fields plus the names that could not be resolved.
    pub fn expand_reporting<'s>(&'s self, fields: &'s [&'s str]) -> (Fields<'s>, Vec<String>) {
        let mut undefined = Vec::new();
        let mut expanded = Vec::with_capacity(fields.len());
        let mut index = 0usize;
        let mut tail: Option<&'s [&'a str]> = None;

        while index < fields.len() {
            let field = fields[index];
            index += 1;

            let Some(name) = field.strip_prefix('$').map(str::trim) else {
                expanded.push(Cow::Borrowed(field));
                continue;
            };
            if name.is_empty() || name.contains(char::is_whitespace) {
                expanded.push(Cow::Borrowed(field));
                continue;
            }

            if let Some(values) = lookup(&self.resolved, name).map(|(_, values)| &**values) {
                let has_override = fields
                    .get(index)
                    .and_then(|field| parse_index_override(field))
                    .is_some();

                // A trailing define with no per-index override stays borrowed.
                if index == fields.len() && !has_override {
                    tail = Some(values);
                    continue;
                }

                // Per-index overrides follow the reference:
                // `$PINLIST, 1="INVALID", 2="STEPPER"`.
                if has_override {
                    let mut values: Vec<Cow<'s, str>> =
                        values.iter().copied().map(Cow::Borrowed).collect();
                    while let Some(override_field) = fields.get(index) {
                        let Some((position, text)) = parse_index_override(override_field) else {
                            break;
                        };
                        if let Some(slot) = values.get_mut(position) {
                            *slot = Cow::Owned(text);
                        }
                        index += 1;
                    }
                    expanded.extend(values);
                } else {
                    expanded.extend(values.iter().copied().map(Cow::Borrowed));
                }
            } else {
                undefined.push(name.to_owned());
                expanded.push(Cow::Borrowed(field));
            }
        }

        (
            Fields {
                prefix: expanded,
                tail,
            },
            undefined,
        )
    }
}

/// Resolves one define and everything it references.
///
/// The returned strings borrow from `defines`, which the caller keeps alive for
/// the whole parse. Results are memoized in `resolved`: a list referenced by
/// many entries is walked once. `stack` holds the names being resolved right
/// now, so a cycle is refused rather than looped on; a define may still be
/// referenced twice by the same entry (`$two, $two`), because each reference is
/// resolved after the previous one has been popped.
fn resolve<'a>(
    defines: &'a BTreeMap<String, Vec<String>>,
    resolved: &mut BTreeMap<String, Vec<&'a str>>,
    name: &str,
    stack: &mut Vec<&'a String>,
    depth: usize,
) -> Option<()> {
    if depth > 32 {
        return None;
    }
    let (key, _) = lookup(defines, name)?;
    if resolved.contains_key(key.as_str()) {
        return Some(());
    }
    if stack.contains(&key) {
        // A cycle in `#define` nesting: refuse rather than loop.
        return None;
    }

    stack.push(key);
    let mut fields = Vec::with_capacity(defines[key.as_str()].len());
    for field in &defines[key.as_str()] {
        match field.strip_prefix('$').map(str::trim) {
            Some(nested) if !nested.is_empty() => {
                resolve(defines, resolved, nested, stack, depth + 1);
                let (nested_key, _) = lookup(defines, nested)?;
                let nested_fields = resolved.get(nested_key.as_str())?;
                fields.extend(nested_fields.iter().copied());
            }
            _ => fields.push(field.as_str()),
        }
    }
    stack.pop();

    resolved.insert(key.clone(), fields);
    Some(())
}

/// The entry a `$name` reference names, with the key it is stored under.
///
/// Define keys are stored lowercase, and references almost always are too, so
/// the common case is a lookup with no allocation at all.
fn lookup<'a, V>(map: &'a BTreeMap<String, V>, name: &str) -> Option<(&'a String, &'a V)> {
    if let Some(entry) = map.get_key_value(name) {
        return Some(entry);
    }
    let lowered = name.trim_ascii().to_ascii_lowercase();
    map.get_key_value(lowered.as_str())
}

fn parse_index_override(field: &str) -> Option<(usize, String)> {
    let (index, value) = field.split_once('=')?;
    let position: usize = index.trim_ascii().parse().ok()?;
    let value = value.trim_ascii();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        Some((position, value.to_owned()))
    } else {
        None
    }
}

/// Resolves an `#include path` to text. Return `None` when the file cannot be
/// found; the caller turns that into a diagnostic instead of failing.
pub type IncludeResolver<'a> = &'a dyn Fn(&str) -> Option<String>;

/// A `/* … */` comment, which may run across physical lines.
///
/// This is a separate scanner rather than one of the `fields` helpers because
/// it rewrites the line and carries state into the next one; the helpers only
/// trim or split a single string.
#[derive(Debug, Default)]
struct BlockComment {
    /// Line where the open comment started, while it is open.
    open: Option<u32>,
}

impl BlockComment {
    /// Strips block comments from one physical line.
    ///
    /// Quoted text is data. A `;` before the assignment is the key-help
    /// separator, so scanning continues; a `;` after the assignment starts a
    /// line comment, and the rest of the line is left for
    /// [`strip_line_comments`]. Each removed comment becomes one space, which
    /// keeps adjacent tokens apart and makes a comment-only line blank.
    fn strip(&mut self, line: &str, line_number: u32) -> String {
        let mut cleaned = String::with_capacity(line.len());
        let mut in_quotes = false;
        let mut escaped = false;
        let mut depth = 0u32;
        let mut first_equals = None;
        let mut equals_ahead = None;
        let mut characters = line.char_indices().peekable();

        while let Some((index, character)) = characters.next() {
            if self.open.is_some() {
                if character == '*' && line[index + 1..].starts_with('/') {
                    self.open = None;
                    characters.next();
                    cleaned.push(' ');
                }
                continue;
            }

            if escaped {
                escaped = false;
                cleaned.push(character);
                continue;
            }

            match character {
                '\\' if in_quotes => {
                    escaped = true;
                    cleaned.push(character);
                }
                '"' => {
                    in_quotes = !in_quotes;
                    cleaned.push(character);
                }
                '{' if !in_quotes => {
                    depth = depth.saturating_add(1);
                    cleaned.push(character);
                }
                '}' if !in_quotes => {
                    depth = depth.saturating_sub(1);
                    cleaned.push(character);
                }
                '=' if !in_quotes && depth == 0 => {
                    first_equals.get_or_insert(index);
                    cleaned.push(character);
                }
                ';' if !in_quotes && depth == 0 => {
                    let comment = first_equals.is_some()
                        || !*equals_ahead
                            .get_or_insert_with(|| assignment_index(&line[index + 1..]).is_some());
                    if comment {
                        cleaned.push_str(&line[index..]);
                        break;
                    }
                    cleaned.push(character);
                }
                '/' if !in_quotes && line[index + 1..].starts_with('*') => {
                    self.open = Some(line_number);
                    characters.next();
                    cleaned.push(' ');
                }
                _ => cleaned.push(character),
            }
        }

        cleaned
    }
}

/// Removes comments from one physical line without losing a `key;help = value`
/// entry.
///
/// The dialect puts field help between the key and the `=`:
/// `injectionMode;+"Select the injection mode" = bits, …`. Stripping at the
/// first `;` would delete the whole entry, so the line is split at its
/// assignment first and each half is cleaned separately.
fn strip_line_comments(raw: &str) -> String {
    match crate::fields::split_assignment(raw) {
        Some((key, value)) => {
            let key = strip_comment(key).trim_ascii();
            let value = strip_comment(value).trim_ascii();
            if key.is_empty() {
                String::new()
            } else {
                // `format!` was measurably slower here: this runs once per
                // physical line of every definition.
                let mut joined = String::with_capacity(key.len() + value.len() + 3);
                joined.push_str(key);
                joined.push_str(" = ");
                joined.push_str(value);
                joined
            }
        }
        None => strip_comment(raw).to_owned(),
    }
}

/// Runs pass 1.
///
/// # Errors
///
/// Returns [`IniError`] when a conditional block is left open or closed
/// without a matching `#if`.
pub fn preprocess(
    text: &str,
    settings: &SettingsContext,
    includes: Option<IncludeResolver<'_>>,
) -> Result<Preprocessed, IniError> {
    let mut preprocessed = Preprocessed {
        settings: settings.clone(),
        ..Preprocessed::default()
    };
    let mut conditionals: Vec<Conditional> = Vec::new();
    let mut fatal = Vec::new();

    let physical: Vec<&str> = text.lines().collect();
    let has_block_comments = text.contains("/*");
    let mut block_comment = BlockComment::default();
    let mut index = 0usize;

    while index < physical.len() {
        let raw = physical[index];
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        index += 1;

        // Block comments go first because they can span lines. A file without
        // the marker skips the extra scan entirely, so the common path keeps
        // its old cost.
        let uncommented = if has_block_comments {
            // A line that starts as a `;` or `#` comment is prose, not a place
            // where `/*` can open a block, whatever the prose says.
            let starts_line_comment = raw.trim_ascii_start().starts_with([';', '#']);
            if block_comment.open.is_some() || (raw.contains("/*") && !starts_line_comment) {
                Cow::Owned(block_comment.strip(raw, line_number))
            } else {
                Cow::Borrowed(raw)
            }
        } else {
            Cow::Borrowed(raw)
        };

        // Comments come off next: `;#if CONN_SLOW` in a real rusEFI INI is a
        // commented-out directive, not a directive.
        let cleaned = strip_line_comments(&uncommented);
        let content = cleaned.trim_ascii();
        if content.is_empty() {
            continue;
        }

        if let Some(directive) = content.strip_prefix('#') {
            handle_directive(
                directive,
                line_number,
                &mut preprocessed,
                &mut conditionals,
                includes,
                &mut fatal,
            );
            continue;
        }

        if !conditionals.iter().all(|frame| frame.active) {
            continue;
        }

        // A continuation is an indented line with no assignment that continues
        // the previous value's comma-separated list. Requiring the trailing
        // comma matters: bare keywords such as `std_separator` are indented and
        // carry no `=`, and must stay their own logical line.
        let continuation = uncommented.starts_with([' ', '\t'])
            && !content.starts_with('[')
            && !has_assignment(content)
            && preprocessed
                .lines
                .last()
                .is_some_and(|previous| previous.text.trim_ascii_end().ends_with(','));

        if continuation {
            if let Some(previous) = preprocessed.lines.last_mut() {
                previous.text.push_str(content);
            }
            continue;
        }

        preprocessed.lines.push(LogicalLine {
            line: line_number,
            text: content.to_owned(),
        });
    }

    if let Some(line) = block_comment.open {
        preprocessed.diagnostics.push(Diagnostic::new(
            Severity::Warning,
            "unterminated-block-comment",
            "this `/*` comment has no matching `*/`; the rest of the file is ignored",
            line,
            "Preprocessor",
        ));
    }

    if preprocessed.hash_comments > 0 {
        preprocessed.diagnostics.push(Diagnostic::whole_file(
            Severity::Note,
            "hash-comments",
            format!(
                "{} `#` line(s) named no directive and were read as comments",
                preprocessed.hash_comments
            ),
        ));
    }

    if let Some(frame) = conditionals
        .iter()
        .rev()
        .find(|frame| frame.depth_line.is_some())
        && let Some(line) = frame.depth_line
    {
        fatal.push(Diagnostic::new(
            Severity::Error,
            "unterminated-if",
            "this `#if` has no matching `#endif`",
            line,
            "Preprocessor",
        ));
    }

    if fatal.is_empty() {
        Ok(preprocessed)
    } else {
        // The truncation explains why later entries are missing, so keep it
        // beside the structural failure instead of discarding it with the rest.
        fatal.extend(
            preprocessed
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code.as_ref() == "unterminated-block-comment")
                .cloned(),
        );
        Err(IniError::new(fatal))
    }
}

/// One open `#if` … `#endif` block.
struct Conditional {
    /// Whether the enclosing block is active.
    parent_active: bool,
    /// Whether this branch is taken.
    branch_taken: bool,
    /// Whether this branch is being emitted.
    active: bool,
    /// Line of the `#if` that opened the block.
    depth_line: Option<u32>,
}

fn handle_directive(
    directive: &str,
    line_number: u32,
    preprocessed: &mut Preprocessed,
    conditionals: &mut Vec<Conditional>,
    includes: Option<IncludeResolver<'_>>,
    fatal: &mut Vec<Diagnostic>,
) {
    let (name, rest) = match directive.split_once(char::is_whitespace) {
        Some((name, rest)) => (name, rest.trim_ascii()),
        None => (directive, ""),
    };
    let name = name.to_ascii_lowercase();

    match name.as_str() {
        "if" | "elif" | "else" | "endif" => handle_conditional_directive(
            &name,
            rest,
            line_number,
            preprocessed,
            conditionals,
            fatal,
        ),
        "define" | "set" | "unset" | "include" => {
            if conditionals.iter().all(|frame| frame.active) {
                handle_active_directive(&name, rest, line_number, preprocessed, includes);
            }
        }
        _ => {
            // rusEFI writes prose comments as `# Digital outputs`. A hash line
            // that names no directive is a comment, not a broken directive.
            preprocessed.hash_comments = preprocessed.hash_comments.saturating_add(1);
        }
    }
}

fn handle_conditional_directive(
    name: &str,
    rest: &str,
    line_number: u32,
    preprocessed: &mut Preprocessed,
    conditionals: &mut Vec<Conditional>,
    fatal: &mut Vec<Diagnostic>,
) {
    let enclosing_active = conditionals.iter().all(|frame| frame.active);

    match name {
        "if" => {
            let result = evaluate_condition(rest, &mut preprocessed.settings);
            let taken = resolve_condition(result, preprocessed, fatal, line_number);
            let taken = taken && enclosing_active;
            conditionals.push(Conditional {
                parent_active: enclosing_active,
                branch_taken: taken,
                active: taken,
                depth_line: Some(line_number),
            });
        }
        "elif" => match conditionals.last_mut() {
            Some(frame) => {
                let result = evaluate_condition(rest, &mut preprocessed.settings);
                let taken = resolve_condition(result, preprocessed, fatal, line_number);
                let taken = taken && !frame.branch_taken && frame.parent_active;
                frame.active = taken;
                frame.branch_taken |= taken;
            }
            None => fatal.push(Diagnostic::new(
                Severity::Error,
                "stray-elif",
                "`#elif` without an open `#if`",
                line_number,
                "Preprocessor",
            )),
        },
        "else" => match conditionals.last_mut() {
            Some(frame) => {
                let taken = !frame.branch_taken && frame.parent_active;
                frame.active = taken;
                frame.branch_taken |= taken;
            }
            None => fatal.push(Diagnostic::new(
                Severity::Error,
                "stray-else",
                "`#else` without an open `#if`",
                line_number,
                "Preprocessor",
            )),
        },
        "endif" if conditionals.pop().is_none() => {
            fatal.push(Diagnostic::new(
                Severity::Error,
                "stray-endif",
                "`#endif` without an open `#if`",
                line_number,
                "Preprocessor",
            ));
        }
        _ => {}
    }
}

fn handle_active_directive(
    name: &str,
    rest: &str,
    line_number: u32,
    preprocessed: &mut Preprocessed,
    includes: Option<IncludeResolver<'_>>,
) {
    match name {
        "define" => {
            let (define_name, value) = match rest.split_once('=') {
                Some((define_name, value)) => (define_name.trim_ascii(), value.trim_ascii()),
                None => (rest.trim_ascii(), ""),
            };
            if define_name.is_empty() {
                preprocessed.diagnostics.push(Diagnostic::new(
                    Severity::Warning,
                    "empty-define",
                    "`#define` without a name",
                    line_number,
                    "Preprocessor",
                ));
                return;
            }
            preprocessed.defines.insert(
                define_name.to_ascii_lowercase(),
                split_fields(value).into_iter().map(str::to_owned).collect(),
            );
        }
        "set" | "unset" => {
            for flag in rest.split_whitespace() {
                preprocessed.settings.declare(flag, name == "set");
            }
        }
        "include" => {
            let path = crate::fields::unquote(rest);
            let resolved = includes.and_then(|resolver| resolver(path));
            match resolved {
                Some(nested_text) => {
                    // Included text is preprocessed in the same context. A
                    // second-level include is deliberately not resolved: the
                    // dialect allows it, but the first implementation keeps the
                    // include graph shallow and reports the rest.
                    if let Ok(nested) = preprocess(&nested_text, &preprocessed.settings, None) {
                        preprocessed.lines.extend(nested.lines);
                        for (key, values) in nested.defines {
                            preprocessed.defines.entry(key).or_insert(values);
                        }
                        preprocessed.diagnostics.extend(nested.diagnostics);
                        preprocessed.settings = nested.settings;
                    }
                }
                None => preprocessed.diagnostics.push(Diagnostic::new(
                    Severity::Note,
                    "include-not-found",
                    format!("could not resolve `#include {path}`; the entry is ignored"),
                    line_number,
                    "Preprocessor",
                )),
            }
        }
        _ => {}
    }
}

/// Evaluates a `#if` condition: flag names, `!`, `&&`, `||`, parentheses and
/// the literals `true` / `false`.
fn evaluate_condition(text: &str, settings: &mut SettingsContext) -> Result<bool, ConditionError> {
    let tokens = condition_tokens(text).map_err(ConditionError::Syntax)?;
    let mut parser = ConditionParser {
        tokens,
        index: 0,
        depth: 0,
        settings,
    };
    let value = parser.or_expression()?;
    if parser.index != parser.tokens.len() {
        return Err(ConditionError::Syntax(format!(
            "unexpected trailing input in condition `{text}`"
        )));
    }
    Ok(value)
}

/// Why a `#if` or `#elif` condition could not be evaluated.
enum ConditionError {
    /// The condition is malformed. The branch is skipped with a warning.
    Syntax(String),
    /// The condition nests past [`MAX_CONDITION_NESTING`]. Fatal, unlike a syntax error,
    /// because it is the signature of hostile or corrupt input rather than a
    /// typo a human can fix on the reported line.
    TooDeep,
}

/// Reports a failed condition and returns the branch decision.
fn resolve_condition(
    result: Result<bool, ConditionError>,
    preprocessed: &mut Preprocessed,
    fatal: &mut Vec<Diagnostic>,
    line_number: u32,
) -> bool {
    match result {
        Ok(taken) => taken,
        Err(ConditionError::TooDeep) => {
            fatal.push(Diagnostic::new(
                Severity::Error,
                "condition-too-deep",
                format!("a condition nests too deeply (limit {MAX_CONDITION_NESTING})"),
                line_number,
                "Preprocessor",
            ));
            false
        }
        Err(ConditionError::Syntax(message)) => {
            preprocessed.diagnostics.push(Diagnostic::new(
                Severity::Warning,
                "bad-condition",
                message,
                line_number,
                "Preprocessor",
            ));
            false
        }
    }
}

fn condition_tokens(text: &str) -> Result<Vec<String>, String> {
    let mut tokens = Vec::new();
    let characters: Vec<char> = text.chars().collect();
    let mut index = 0usize;

    while index < characters.len() {
        let character = characters[index];
        if character.is_whitespace() {
            index += 1;
            continue;
        }
        if character.is_alphanumeric() || character == '_' {
            let start = index;
            while index < characters.len()
                && (characters[index].is_alphanumeric() || characters[index] == '_')
            {
                index += 1;
            }
            tokens.push(characters[start..index].iter().collect());
            continue;
        }
        if let Some(next) = characters.get(index + 1)
            && matches!((character, *next), ('&', '&') | ('|', '|'))
        {
            tokens.push(characters[index..index + 2].iter().collect());
            index += 2;
            continue;
        }
        if matches!(character, '!' | '(' | ')') {
            tokens.push(character.to_string());
            index += 1;
            continue;
        }
        return Err(format!(
            "unexpected character {character:?} in condition `{text}`"
        ));
    }

    Ok(tokens)
}

/// How deeply parentheses and `!` may nest in a `#if` condition.
///
/// Independent of the expression parser's cap: this grammar is much smaller,
/// so the two limits do not need to move together.
const MAX_CONDITION_NESTING: u32 = 128;

struct ConditionParser<'a> {
    tokens: Vec<String>,
    index: usize,
    depth: u32,
    settings: &'a mut SettingsContext,
}

impl ConditionParser<'_> {
    fn peek(&self) -> Option<&str> {
        self.tokens.get(self.index).map(String::as_str)
    }

    fn eat(&mut self, token: &str) -> bool {
        if self.peek() == Some(token) {
            self.index += 1;
            return true;
        }
        false
    }

    fn or_expression(&mut self) -> Result<bool, ConditionError> {
        let mut value = self.and_expression()?;
        while self.eat("||") {
            let right = self.and_expression()?;
            value = value || right;
        }
        Ok(value)
    }

    fn and_expression(&mut self) -> Result<bool, ConditionError> {
        let mut value = self.unary_expression()?;
        while self.eat("&&") {
            let right = self.unary_expression()?;
            value = value && right;
        }
        Ok(value)
    }

    fn enter(&mut self) -> Result<(), ConditionError> {
        if !bump_depth(&mut self.depth, MAX_CONDITION_NESTING) {
            return Err(ConditionError::TooDeep);
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn unary_expression(&mut self) -> Result<bool, ConditionError> {
        if self.eat("!") {
            self.enter()?;
            let value = !self.unary_expression()?;
            self.leave();

            return Ok(value);
        }
        if self.eat("(") {
            self.enter()?;
            let value = self.or_expression()?;
            self.leave();

            if !self.eat(")") {
                return Err(ConditionError::Syntax(
                    "unbalanced parentheses in condition".to_owned(),
                ));
            }

            return Ok(value);
        }
        match self.peek() {
            Some(token) => {
                let token = token.to_owned();
                self.index += 1;
                match token.to_ascii_lowercase().as_str() {
                    "true" => Ok(true),
                    "false" => Ok(false),
                    _ => {
                        self.settings.note_tested(&token);
                        Ok(self.settings.is_active(&token))
                    }
                }
            }
            None => Err(ConditionError::Syntax(
                "a condition ends with an operator".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Preprocessed {
        preprocess(text, &SettingsContext::new(), None).expect("preprocesses")
    }

    fn nesting_cap() -> usize {
        usize::try_from(MAX_CONDITION_NESTING).expect("the nesting cap fits in usize")
    }

    #[test]
    fn continuations_are_joined_and_keep_the_first_line_number() {
        let output = run("[Constants]\n   pageSize = 128, 288,\n    192, 256\n   x = scalar\n");

        assert_eq!(output.lines.len(), 3);
        assert_eq!(output.lines[1].text, "pageSize = 128, 288,192, 256");
        assert_eq!(output.lines[1].line, 2);
    }

    #[test]
    fn an_indented_section_header_is_never_joined_to_the_previous_entry() {
        let output = run("a = 1\n   [Constants]\n");

        assert_eq!(output.lines.len(), 2);
        assert_eq!(output.lines[1].text, "[Constants]");
    }

    #[test]
    fn an_indented_bare_keyword_is_not_joined_to_a_complete_entry() {
        // `std_separator` is indented and has no `=`; it is an entry of its own.
        let output = run("[Menu]\n   subMenu = a, \"A\"\n   std_separator\n");

        assert_eq!(output.lines.len(), 3);
        assert_eq!(output.lines[2].text, "std_separator");
    }

    #[test]
    fn a_commented_out_directive_is_text_not_a_directive() {
        // rusEFI ships `;#if CONN_SLOW` lines.
        let output = run("[Constants]\n   value = 1\n   ;#if CONN_SLOW\n");

        assert_eq!(output.lines.len(), 2);
        assert!(
            output.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            output.diagnostics
        );
        assert!(!output.settings.tested_flags().contains("conn_slow"));
    }

    #[test]
    fn a_block_comment_is_removed_across_lines_and_quotes_keep_their_markers() {
        let output = run(
            "[Constants]\nname = \"keep /* this */\"\n/* dead = 1,\n   still dead */\nlive = 2\n",
        );

        assert_eq!(output.lines.len(), 3);
        assert_eq!(output.lines[1].text, "name = \"keep /* this */\"");
        assert_eq!(output.lines[2].text, "live = 2");
        assert!(
            output.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn an_unterminated_block_comment_drops_the_rest_and_warns() {
        let output = run("a = 1\n/* dead\nb = 2\n");

        assert_eq!(output.lines.len(), 1);
        assert_eq!(output.lines[0].text, "a = 1");
        assert!(
            output.diagnostics.iter().any(|diagnostic| {
                diagnostic.severity == Severity::Warning
                    && diagnostic.code.as_ref() == "unterminated-block-comment"
                    && diagnostic.line == Some(2)
            }),
            "unexpected diagnostics: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn a_block_marker_inside_a_line_comment_is_text() {
        let output = run("a = 1 ; /* not an opening\nb = 2\n");

        assert_eq!(output.lines.len(), 2);
        assert_eq!(output.lines[1].text, "b = 2");
        assert!(
            output.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn a_comment_line_whose_prose_contains_an_equals_does_not_open_a_block() {
        for comment in ["; see foo=bar /* details", "# note /* details"] {
            let text = format!("[Constants]\na = 1\n{comment}\nb = 2\n");

            let output = run(&text);

            assert_eq!(output.lines.len(), 3, "{comment}");
            assert_eq!(output.lines[2].text, "b = 2", "{comment}");
            assert!(
                !output
                    .diagnostics
                    .iter()
                    .any(|diagnostic| { diagnostic.code.as_ref() == "unterminated-block-comment" }),
                "{comment}: {:?}",
                output.diagnostics
            );
        }
    }

    #[test]
    fn an_unterminated_block_comment_survives_a_fatal_condition() {
        let error = preprocess(
            "a = 1\n#if X\nb = 2\n/* dead\n",
            &SettingsContext::new(),
            None,
        )
        .expect_err("the #if is unterminated");

        assert!(
            error
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code.as_ref() == "unterminated-block-comment"),
            "the truncation warning is lost: {error}"
        );
    }

    #[test]
    fn a_block_comment_after_a_key_help_semicolon_is_removed() {
        let output = run("key;\"help\" = scalar /* note */\n");

        assert_eq!(output.lines[0].text, "key = scalar");
        assert!(
            output.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn a_key_help_semicolon_after_a_mid_line_block_comment_is_kept() {
        let output = run("/* note */ key;\"help\" = scalar\n");

        assert_eq!(output.lines[0].text, "key = scalar");
        assert!(
            output.diagnostics.is_empty(),
            "unexpected diagnostics: {:?}",
            output.diagnostics
        );
    }

    #[test]
    fn the_first_branch_of_an_if_is_taken_only_when_the_flag_is_set() {
        let text = "[Constants]\n#if CELSIUS\ntemp = \"C\"\n#else\ntemp = \"F\"\n#endif\n";

        let without = preprocess(text, &SettingsContext::new(), None).expect("parses");
        assert_eq!(without.lines[1].text, r#"temp = "F""#);

        let with = preprocess(
            text,
            &SettingsContext::new().with_overrides([("CELSIUS", true)]),
            None,
        )
        .expect("parses");
        assert_eq!(with.lines[1].text, r#"temp = "C""#);
    }

    #[test]
    fn elif_chooses_the_first_matching_branch() {
        let text = "[X]\n#if mcu_teensy\na = teensy\n#elif mcu_stm32\na = stm32\n#else\na = mega\n#endif\n";

        let stm32 = preprocess(
            text,
            &SettingsContext::new().with_overrides([("mcu_stm32", true)]),
            None,
        )
        .expect("parses");
        assert_eq!(stm32.lines[1].text, "a = stm32");

        let neither = preprocess(text, &SettingsContext::new(), None).expect("parses");
        assert_eq!(neither.lines[1].text, "a = mega");
    }

    #[test]
    fn nested_conditionals_respect_the_enclosing_branch() {
        let text = "[X]\n#if A\n#if B\ndeep = 1\n#endif\n#endif\n";

        let only_a = preprocess(
            text,
            &SettingsContext::new().with_overrides([("A", true)]),
            None,
        )
        .expect("parses");
        assert!(only_a.lines.iter().all(|line| line.text != "deep = 1"));
    }

    #[test]
    fn defines_are_recorded_and_expanded_recursively() {
        let output = run(
            "[Constants]\n#define two = \"a\", \"b\"\n#define four = $two, $two\nx = bits, U08, 0, [0:3], $four\n",
        );

        let (_, value) =
            crate::fields::split_assignment(&output.lines[1].text).expect("assignment");
        let value = split_fields(value)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let expanded = output.expand_fields(&value);

        assert_eq!(
            expanded,
            vec![
                "bits", "U08", "0", "[0:3]", "\"a\"", "\"b\"", "\"a\"", "\"b\""
            ]
        );
    }

    #[test]
    fn a_per_index_override_replaces_one_define_element() {
        let output = run(
            "[Constants]\n#define pins = \"A\", \"B\", \"C\"\nx = bits, U08, 0, [0:2], $pins, 1=\"INVALID\"\n",
        );

        let (_, value) =
            crate::fields::split_assignment(&output.lines[1].text).expect("assignment");

        assert_eq!(
            {
                let value = split_fields(value)
                    .into_iter()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                output.expand_fields(&value)
            }[4..],
            ["\"A\"", "\"INVALID\"", "\"C\""]
        );
    }

    #[test]
    fn a_define_cycle_is_refused_instead_of_looping() {
        let output = run("[X]\n#define a = $b\n#define b = $a\nx = $a\n");

        let value = split_fields("$a")
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let (expanded, unresolved) = output.expand_reporting(&value);

        assert_eq!(expanded, vec!["$a"], "the reference stays visible");
        assert_eq!(unresolved, vec!["a"], "and is reported");
    }

    #[test]
    fn a_deeply_nested_condition_is_fatal() {
        let condition = format!("{}FLAG{}", "(".repeat(10_000), ")".repeat(10_000));
        let text = format!("[X]\n#if {condition}\n#endif\n");

        let error = preprocess(&text, &SettingsContext::new(), None).expect_err("must be fatal");

        assert_eq!(error.diagnostics()[0].code, "condition-too-deep");
        assert_eq!(error.diagnostics()[0].line, Some(2));
    }

    #[test]
    fn condition_tokens_split_two_character_operators() {
        assert_eq!(
            condition_tokens("a && b || !c").expect("tokens"),
            vec!["a", "&&", "b", "||", "!", "c"]
        );
        assert!(condition_tokens("a &").is_err());
    }

    #[test]
    fn a_condition_at_the_nesting_cap_evaluates() {
        let nested = format!(
            "{}true{}",
            "(".repeat(nesting_cap()),
            ")".repeat(nesting_cap())
        );

        let output = run(&format!("[X]\n#if {nested}\nvalue = 1\n#endif\n"));

        assert!(output.lines.iter().any(|line| line.text == "value = 1"));
    }

    #[test]
    fn one_past_the_condition_nesting_cap_is_fatal() {
        let nested = format!(
            "{}true{}",
            "(".repeat(nesting_cap() + 1),
            ")".repeat(nesting_cap() + 1)
        );
        let text = format!("[X]\n#if {nested}\n#endif\n");

        let error = preprocess(&text, &SettingsContext::new(), None).expect_err("must be fatal");

        assert_eq!(error.diagnostics()[0].code, "condition-too-deep");
    }

    #[test]
    fn condition_operands_do_not_accumulate_depth() {
        let branch = format!("{}FLAG{}", "(".repeat(60), ")".repeat(60));

        let output = run(&format!(
            "[X]\n#if {branch} || {branch}\nvalue = 1\n#endif\n"
        ));

        assert_eq!(
            output.lines.len(),
            1,
            "the header stays, the branch is skipped"
        );
    }

    #[test]
    fn a_long_negation_chain_is_fatal() {
        let condition = "!".repeat(10_000);
        let text = format!("[X]\n#if {condition} FLAG\n#endif\n");

        let error = preprocess(&text, &SettingsContext::new(), None).expect_err("must be fatal");

        assert_eq!(error.diagnostics()[0].code, "condition-too-deep");
    }

    #[test]
    fn a_deeply_nested_elif_condition_is_fatal() {
        let nested = format!("{}FLAG{}", "(".repeat(10_000), ")".repeat(10_000));
        let text = format!("[X]\n#if true\n#elif {nested}\n#endif\n");

        let error = preprocess(&text, &SettingsContext::new(), None).expect_err("must be fatal");

        assert_eq!(error.diagnostics()[0].code, "condition-too-deep");
    }

    #[test]
    fn unbalanced_conditionals_are_fatal_with_a_line_number() {
        let error =
            preprocess("[X]\n#if A\n", &SettingsContext::new(), None).expect_err("must be fatal");

        assert_eq!(error.diagnostics()[0].code, "unterminated-if");
        assert_eq!(error.diagnostics()[0].line, Some(2));
    }

    #[test]
    fn a_stray_endif_is_fatal() {
        let error =
            preprocess("[X]\n#endif\n", &SettingsContext::new(), None).expect_err("must be fatal");

        assert_eq!(error.diagnostics()[0].code, "stray-endif");
    }

    #[test]
    fn an_include_is_inlined_when_the_resolver_finds_it() {
        let resolver = |path: &str| (path == "extra.inc").then(|| "included = 1\n".to_owned());

        let output = preprocess(
            "[X]\na = 1\n#include extra.inc\n",
            &SettingsContext::new(),
            Some(&resolver),
        )
        .expect("preprocesses");

        assert_eq!(output.lines.len(), 3);
        assert_eq!(output.lines[2].text, "included = 1");
    }

    #[test]
    fn a_missing_include_is_reported_and_never_fatal() {
        let resolver = |_: &str| None;

        let output = preprocess(
            "[X]\na = 1\n#include nope.inc\n",
            &SettingsContext::new(),
            Some(&resolver),
        )
        .expect("preprocesses");

        assert_eq!(output.lines.len(), 2);
        assert_eq!(output.diagnostics[0].code, "include-not-found");
    }

    #[test]
    fn a_hash_line_that_names_no_directive_is_a_comment() {
        // rusEFI writes `# Digital outputs` as prose; treating it as a broken
        // directive would reject a working definition.
        let output = run("[X]\n# Digital outputs\na = 1\n");

        assert_eq!(output.lines.len(), 2);
        assert_eq!(output.hash_comments, 1);
        assert!(
            output
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "hash-comments")
        );
    }

    #[test]
    fn a_trailing_define_is_borrowed_but_reads_exactly_like_the_expansion() {
        // The whole option list of a `bits` entry is one `$define`. It stays
        // borrowed, and the reader must see the same fields in the same order.
        let output = run("[X]\n#define big = \"a\", \"b\", \"c\"\nx = bits, U08, 0, [0:2], $big\n");

        let (_, value) =
            crate::fields::split_assignment(&output.lines[1].text).expect("assignment");
        let raw = split_fields(value);
        let expander = output.expander();
        let fields = expander.expand(&raw);

        assert_eq!(fields.len(), 7);
        assert_eq!(
            fields.iter().collect::<Vec<_>>(),
            ["bits", "U08", "0", "[0:2]", "\"a\"", "\"b\"", "\"c\""]
        );
    }

    #[test]
    fn a_define_in_the_middle_is_still_spliced_inline() {
        // Only a *trailing* define is borrowed; one before another field has to
        // be expanded in place.
        let output = run("[X]\n#define two = \"a\", \"b\"\nx = bits, $two, \"z\"\n");

        let (_, value) =
            crate::fields::split_assignment(&output.lines[1].text).expect("assignment");
        let raw = split_fields(value);
        let expander = output.expander();
        let fields = expander.expand(&raw);

        assert_eq!(
            fields.iter().collect::<Vec<_>>(),
            ["bits", "\"a\"", "\"b\"", "\"z\""]
        );
    }

    #[test]
    fn a_trailing_define_with_a_per_index_override_is_expanded() {
        // The override rewrites one element, so the list cannot stay borrowed.
        let output = run("[X]\n#define two = \"a\", \"b\"\nx = bits, $two, 1=\"B\"\n");

        let (_, value) =
            crate::fields::split_assignment(&output.lines[1].text).expect("assignment");
        let raw = split_fields(value);
        let expander = output.expander();
        let fields = expander.expand(&raw);

        assert_eq!(
            fields.iter().collect::<Vec<_>>(),
            ["bits", "\"a\"", "\"B\""]
        );
    }
}
