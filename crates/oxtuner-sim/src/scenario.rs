//! Scenario files: a session described in text, without code.
//!
//! The format is deliberately small and dependency-free. One directive per
//! line, `#` starts a comment:
//!
//! ```text
//! # a bench regression
//! seed = 7
//! signature = speeduino broken
//! page = 1 : 00 01 02 03
//! fault = read-page:truncate:32
//! fault = burn:drop x2
//! fault = signature:corrupt:0
//! fault = any:delay:100
//! ```
//!
//! Faults are `operation:fault[:argument]`, where the argument is a byte count
//! (`truncate`, `partial-write`), a byte index (`corrupt`) or milliseconds
//! (`delay`). A trailing `xN` fires the fault `N` times; without it, once.

use std::time::Duration;

use crate::faults::{Fault, FaultRule, SimOp};

/// A parsed scenario.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scenario {
    /// Seed for the memory fill and the waveform phase.
    pub seed: Option<u64>,
    /// Signature the virtual ECU answers with, when it should differ from the
    /// definition's.
    pub signature: Option<String>,
    /// Pages preloaded with bytes.
    pub pages: Vec<(u8, Vec<u8>)>,
    /// Armed faults.
    pub faults: Vec<FaultRule>,
}

/// Why a scenario line could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScenarioError {
    /// One-based line number.
    pub line: usize,
    /// What was wrong.
    pub message: String,
}

impl std::fmt::Display for ScenarioError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "scenario line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ScenarioError {}

impl Scenario {
    /// Parses the line format.
    pub fn parse(text: &str) -> Result<Self, ScenarioError> {
        let mut scenario = Scenario::default();

        for (index, line) in text.lines().enumerate() {
            let number = index + 1;
            let line = line.split('#').next().unwrap_or("").trim();
            if line.is_empty() {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| ScenarioError {
                line: number,
                message: "expected `key = value`".to_owned(),
            })?;
            let key = key.trim();
            let value = value.trim();

            match key {
                "seed" => {
                    scenario.seed = Some(value.parse().map_err(|_| ScenarioError {
                        line: number,
                        message: format!("{value:?} is not a seed"),
                    })?);
                }
                "signature" => scenario.signature = Some(value.to_owned()),
                "page" => scenario.pages.push(parse_page(number, value)?),
                "fault" => scenario.faults.push(parse_fault(number, value)?),
                other => {
                    return Err(ScenarioError {
                        line: number,
                        message: format!("unknown directive {other:?}"),
                    });
                }
            }
        }

        Ok(scenario)
    }

    /// Loads a scenario from disk.
    pub fn load(path: &std::path::Path) -> Result<Self, ScenarioError> {
        let text = std::fs::read_to_string(path).map_err(|error| ScenarioError {
            line: 0,
            message: format!("{}: {error}", path.display()),
        })?;
        Self::parse(&text)
    }
}

/// `page = 1 : 00 01 02`
fn parse_page(line: usize, value: &str) -> Result<(u8, Vec<u8>), ScenarioError> {
    let (page, bytes) = value.split_once(':').ok_or_else(|| ScenarioError {
        line,
        message: "expected `page = <number> : <hex bytes>`".to_owned(),
    })?;
    let page = page.trim().parse::<u8>().map_err(|_| ScenarioError {
        line,
        message: format!("{:?} is not a page number", page.trim()),
    })?;
    let mut data = Vec::new();
    for field in bytes.split_whitespace() {
        let byte = u8::from_str_radix(field, 16).map_err(|_| ScenarioError {
            line,
            message: format!("{field:?} is not a hex byte"),
        })?;
        data.push(byte);
    }
    Ok((page, data))
}

/// `fault = read-page:truncate:32 x2`
fn parse_fault(line: usize, value: &str) -> Result<FaultRule, ScenarioError> {
    let (body, times) = match value.rsplit_once(" x") {
        Some((body, times)) => {
            let times = times.parse::<u32>().map_err(|_| ScenarioError {
                line,
                message: format!("{times:?} is not a repeat count"),
            })?;
            (body.trim(), times)
        }
        None => (value, 1),
    };

    let mut fields = body.split(':');
    let operation = fields.next().unwrap_or_default();
    let fault = fields.next().ok_or_else(|| ScenarioError {
        line,
        message: "expected `operation:fault[:argument]`".to_owned(),
    })?;
    let argument = fields.next();
    if fields.next().is_some() {
        return Err(ScenarioError {
            line,
            message: "too many fields for a fault".to_owned(),
        });
    }

    let operation = SimOp::parse(operation).ok_or_else(|| ScenarioError {
        line,
        message: format!("{operation:?} is not an operation"),
    })?;
    let number = || -> Result<usize, ScenarioError> {
        argument
            .and_then(|argument| argument.trim().parse().ok())
            .ok_or_else(|| ScenarioError {
                line,
                message: format!("the `{fault}` fault needs a number"),
            })
    };
    let fault = match fault.trim().to_ascii_lowercase().as_str() {
        "drop" => Fault::DropReply,
        "truncate" => Fault::Truncate { keep: number()? },
        "corrupt" => Fault::Corrupt { at: number()? },
        "delay" => Fault::Delay {
            delay: Duration::from_millis(u64::try_from(number()?).unwrap_or(0)),
        },
        "disconnect" => Fault::Disconnect,
        "refuse-burn" => Fault::RefuseBurn,
        "bad-crc" => Fault::BadCrc,
        "partial-write" => Fault::PartialWrite { bytes: number()? },
        other => {
            return Err(ScenarioError {
                line,
                message: format!("unknown fault {other:?}"),
            });
        }
    };

    Ok(FaultRule::new(operation, fault, times))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scenario_reads_every_directive() {
        let scenario = Scenario::parse(
            "\
# regression
seed = 7
signature = speeduino broken
page = 1 : 00 01 ff
fault = read-page:truncate:2
fault = burn:drop x3
",
        )
        .expect("a scenario");

        assert_eq!(scenario.seed, Some(7));
        assert_eq!(scenario.signature.as_deref(), Some("speeduino broken"));
        assert_eq!(scenario.pages, vec![(1, vec![0, 1, 255])]);
        assert_eq!(scenario.faults.len(), 2);
        assert_eq!(scenario.faults[0].remaining, 1);
        assert_eq!(scenario.faults[1].remaining, 3);
    }

    #[test]
    fn an_unknown_directive_names_its_line() {
        let error = Scenario::parse("seed = 1\nwibble = 2\n").expect_err("unknown");

        assert_eq!(error.line, 2);
        assert!(error.message.contains("wibble"));
    }

    #[test]
    fn a_fault_without_its_number_is_rejected() {
        let error = Scenario::parse("fault = read-page:truncate\n").expect_err("no number");

        assert_eq!(error.line, 1);
    }
}
