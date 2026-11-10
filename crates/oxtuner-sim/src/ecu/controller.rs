use std::collections::HashMap;

use oxtuner_ini::Definition;
use oxtuner_protocol::CommandPattern;

use super::pattern_for;

/// Every controller action command string, in definition order.
pub(super) fn build_controller(
    definition: &Definition,
    variables: &HashMap<String, u8>,
    force_big_endian: bool,
) -> Vec<CommandPattern> {
    let mut controller = Vec::new();
    for (_, action) in definition.controller_commands.iter() {
        for command in &action.commands {
            if let Some(pattern) = pattern_for(command, variables, force_big_endian) {
                controller.push(pattern);
            }
        }
    }
    controller
}
