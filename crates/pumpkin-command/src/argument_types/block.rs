use crate::{
    argument_types::argument_type::{ArgumentType, JavaClientArgumentType},
    context::command_context::CommandContext,
    errors::command_syntax_error::CommandSyntaxError,
    errors::error_types::CommandErrorType,
    string_reader::StringReader,
    suggestion::suggestions::{Suggestions, SuggestionsBuilder},
};
use pumpkin_data::{Block, BlockStateId, translation};
use pumpkin_util::text::TextComponent;

pub const INVALID_BLOCK_ERROR_TYPE: CommandErrorType<1> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_ID_INVALID,
    translation::java::ARGUMENT_BLOCK_ID_INVALID,
);
pub const UNKNOWN_PROPERTY_ERROR_TYPE: CommandErrorType<2> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_PROPERTY_UNKNOWN,
    translation::java::ARGUMENT_BLOCK_PROPERTY_UNKNOWN,
);
pub const INVALID_PROPERTY_ERROR_TYPE: CommandErrorType<3> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_PROPERTY_INVALID,
    translation::java::ARGUMENT_BLOCK_PROPERTY_INVALID,
);
pub const DUPLICATE_PROPERTY_ERROR_TYPE: CommandErrorType<2> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_PROPERTY_DUPLICATE,
    translation::java::ARGUMENT_BLOCK_PROPERTY_DUPLICATE,
);
pub const MISSING_VALUE_ERROR_TYPE: CommandErrorType<2> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_PROPERTY_NOVALUE,
    translation::java::ARGUMENT_BLOCK_PROPERTY_NOVALUE,
);
pub const UNCLOSED_PROPERTIES_ERROR_TYPE: CommandErrorType<0> = CommandErrorType::new(
    translation::java::ARGUMENT_BLOCK_PROPERTY_UNCLOSED,
    translation::java::ARGUMENT_BLOCK_PROPERTY_UNCLOSED,
);

#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct BlockArgumentType;

impl<S: crate::source::CommandSource> ArgumentType<S> for BlockArgumentType {
    type Item = &'static Block;

    fn parse(&self, reader: &mut StringReader) -> Result<Self::Item, CommandSyntaxError> {
        let start = reader.cursor();
        while let Some(c) = reader.peek() {
            if c.is_alphanumeric() || c == '_' || c == ':' || c == '/' || c == '.' || c == '-' {
                reader.skip();
            } else {
                break;
            }
        }
        let block_name = &reader.string()[start..reader.cursor()];
        let normalized = if block_name.contains(':') {
            block_name.to_string()
        } else {
            format!("minecraft:{block_name}")
        };

        Block::from_name(&normalized)
            .ok_or_else(|| INVALID_BLOCK_ERROR_TYPE.create(reader, TextComponent::text(normalized)))
    }

    fn client_side_parser(&'_ self) -> JavaClientArgumentType {
        JavaClientArgumentType::BlockState
    }

    fn list_suggestions(
        &self,
        _context: &CommandContext<S>,
        builder: SuggestionsBuilder,
    ) -> Suggestions {
        builder.build()
    }
}

impl BlockArgumentType {
    pub fn get<S: crate::source::CommandSource>(
        context: &CommandContext<S>,
        name: &str,
    ) -> Result<&'static Block, CommandSyntaxError> {
        context.get_argument::<&'static Block>(name).copied()
    }
}

/// A block with optional state properties, as `/setblock` takes it in vanilla:
/// `minecraft:furnace[facing=east,lit=true]`.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
pub struct BlockStateArgumentType;

impl<S: crate::source::CommandSource> ArgumentType<S> for BlockStateArgumentType {
    type Item = (&'static Block, BlockStateId);

    fn parse(&self, reader: &mut StringReader) -> Result<Self::Item, CommandSyntaxError> {
        let block = <BlockArgumentType as ArgumentType<S>>::parse(&BlockArgumentType, reader)?;
        if reader.peek() != Some('[') {
            return Ok((block, block.default_state.id));
        }
        reader.skip();

        let block_name = || TextComponent::text(block.name);
        let mut pairs: Vec<(String, String)> = Vec::new();
        loop {
            reader.skip_whitespace();
            if reader.peek() == Some(']') {
                reader.skip();
                break;
            }
            let key = reader.read_unquoted_string();
            if key.is_empty() {
                return Err(UNCLOSED_PROPERTIES_ERROR_TYPE.create(reader));
            }
            if pairs.iter().any(|(seen, _)| *seen == key) {
                return Err(DUPLICATE_PROPERTY_ERROR_TYPE.create(
                    reader,
                    TextComponent::text(key),
                    block_name(),
                ));
            }
            reader.skip_whitespace();
            if reader.peek() != Some('=') {
                return Err(MISSING_VALUE_ERROR_TYPE.create(
                    reader,
                    TextComponent::text(key),
                    block_name(),
                ));
            }
            reader.skip();
            reader.skip_whitespace();
            let value = reader.read_unquoted_string();
            if value.is_empty() {
                return Err(MISSING_VALUE_ERROR_TYPE.create(
                    reader,
                    TextComponent::text(key),
                    block_name(),
                ));
            }
            pairs.push((key, value));
            reader.skip_whitespace();
            match reader.peek() {
                Some(',') => reader.skip(),
                Some(']') => {
                    reader.skip();
                    break;
                }
                _ => return Err(UNCLOSED_PROPERTIES_ERROR_TYPE.create(reader)),
            }
        }

        state_with(block, &pairs, reader).map(|state| (block, state))
    }

    fn client_side_parser(&'_ self) -> JavaClientArgumentType {
        JavaClientArgumentType::BlockState
    }

    fn list_suggestions(
        &self,
        _context: &CommandContext<S>,
        builder: SuggestionsBuilder,
    ) -> Suggestions {
        builder.build()
    }
}

/// Finds the state of `block` whose properties are its default ones with `pairs`
/// applied.
///
/// Valid values are learnt by enumerating the block's actual states rather than
/// by calling `Block::from_properties`: the generated property types panic on a
/// value they do not know, which a typo in a command must never be able to do.
/// The same walk works for vanilla blocks and for runtime extension blocks.
fn state_with(
    block: &'static Block,
    pairs: &[(String, String)],
    reader: &StringReader,
) -> Result<BlockStateId, CommandSyntaxError> {
    if pairs.is_empty() {
        return Ok(block.default_state.id);
    }
    let block_name = || TextComponent::text(block.name);

    let props_of = |state: BlockStateId| block.properties(state).map(|p| p.to_props());
    let mut desired = props_of(block.default_state.id).unwrap_or_default();

    for (key, value) in pairs {
        let Some(slot) = desired.iter().position(|p| p.key_str() == key) else {
            return Err(UNKNOWN_PROPERTY_ERROR_TYPE.create(
                reader,
                block_name(),
                TextComponent::text(key.clone()),
            ));
        };
        let accepted = block.states.iter().find_map(|state| {
            props_of(state.id)?
                .into_iter()
                .find(|candidate| candidate.key_str() == key && candidate.value_str() == value)
        });
        let Some(accepted) = accepted else {
            return Err(INVALID_PROPERTY_ERROR_TYPE.create(
                reader,
                block_name(),
                TextComponent::text(value.clone()),
                TextComponent::text(key.clone()),
            ));
        };
        desired[slot] = accepted;
    }

    Ok(block
        .states
        .iter()
        .find(|state| props_of(state.id).as_ref() == Some(&desired))
        .map_or(block.default_state.id, |state| state.id))
}

impl BlockStateArgumentType {
    pub fn get<S: crate::source::CommandSource>(
        context: &CommandContext<S>,
        name: &str,
    ) -> Result<(&'static Block, BlockStateId), CommandSyntaxError> {
        context
            .get_argument::<(&'static Block, BlockStateId)>(name)
            .copied()
    }
}
