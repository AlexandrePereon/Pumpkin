use std::io::Write;

use crate::{ClientPacket, WritingError, ser::NetworkWriteExt};

use crate::codec::var_int::VarInt;
use pumpkin_data::{
    packet::clientbound::config::UPDATE_TAGS, tag::RegistryKey, tag_extension::network_tags,
};
use pumpkin_macros::java_packet;
use pumpkin_util::version::JavaMinecraftVersion;

#[java_packet(UPDATE_TAGS)]
pub struct CUpdateTags<'a> {
    pub tags: &'a [pumpkin_data::tag::RegistryKey],
}

impl<'a> CUpdateTags<'a> {
    #[must_use]
    pub const fn new(tags: &'a [RegistryKey]) -> Self {
        Self { tags }
    }
}

impl ClientPacket for CUpdateTags<'_> {
    fn write_packet_data(
        &self,
        mut write: impl Write,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let valid_keys: Vec<_> = self
            .tags
            .iter()
            .copied()
            .filter(|key| key.is_valid_for_version(*version))
            .collect();

        write.write_list(&valid_keys, |p, &registry_key| {
            p.write_string(&format!("minecraft:{}", registry_key.identifier_string()))?;

            // Vanilla's tags as the datapacks left them, and theirs.
            let values = network_tags(registry_key);
            p.write_var_int(&values.len().try_into().map_err(|_| {
                WritingError::Message(format!("{} isn't representable as a VarInt", values.len()))
            })?)?;

            for (key, ids) in &values {
                // This is technically a `ResourceLocation` but same thing
                p.write_string_bounded(key, u16::MAX as usize)?;
                p.write_list(ids, |p, &id| p.write_var_int(&VarInt::from(id)))?;
            }

            Ok(())
        })
    }
}
