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
use crate::fields::{has_assignment, split_fields, strip_comment};
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
        expander
            .expand(fields)
            .into_iter()
            .map(Cow::into_owned)
            .collect()
    }

    /// [`Preprocessed::expand_fields`], also returning undefined references.
    pub fn expand_reporting(&self, fields: &[String]) -> (Vec<String>, Vec<String>) {
        let expander = self.expander();
        let (expanded, undefined) = expander.expand_reporting(fields);
        (
            expanded.into_iter().map(Cow::into_owned).collect(),
            undefined,
        )
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
        let mut resolved = BTreeMap::new();
        let mut stack = Vec::new();
        for name in defines.keys() {
            stack.clear();
            if let Some(fields) = resolve(defines, &mut resolved, name, &mut stack, 0) {
                resolved.insert(name.clone(), fields);
            }
        }
        Self { resolved }
    }

    /// Expanded fields.
    pub fn expand<'s>(&'s self, fields: &[String]) -> Vec<Cow<'s, str>> {
        self.expand_reporting(fields).0
    }

    /// Expanded fields plus the names that could not be resolved.
    pub fn expand_reporting<'s>(&'s self, fields: &[String]) -> (Vec<Cow<'s, str>>, Vec<String>) {
        let mut undefined = Vec::new();
        let mut expanded = Vec::with_capacity(fields.len());
        let mut index = 0usize;

        while index < fields.len() {
            let field = &fields[index];
            index += 1;

            let Some(name) = field.strip_prefix('$').map(str::trim) else {
                expanded.push(Cow::Owned(field.clone()));
                continue;
            };
            if name.is_empty() || name.contains(char::is_whitespace) {
                expanded.push(Cow::Owned(field.clone()));
                continue;
            }

            match lookup(&self.resolved, name).map(|(_, values)| values) {
                Some(values) => {
                    // Per-index overrides follow the reference:
                    // `$PINLIST, 1="INVALID", 2="STEPPER"`.
                    if fields
                        .get(index)
                        .and_then(|field| parse_index_override(field))
                        .is_some()
                    {
                        let mut values: Vec<Cow<'s, str>> =
                            values.iter().copied().map(Cow::Borrowed).collect();
                        while let Some(override_field) = fields.get(index) {
                            let Some((position, text)) = parse_index_override(override_field)
                            else {
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
                }
                None => {
                    undefined.push(name.to_owned());
                    expanded.push(Cow::Owned(field.clone()));
                }
            }
        }

        (expanded, undefined)
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
) -> Option<Vec<&'a str>> {
    if depth > 32 {
        return None;
    }
    let (key, _) = lookup(defines, name)?;
    if let Some(fields) = resolved.get(key.as_str()) {
        return Some(fields.clone());
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
                fields.extend(resolve(defines, resolved, nested, stack, depth + 1)?);
            }
            _ => fields.push(field.as_str()),
        }
    }
    stack.pop();

    resolved.insert(key.clone(), fields.clone());
    Some(fields)
}

/// The entry a `$name` reference names, with the key it is stored under.
///
/// Define keys are stored lowercase, and references almost always are too, so
/// the common case is a lookup with no allocation at all.
fn lookup<'a, V>(map: &'a BTreeMap<String, V>, name: &str) -> Option<(&'a String, &'a V)> {
    if let Some(entry) = map.get_key_value(name) {
        return Some(entry);
    }
    let lowered = name.trim().to_ascii_lowercase();
    map.get_key_value(lowered.as_str())
}

fn parse_index_override(field: &str) -> Option<(usize, String)> {
    let (index, value) = field.split_once('=')?;
    let position: usize = index.trim().parse().ok()?;
    let value = value.trim();
    if value.len() >= 2 && value.starts_with('"') && value.ends_with('"') {
        Some((position, value.to_owned()))
    } else {
        None
    }
}

/// Resolves an `#include path` to text. Return `None` when the file cannot be
/// found; the caller turns that into a diagnostic instead of failing.
pub type IncludeResolver<'a> = &'a dyn Fn(&str) -> Option<String>;

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
            let key = strip_comment(key).trim();
            let value = strip_comment(value).trim();
            if key.is_empty() {
                String::new()
            } else {
                format!("{key} = {value}")
            }
        }
        None => strip_comment(raw).to_owned(),
    }
}

/// Runs pass 1.
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
    let mut index = 0usize;

    while index < physical.len() {
        let raw = physical[index];
        let line_number = u32::try_from(index + 1).unwrap_or(u32::MAX);
        index += 1;

        // Comments come off first: `;#if CONN_SLOW` in a real rusEFI INI is a
        // commented-out directive, not a directive.
        let cleaned = strip_line_comments(raw);
        let content = cleaned.trim();
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
        let continuation = raw.starts_with([' ', '\t'])
            && !content.starts_with('[')
            && !has_assignment(content)
            && preprocessed
                .lines
                .last()
                .is_some_and(|previous| previous.text.trim_end().ends_with(','));

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
        Some((name, rest)) => (name, rest.trim()),
        None => (directive, ""),
    };
    let name = name.to_ascii_lowercase();
    let enclosing_active = conditionals.iter().all(|frame| frame.active);

    match name.as_str() {
        "if" => {
            let result = evaluate_condition(rest, &mut preprocessed.settings);
            let taken = match result {
                Ok(taken) => taken,
                Err(message) => {
                    preprocessed.diagnostics.push(Diagnostic::new(
                        Severity::Warning,
                        "bad-condition",
                        message,
                        line_number,
                        "Preprocessor",
                    ));
                    false
                }
            };
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
                let taken = result.unwrap_or_else(|message| {
                    preprocessed.diagnostics.push(Diagnostic::new(
                        Severity::Warning,
                        "bad-condition",
                        message,
                        line_number,
                        "Preprocessor",
                    ));
                    false
                });
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
        "endif" => {
            if conditionals.pop().is_none() {
                fatal.push(Diagnostic::new(
                    Severity::Error,
                    "stray-endif",
                    "`#endif` without an open `#if`",
                    line_number,
                    "Preprocessor",
                ));
            }
        }
        "define" => {
            if !enclosing_active {
                return;
            }
            let (define_name, value) = match rest.split_once('=') {
                Some((define_name, value)) => (define_name.trim(), value.trim()),
                None => (rest.trim(), ""),
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
            preprocessed
                .defines
                .insert(define_name.to_ascii_lowercase(), split_fields(value));
        }
        "set" | "unset" => {
            if !enclosing_active {
                return;
            }
            for flag in rest.split_whitespace() {
                preprocessed.settings.declare(flag, name == "set");
            }
        }
        "include" => {
            if !enclosing_active {
                return;
            }
            let path = crate::fields::unquote(rest);
            let resolved = includes.and_then(|resolver| resolver(path));
            match resolved {
                Some(included) => {
                    // Included text is preprocessed in the same context. A
                    // second-level include is deliberately not resolved: the
                    // dialect allows it, but the first implementation keeps the
                    // include graph shallow and reports the rest.
                    if let Ok(nested) = preprocess(&included, &preprocessed.settings, None) {
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
        _ => {
            // rusEFI writes prose comments as `# Digital outputs`. A hash line
            // that names no directive is a comment, not a broken directive.
            preprocessed.hash_comments = preprocessed.hash_comments.saturating_add(1);
        }
    }
}

/// Evaluates a `#if` condition: flag names, `!`, `&&`, `||`, parentheses and
/// the literals `true` / `false`.
fn evaluate_condition(text: &str, settings: &mut SettingsContext) -> Result<bool, String> {
    let tokens = condition_tokens(text)?;
    let mut parser = ConditionParser {
        tokens,
        index: 0,
        settings,
    };
    let value = parser.or_expression()?;
    if parser.index != parser.tokens.len() {
        return Err(format!("unexpected trailing input in condition `{text}`"));
    }
    Ok(value)
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
        let two: String = characters.iter().skip(index).take(2).collect();
        if matches!(two.as_str(), "&&" | "||") {
            tokens.push(two);
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

struct ConditionParser<'a> {
    tokens: Vec<String>,
    index: usize,
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

    fn or_expression(&mut self) -> Result<bool, String> {
        let mut value = self.and_expression()?;
        while self.eat("||") {
            let right = self.and_expression()?;
            value = value || right;
        }
        Ok(value)
    }

    fn and_expression(&mut self) -> Result<bool, String> {
        let mut value = self.unary_expression()?;
        while self.eat("&&") {
            let right = self.unary_expression()?;
            value = value && right;
        }
        Ok(value)
    }

    fn unary_expression(&mut self) -> Result<bool, String> {
        if self.eat("!") {
            return Ok(!self.unary_expression()?);
        }
        if self.eat("(") {
            let value = self.or_expression()?;
            if !self.eat(")") {
                return Err("unbalanced parentheses in condition".to_owned());
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
            None => Err("a condition ends with an operator".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> Preprocessed {
        preprocess(text, &SettingsContext::new(), None).expect("preprocesses")
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
        assert!(output.diagnostics.is_empty());
        assert!(!output.settings.tested_flags().contains("conn_slow"));
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
        let expanded = output.expand_fields(&split_fields(value));

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
            output.expand_fields(&split_fields(value))[4..],
            ["\"A\"", "\"INVALID\"", "\"C\""]
        );
    }

    #[test]
    fn a_define_cycle_is_refused_instead_of_looping() {
        let output = run("[X]\n#define a = $b\n#define b = $a\nx = $a\n");

        let (expanded, unresolved) = output.expand_reporting(&split_fields("$a"));

        assert_eq!(expanded, vec!["$a"], "the reference stays visible");
        assert_eq!(unresolved, vec!["a"], "and is reported");
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
}
