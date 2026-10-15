use crate::VarInt;
use crate::codec::data_component::{DataComponentCodec, deserialize, serialize};
use crate::ser::{NetworkReadExt, NetworkWriteExt, ReadingError, WritingError};
use pumpkin_data::data_component::DataComponent;
use pumpkin_data::data_component_extension;
use pumpkin_data::data_component_impl::{
    CustomDataImpl, CustomNameImpl, DataComponentImpl, ItemNameImpl,
};
use pumpkin_data::item::Item;
use pumpkin_data::item_stack::ItemStack;
use pumpkin_nbt::tag::NbtTag;
use pumpkin_util::text::TextComponent;
use pumpkin_util::version::JavaMinecraftVersion;
use std::borrow::Cow;
use std::io::Cursor;

#[derive(Clone)]
pub struct ItemStackSerializer<'a>(pub Cow<'a, ItemStack>);

fn item_component_counts(stack: &ItemStack) -> (u8, u8) {
    let mut to_add = 0u8;
    let mut to_remove = 0u8;

    for (_, data) in &stack.patch {
        if data.is_none() {
            to_remove += 1;
        } else {
            to_add += 1;
        }
    }
    for (_, bytes) in &stack.opaque_patch {
        if bytes.is_none() {
            to_remove += 1;
        } else {
            to_add += 1;
        }
    }

    (to_add, to_remove)
}

/// Writes the added extension components of `stack`, after the vanilla ones.
/// `length_prefixed` selects the delimited form some packets use.
fn write_opaque_added(
    stack: &ItemStack,
    length_prefixed: bool,
    write: &mut impl NetworkWriteExt,
) -> Result<(), WritingError> {
    for (id, bytes) in &stack.opaque_patch {
        if let Some(bytes) = bytes {
            write.put_var_int(&VarInt(i32::from(*id)))?;
            if length_prefixed {
                let len = i32::try_from(bytes.len())
                    .map_err(|_| WritingError::Message("Component data too large".into()))?;
                write.put_var_int(&VarInt(len))?;
            }
            write.write_slice(bytes)?;
        }
    }
    Ok(())
}

/// Writes the ids of the removed extension components of `stack`.
fn write_opaque_removed(
    stack: &ItemStack,
    write: &mut impl NetworkWriteExt,
) -> Result<(), WritingError> {
    for (id, bytes) in &stack.opaque_patch {
        if bytes.is_none() {
            write.put_var_int(&VarInt(i32::from(*id)))?;
        }
    }
    Ok(())
}

fn serialize_item_stack_with_id(
    stack: &ItemStack,
    item_id: u16,
    version: JavaMinecraftVersion,
    write: &mut impl NetworkWriteExt,
) -> Result<(), WritingError> {
    if version >= JavaMinecraftVersion::V_1_20_5 {
        if stack.is_empty() {
            write.put_var_int(&VarInt(0))
        } else {
            let (to_add, to_remove) = item_component_counts(stack);
            write.put_var_int(&VarInt::from(stack.item_count))?;
            write.put_var_int(&VarInt::from(item_id))?;
            write.put_var_int(&VarInt::from(to_add))?;
            write.put_var_int(&VarInt::from(to_remove))?;

            for (id, data) in &stack.patch {
                if let Some(data) = data {
                    write.put_var_int(&VarInt(i32::from(id.to_id())))?;
                    serialize(*id, data.as_ref(), write)?;
                }
            }
            write_opaque_added(stack, false, write)?;

            for (id, data) in &stack.patch {
                if data.is_none() {
                    write.put_var_int(&VarInt(i32::from(id.to_id())))?;
                }
            }
            write_opaque_removed(stack, write)?;

            Ok(())
        }
    } else if version >= JavaMinecraftVersion::V_1_13_2 {
        if stack.is_empty() {
            write.write_bool(false)
        } else {
            write.write_bool(true)?;
            write.put_var_int(&VarInt::from(item_id))?;
            write.write_i8(stack.item_count as i8)?;
            write.write_u8(0)?; // TAG_End (no NBT)
            Ok(())
        }
    } else if version >= JavaMinecraftVersion::V_1_13 {
        // 1.13 and 1.13.1: short id (-1 if empty), byte count, TAG_End
        if stack.is_empty() {
            write.write_i16_be(-1)
        } else {
            write.write_i16_be(item_id as i16)?;
            write.write_i8(stack.item_count as i8)?;
            write.write_u8(0)?; // TAG_End (no NBT)
            Ok(())
        }
    } else {
        // <= 1.12.2: short id (-1 if empty), byte count, short damage, TAG_End
        if stack.is_empty() {
            write.write_i16_be(-1)
        } else {
            write.write_i16_be(item_id as i16)?;
            write.write_i8(stack.item_count as i8)?;
            write.write_i16_be(0)?; // damage / metadata
            write.write_u8(0)?; // TAG_End (no NBT)
            Ok(())
        }
    }
}

fn serialize_length_prefixed_item_stack_with_id(
    stack: &ItemStack,
    item_id: u16,
    version: JavaMinecraftVersion,
    write: &mut impl NetworkWriteExt,
) -> Result<(), WritingError> {
    if version >= JavaMinecraftVersion::V_1_20_5 {
        if stack.is_empty() {
            write.put_var_int(&VarInt(0))
        } else {
            let (to_add, to_remove) = item_component_counts(stack);
            write.put_var_int(&VarInt::from(stack.item_count))?;
            write.put_var_int(&VarInt::from(item_id))?;
            write.put_var_int(&VarInt::from(to_add))?;
            write.put_var_int(&VarInt::from(to_remove))?;

            for (id, data) in &stack.patch {
                if let Some(data) = data {
                    write.put_var_int(&VarInt(i32::from(id.to_id())))?;
                    let mut comp_buf = Vec::new();
                    serialize(*id, data.as_ref(), &mut comp_buf)?;
                    write.put_var_int(&VarInt::from(comp_buf.len() as i32))?;
                    write.write_slice(&comp_buf)?;
                }
            }
            write_opaque_added(stack, true, write)?;

            for (id, data) in &stack.patch {
                if data.is_none() {
                    write.put_var_int(&VarInt(i32::from(id.to_id())))?;
                }
            }
            write_opaque_removed(stack, write)?;

            Ok(())
        }
    } else {
        serialize_item_stack_with_id(stack, item_id, version, write)
    }
}

fn serialize_item_cost_with_id(
    stack: &ItemStack,
    item_id: u16,
    _version: JavaMinecraftVersion,
    write: &mut impl NetworkWriteExt,
) -> Result<(), WritingError> {
    let component_count = stack
        .patch
        .iter()
        .filter(|(_, data)| data.is_some())
        .count();
    let component_count = i32::try_from(component_count)
        .map_err(|_| WritingError::Message("Too many item cost components".into()))?;

    write.put_var_int(&VarInt::from(item_id))?;
    write.put_var_int(&VarInt::from(stack.item_count))?;
    write.put_var_int(&VarInt(component_count))?;
    for (id, data) in &stack.patch {
        if let Some(data) = data {
            write.put_var_int(&VarInt(i32::from(id.to_id())))?;
            serialize(*id, data.as_ref(), write)?;
        }
    }
    Ok(())
}

fn component_from_raw_id(id_val: i32) -> Result<DataComponent, ReadingError> {
    let id_u8 = id_val
        .try_into()
        .map_err(|_| ReadingError::Message(format!("Invalid component ID: {id_val}")))?;
    DataComponent::try_from_id(id_u8)
        .ok_or_else(|| ReadingError::Message(format!("Unknown component ID: {id_val}")))
}

fn decode_custom_name(component_data: &[u8]) -> Result<Box<dyn DataComponentImpl>, ReadingError> {
    let mut cursor = Cursor::new(component_data);
    let mut nbt_reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(&mut cursor);
    let tag = NbtTag::deserialize(&mut nbt_reader)
        .map_err(|err| ReadingError::Message(format!("Failed to decode CustomName NBT: {err}")))?;
    let name = TextComponent::from_nbt(&tag);
    Ok(CustomNameImpl { name }.to_dyn())
}

fn decode_item_name(component_data: &[u8]) -> Result<Box<dyn DataComponentImpl>, ReadingError> {
    let mut cursor = Cursor::new(component_data);
    let mut nbt_reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(&mut cursor);
    let tag = NbtTag::deserialize(&mut nbt_reader)
        .map_err(|err| ReadingError::Message(format!("Failed to decode ItemName NBT: {err}")))?;
    let name = match tag {
        NbtTag::String(name) => name.to_string(),
        NbtTag::Compound(compound) => compound
            .get_string("translate")
            .or_else(|| compound.get_string("text"))
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    };
    Ok(ItemNameImpl {
        name: Cow::Owned(name),
    }
    .to_dyn())
}

fn decode_custom_data(component_data: &[u8]) -> Result<Box<dyn DataComponentImpl>, ReadingError> {
    let mut cursor = Cursor::new(component_data);
    let mut nbt_reader = pumpkin_nbt::deserializer::NbtReadHelperJava::new(&mut cursor);
    let tag = NbtTag::deserialize(&mut nbt_reader)
        .map_err(|err| ReadingError::Message(format!("Failed to decode CustomData NBT: {err}")))?;
    let data = match tag {
        NbtTag::Compound(compound) => compound,
        _ => pumpkin_nbt::compound::NbtCompound::new(),
    };
    Ok(CustomDataImpl::new(data).to_dyn())
}

fn decode_component(
    id: DataComponent,
    component_data: &[u8],
) -> Result<Box<dyn DataComponentImpl>, ReadingError> {
    match id {
        DataComponent::CustomName => decode_custom_name(component_data),
        DataComponent::ItemName => decode_item_name(component_data),
        DataComponent::CustomData => decode_custom_data(component_data),
        _ => {
            let mut cursor = Cursor::new(component_data);
            deserialize(id, &mut cursor)
        }
    }
}

/// Reads a length-prefixed component value as raw bytes, for an extension type.
fn read_length_prefixed_bytes(read: &mut impl NetworkReadExt) -> Result<Box<[u8]>, ReadingError> {
    let byte_len: usize = read
        .get_var_int()?
        .0
        .try_into()
        .map_err(|_| ReadingError::Message("Negative component data length".into()))?;
    if byte_len > crate::MAX_PACKET_DATA_SIZE {
        return Err(ReadingError::TooLarge("Component data too large".into()));
    }
    let mut bytes = vec![0u8; byte_len];
    read.read_bytes_to_buf(&mut bytes)?;
    Ok(bytes.into_boxed_slice())
}

fn read_length_prefixed_component(
    id: DataComponent,
    read: &mut impl NetworkReadExt,
) -> Result<(DataComponent, Box<dyn DataComponentImpl>), ReadingError> {
    let byte_len = read.get_var_int()?.0;
    let byte_len: usize = byte_len
        .try_into()
        .map_err(|_| ReadingError::Message("Negative component data length".into()))?;
    if byte_len > crate::MAX_PACKET_DATA_SIZE {
        return Err(ReadingError::TooLarge("Component data too large".into()));
    }

    let component_impl = if byte_len <= 256 {
        let mut stack_buf = [0u8; 256];
        let slice = &mut stack_buf[..byte_len];
        read.read_bytes_to_buf(slice)?;
        decode_component(id, slice)?
    } else {
        let mut component_data = vec![0u8; byte_len];
        read.read_bytes_to_buf(&mut component_data)?;
        decode_component(id, &component_data)?
    };

    Ok((id, component_impl))
}

impl ItemStackSerializer<'_> {
    pub fn read(
        read: &mut impl NetworkReadExt,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        const MAX_COMPONENTS: i32 = 256;

        let item_count = read.get_var_int()?;
        if item_count.0 == 0 {
            return Ok(ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)));
        }

        let item_id = read.get_var_int()?;
        let num_to_add = read.get_var_int()?.0;
        let num_to_remove = read.get_var_int()?.0;

        if num_to_add < 0 || num_to_remove < 0 {
            return Err(ReadingError::Message("Negative component count".into()));
        }

        let total_components = num_to_add
            .checked_add(num_to_remove)
            .ok_or_else(|| ReadingError::Message("Component count overflow".into()))?;

        if total_components > MAX_COMPONENTS {
            return Err(ReadingError::Message(
                "Too many components in ItemStack patch".into(),
            ));
        }

        let mut patch = Vec::with_capacity((num_to_add + num_to_remove) as usize);

        for _ in 0..num_to_add {
            let id_val = read.get_var_int()?.0;
            let id = DataComponent::try_from_id(id_val as u8)
                .ok_or_else(|| ReadingError::Message(format!("Unknown component ID: {id_val}")))?;

            let component_impl = if id == DataComponent::CustomData {
                CustomDataImpl::deserialize(read)?.to_dyn()
            } else {
                deserialize(id, read)?
            };
            patch.push((id, Some(component_impl)));
        }

        for _ in 0..num_to_remove {
            let id_val = read.get_var_int()?.0;
            let id = DataComponent::try_from_id(id_val as u8)
                .ok_or_else(|| ReadingError::Message("Unknown component ID".into()))?;
            patch.push((id, None));
        }

        let item_id_u16: u16 = item_id
            .0
            .try_into()
            .map_err(|_| ReadingError::Message("Invalid item id!".into()))?;

        Ok(ItemStackSerializer(Cow::Owned(
            ItemStack::new_with_component(
                item_count.0 as u8,
                Item::from_id(item_id_u16).unwrap_or(&Item::AIR),
                patch,
            ),
        )))
    }

    pub fn read_with_version(
        read: &mut impl NetworkReadExt,
        version: &JavaMinecraftVersion,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        if *version >= JavaMinecraftVersion::V_1_20_5 {
            let serializer = Self::read(read)?;
            if *version < JavaMinecraftVersion::V_26_3 {
                Ok(ItemStackSerializer(Cow::Owned(
                    serializer.to_stack_for_version(version),
                )))
            } else {
                Ok(serializer)
            }
        } else if *version >= JavaMinecraftVersion::V_1_13_2 {
            let present = read.get_bool()?;
            if !present {
                return Ok(ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)));
            }
            let raw_item_id = read.get_var_int()?.0 as u16;
            let count = read.get_i8()? as u8;
            let nbt_type = read.get_u8()?;
            if nbt_type != 0 {
                // TAG_End is 0 when no NBT is present
            }
            let item = Item::from_id(raw_item_id).unwrap_or(&Item::AIR);
            Ok(ItemStackSerializer(Cow::Owned(ItemStack::new(count, item))))
        } else if *version >= JavaMinecraftVersion::V_1_13 {
            let raw_item_id = read.get_i16_be()?;
            if raw_item_id == -1 || raw_item_id < 0 {
                return Ok(ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)));
            }
            let count = read.get_i8()? as u8;
            let nbt_type = read.get_u8()?;
            if nbt_type != 0 {
                // TAG_End is 0 when no NBT is present
            }
            let item = Item::from_id(raw_item_id as u16).unwrap_or(&Item::AIR);
            Ok(ItemStackSerializer(Cow::Owned(ItemStack::new(count, item))))
        } else {
            let raw_item_id = read.get_i16_be()?;
            if raw_item_id == -1 || raw_item_id < 0 {
                return Ok(ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)));
            }
            let count = read.get_i8()? as u8;
            let _damage = read.get_i16_be()?;
            let nbt_type = read.get_u8()?;
            if nbt_type != 0 {
                // TAG_End is 0 when no NBT is present
            }
            let item = Item::from_id(raw_item_id as u16).unwrap_or(&Item::AIR);
            Ok(ItemStackSerializer(Cow::Owned(ItemStack::new(count, item))))
        }
    }

    pub fn read_untrusted_with_version(
        read: &mut impl NetworkReadExt,
        version: &JavaMinecraftVersion,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        if *version >= JavaMinecraftVersion::V_1_21_5 {
            Self::read_length_prefixed_optional(read)
        } else {
            Self::read_with_version(read, version)
        }
    }

    pub fn read_template_with_version(
        read: &mut impl NetworkReadExt,
        version: &JavaMinecraftVersion,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        if *version < JavaMinecraftVersion::V_26_1 {
            Self::read_with_version(read, version)
        } else {
            Self::read_template0(read, version)
        }
    }

    pub fn read_optional_template_with_version(
        read: &mut impl NetworkReadExt,
        version: &JavaMinecraftVersion,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        if *version < JavaMinecraftVersion::V_26_1 {
            Self::read_with_version(read, version)
        } else if read.get_bool()? {
            Self::read_template0(read, version)
        } else {
            Ok(ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)))
        }
    }

    pub fn read_template0(
        read: &mut impl NetworkReadExt,
        _version: &JavaMinecraftVersion,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        const MAX_COMPONENTS: i32 = 256;

        let raw_item_id = read.get_var_int()?;
        let item_count = read.get_var_int()?;

        let item_id_u16: u16 = raw_item_id
            .0
            .try_into()
            .map_err(|_| ReadingError::Message("Invalid item id!".into()))?;
        let item = Item::from_id(item_id_u16).unwrap_or(&Item::AIR);

        let num_to_add = read.get_var_int()?.0;
        let num_to_remove = read.get_var_int()?.0;

        if num_to_add < 0 || num_to_remove < 0 {
            return Err(ReadingError::Message("Negative component count".into()));
        }

        let total_components = num_to_add
            .checked_add(num_to_remove)
            .ok_or_else(|| ReadingError::Message("Component count overflow".into()))?;

        if total_components > MAX_COMPONENTS {
            return Err(ReadingError::Message(
                "Too many components in ItemStack patch".into(),
            ));
        }

        let mut patch = Vec::with_capacity(total_components as usize);

        for _ in 0..num_to_add {
            let id_val = read.get_var_int()?.0;
            let id = DataComponent::try_from_id(id_val as u8)
                .ok_or_else(|| ReadingError::Message(format!("Unknown component ID: {id_val}")))?;

            let component_impl = if id == DataComponent::CustomData {
                CustomDataImpl::deserialize(read)?.to_dyn()
            } else {
                deserialize(id, read)?
            };
            patch.push((id, Some(component_impl)));
        }

        for _ in 0..num_to_remove {
            let id_val = read.get_var_int()?.0;
            let id = DataComponent::try_from_id(id_val as u8)
                .ok_or_else(|| ReadingError::Message("Unknown component ID".into()))?;
            patch.push((id, None));
        }

        let item_count_u8: u8 = item_count
            .0
            .try_into()
            .map_err(|_| ReadingError::Message("Invalid item count!".into()))?;

        let stack = ItemStack::new_with_component(item_count_u8, item, patch);
        if stack.is_empty() {
            return Err(ReadingError::Message(
                "Can't read empty item stack template".into(),
            ));
        }

        Ok(ItemStackSerializer(Cow::Owned(stack)))
    }

    pub fn write(&self, write: &mut impl NetworkWriteExt) -> Result<(), WritingError> {
        self.write_with_version(write, &JavaMinecraftVersion::V_26_3)
    }

    pub fn read_length_prefixed_optional(
        read: &mut impl NetworkReadExt,
    ) -> Result<ItemStackSerializer<'static>, ReadingError> {
        const MAX_COMPONENTS: i32 = 256;

        let item_count = read.get_var_int()?;
        if item_count.0 == 0 {
            return Ok(ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)));
        }
        let item_count_u8 = item_count
            .0
            .try_into()
            .map_err(|_| ReadingError::Message("Invalid item count!".into()))?;

        let item_id = read.get_var_int()?;
        let num_to_add = read.get_var_int()?.0;
        let num_to_remove = read.get_var_int()?.0;

        if num_to_add < 0 || num_to_remove < 0 {
            return Err(ReadingError::Message("Negative component count".into()));
        }

        let total_components = num_to_add
            .checked_add(num_to_remove)
            .ok_or_else(|| ReadingError::Message("Component count overflow".into()))?;

        if total_components > MAX_COMPONENTS {
            return Err(ReadingError::Message(
                "Too many components in ItemStack patch".into(),
            ));
        }

        let mut patch = Vec::with_capacity(total_components as usize);
        let mut opaque_patch = Vec::new();

        // Every value here is length-prefixed, so an extension component — whose
        // format only the mod knows — can be kept as raw bytes instead of failing.
        for _ in 0..num_to_add {
            let raw_id = read.get_var_int()?.0;
            if data_component_extension::is_extension(raw_id) {
                opaque_patch.push((raw_id as u16, Some(read_length_prefixed_bytes(read)?)));
            } else {
                let (id, component_impl) =
                    read_length_prefixed_component(component_from_raw_id(raw_id)?, read)?;
                patch.push((id, Some(component_impl)));
            }
        }

        for _ in 0..num_to_remove {
            let raw_id = read.get_var_int()?.0;
            if data_component_extension::is_extension(raw_id) {
                opaque_patch.push((raw_id as u16, None));
            } else {
                patch.push((component_from_raw_id(raw_id)?, None));
            }
        }

        let item_id_u16 = item_id
            .0
            .try_into()
            .map_err(|_| ReadingError::Message("Invalid item id!".into()))?;

        let mut stack = ItemStack::new_with_component(
            item_count_u8,
            Item::from_id(item_id_u16).unwrap_or(&Item::AIR),
            patch,
        );
        stack.opaque_patch = opaque_patch;
        Ok(ItemStackSerializer(Cow::Owned(stack)))
    }

    pub fn write_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        serialize_item_stack_with_id(self.0.as_ref(), self.0.item.id, *version, write)
    }

    pub fn write_length_prefixed_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        serialize_length_prefixed_item_stack_with_id(
            self.0.as_ref(),
            self.0.item.id,
            *version,
            write,
        )
    }

    pub fn write_item_cost_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        serialize_item_cost_with_id(self.0.as_ref(), self.0.item.id, *version, write)
    }

    pub fn write_untrusted_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if *version >= JavaMinecraftVersion::V_1_21_5 {
            self.write_length_prefixed_with_version(write, version)
        } else {
            self.write_with_version(write, version)
        }
    }

    pub fn write_template_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if *version < JavaMinecraftVersion::V_26_1 {
            self.write_with_version(write, version)
        } else {
            self.write_template0(write, version)
        }
    }

    pub fn write_optional_template_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if *version < JavaMinecraftVersion::V_26_1 {
            self.write_with_version(write, version)
        } else if !self.0.is_empty() {
            write.write_bool(true)?;
            self.write_template0(write, version)
        } else {
            write.write_bool(false)
        }
    }

    pub fn write_template0(
        &self,
        write: &mut impl NetworkWriteExt,
        _version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        if self.0.is_empty() {
            return Err(WritingError::Message(
                "Can't write empty item stack template".into(),
            ));
        }
        let (to_add, to_remove) = item_component_counts(self.0.as_ref());
        write.put_var_int(&VarInt::from(self.0.item.id))?;
        write.put_var_int(&VarInt::from(self.0.item_count))?;
        write.put_var_int(&VarInt::from(to_add))?;
        write.put_var_int(&VarInt::from(to_remove))?;

        for (id, data) in &self.0.patch {
            if let Some(data) = data {
                write.put_var_int(&VarInt(i32::from(id.to_id())))?;
                serialize(*id, data.as_ref(), write)?;
            }
        }

        for (id, data) in &self.0.patch {
            if data.is_none() {
                write.put_var_int(&VarInt(i32::from(id.to_id())))?;
            }
        }

        Ok(())
    }

    #[must_use]
    pub fn to_stack(self) -> ItemStack {
        self.0.into_owned()
    }

    #[must_use]
    pub fn to_stack_for_version(self, _version: &JavaMinecraftVersion) -> ItemStack {
        self.0.into_owned()
    }
}

impl From<ItemStack> for ItemStackSerializer<'_> {
    fn from(item: ItemStack) -> Self {
        ItemStackSerializer(Cow::Owned(item))
    }
}

impl From<Option<ItemStack>> for ItemStackSerializer<'_> {
    fn from(item: Option<ItemStack>) -> Self {
        item.map_or_else(
            || ItemStackSerializer(Cow::Borrowed(ItemStack::EMPTY)),
            ItemStackSerializer::from,
        )
    }
}

#[derive(Debug, Clone)]
pub struct ItemComponentHash {
    pub added: Vec<(VarInt, i32)>,
    pub removed: Vec<VarInt>,
}

impl ItemComponentHash {
    pub fn read(read: &mut impl NetworkReadExt) -> Result<Self, ReadingError> {
        const MAX_COMPONENTS: i32 = 256;

        let added_length = read.get_var_int()?;
        if added_length.0 < 0 || added_length.0 > MAX_COMPONENTS {
            return Err(ReadingError::Message("added_length out of bounds".into()));
        }
        let mut added = Vec::with_capacity(added_length.0 as usize);
        for _ in 0..added_length.0 {
            let component_id = read.get_var_int()?;
            let component_value = read.get_i32()?;
            added.push((component_id, component_value));
        }

        let removed_length = read.get_var_int()?;
        if removed_length.0 < 0 || removed_length.0 > MAX_COMPONENTS {
            return Err(ReadingError::Message("removed_length out of bounds".into()));
        }
        let mut removed = Vec::with_capacity(removed_length.0 as usize);
        for _ in 0..removed_length.0 {
            let component_id = read.get_var_int()?;
            removed.push(component_id);
        }

        Ok(Self { added, removed })
    }

    pub fn write(&self, write: &mut impl NetworkWriteExt) -> Result<(), WritingError> {
        write.put_var_int(&VarInt::from(self.added.len() as i32))?;
        for (id, val) in &self.added {
            write.put_var_int(id)?;
            write.put_i32(*val)?;
        }
        write.put_var_int(&VarInt::from(self.removed.len() as i32))?;
        for id in &self.removed {
            write.put_var_int(id)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ItemStackHash {
    item_id: VarInt,
    count: VarInt,
    components: ItemComponentHash,
}

#[derive(Debug, Clone)]
pub struct OptionalItemStackHash(pub Option<ItemStackHash>);

impl OptionalItemStackHash {
    pub fn read(read: &mut impl NetworkReadExt) -> Result<Self, ReadingError> {
        let is_some = read.get_bool()?;
        if is_some {
            let item_id = read.get_var_int()?;
            let count = read.get_var_int()?;
            let components = ItemComponentHash::read(read)?;

            Ok(Self(Some(ItemStackHash {
                item_id,
                count,
                components,
            })))
        } else {
            Ok(Self(None))
        }
    }

    pub fn write(&self, write: &mut impl NetworkWriteExt) -> Result<(), WritingError> {
        if let Some(hash) = &self.0 {
            write.put_bool(true)?;
            write.put_var_int(&hash.item_id)?;
            write.put_var_int(&hash.count)?;
            hash.components.write(write)?;
        } else {
            write.put_bool(false)?;
        }
        Ok(())
    }

    #[must_use]
    pub fn hash_equals(&self, other: &ItemStack) -> bool {
        if let Some(hash) = &self.0 {
            if hash.item_id != other.item.id.into() || hash.count != other.item_count.into() {
                return false;
            }
            let calc = || {
                let mut to_add = 0u8;
                let mut to_remove = 0u8;
                for (_id, data) in &other.patch {
                    if data.is_none() {
                        to_remove += 1;
                    } else {
                        to_add += 1;
                    }
                }
                for (_id, bytes) in &other.opaque_patch {
                    if bytes.is_none() {
                        to_remove += 1;
                    } else {
                        to_add += 1;
                    }
                }
                (to_add, to_remove)
            };
            let (to_add, to_remove) = calc();
            if to_add as usize != hash.components.added.len()
                || to_remove as usize != hash.components.removed.len()
            {
                return false;
            }
            for (other_id, data) in &other.patch {
                if let Some(data) = data {
                    let checksum = data.get_hash();
                    for (id, hash) in &hash.components.added {
                        if id == &VarInt::from(other_id.to_id()) {
                            if hash == &checksum {
                                break;
                            }
                            return false;
                        }
                    }
                } else if !hash
                    .components
                    .removed
                    .contains(&VarInt::from(other_id.to_id()))
                {
                    return false;
                }
            }
            // The server cannot compute the hash of a value it has no codec for,
            // so an extension component only has to be present on both sides.
            for (other_id, bytes) in &other.opaque_patch {
                let id = VarInt(i32::from(*other_id));
                let present = if bytes.is_some() {
                    hash.components.added.iter().any(|(added, _)| added == &id)
                } else {
                    hash.components.removed.contains(&id)
                };
                if !present {
                    return false;
                }
            }
            true
        } else {
            other.is_empty()
        }
    }
}

pub struct ItemStackTemplateSerializer<'a>(pub Cow<'a, ItemStack>);

impl ItemStackTemplateSerializer<'_> {
    pub fn write_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let serializer = ItemStackSerializer(Cow::Borrowed(self.0.as_ref()));
        serializer.write_template_with_version(write, version)
    }

    pub fn write(&self, write: &mut impl NetworkWriteExt) -> Result<(), WritingError> {
        self.write_with_version(write, &JavaMinecraftVersion::V_26_3)
    }
}

impl From<ItemStack> for ItemStackTemplateSerializer<'_> {
    fn from(item: ItemStack) -> Self {
        ItemStackTemplateSerializer(Cow::Owned(item))
    }
}

pub struct ItemStackOptionalTemplateSerializer<'a>(pub Cow<'a, ItemStack>);

impl ItemStackOptionalTemplateSerializer<'_> {
    pub fn write_with_version(
        &self,
        write: &mut impl NetworkWriteExt,
        version: &JavaMinecraftVersion,
    ) -> Result<(), WritingError> {
        let serializer = ItemStackSerializer(Cow::Borrowed(self.0.as_ref()));
        serializer.write_optional_template_with_version(write, version)
    }

    pub fn write(&self, write: &mut impl NetworkWriteExt) -> Result<(), WritingError> {
        self.write_with_version(write, &JavaMinecraftVersion::V_26_3)
    }
}

impl From<ItemStack> for ItemStackOptionalTemplateSerializer<'_> {
    fn from(item: ItemStack) -> Self {
        ItemStackOptionalTemplateSerializer(Cow::Owned(item))
    }
}

impl From<Option<ItemStack>> for ItemStackOptionalTemplateSerializer<'_> {
    fn from(item: Option<ItemStack>) -> Self {
        item.map_or_else(
            || ItemStackOptionalTemplateSerializer(Cow::Borrowed(ItemStack::EMPTY)),
            ItemStackOptionalTemplateSerializer::from,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{ItemStackSerializer, OptionalItemStackHash};
    use crate::VarInt;
    use crate::ser::NetworkWriteExt;
    use pumpkin_data::data_component_extension::{self, VANILLA_COUNT};
    use pumpkin_data::item::Item;
    use pumpkin_util::version::JavaMinecraftVersion;
    use std::borrow::Cow;

    fn var_int(out: &mut Vec<u8>, value: i32) {
        out.put_var_int(&VarInt(value)).expect("writing to a Vec");
    }

    /// A stack whose patch sets one extension component to the single byte 5,
    /// as `ItemStack.OPTIONAL_UNTRUSTED_STREAM_CODEC` frames it (delimited).
    fn delimited_stack(component_id: u16) -> Vec<u8> {
        let mut out = Vec::new();
        var_int(&mut out, 1); // count
        var_int(&mut out, i32::from(Item::STONE.id));
        var_int(&mut out, 1); // components added
        var_int(&mut out, 0); // components removed
        var_int(&mut out, i32::from(component_id));
        var_int(&mut out, 1); // value length
        out.push(5); // value
        out
    }

    /// One test for the module: the extension is a process-wide `OnceLock`.
    #[test]
    fn extension_components_round_trip_as_opaque_bytes() {
        let id = VANILLA_COUNT;
        let wire = delimited_stack(id);

        // Before the type is installed, it is unknown and the read fails.
        assert!(ItemStackSerializer::read_length_prefixed_optional(&mut wire.as_slice()).is_err());

        data_component_extension::install(vec![(id, "pumpkinverse_test:charge")])
            .expect("contiguous ids install");

        let stack = ItemStackSerializer::read_length_prefixed_optional(&mut wire.as_slice())
            .expect("an installed extension component is accepted")
            .to_stack();
        assert!(stack.patch.is_empty());
        assert_eq!(
            stack.opaque_patch,
            vec![(id, Some(Box::from([5u8].as_slice())))]
        );

        let version = JavaMinecraftVersion::V_26_3;
        let serializer = ItemStackSerializer(Cow::Borrowed(&stack));

        // Delimited form: byte-identical to what the client sent.
        let mut delimited = Vec::new();
        serializer
            .write_length_prefixed_with_version(&mut delimited, &version)
            .expect("writing to a Vec");
        assert_eq!(delimited, wire);

        // Plain form (server to client): the same, without the value's length.
        let mut plain = Vec::new();
        serializer
            .write_with_version(&mut plain, &version)
            .expect("writing to a Vec");
        let mut expected = Vec::new();
        var_int(&mut expected, 1);
        var_int(&mut expected, i32::from(Item::STONE.id));
        var_int(&mut expected, 1);
        var_int(&mut expected, 0);
        var_int(&mut expected, i32::from(id));
        expected.push(5);
        assert_eq!(plain, expected);

        // A hashed stack (container clicks) matches on presence alone.
        let mut hashed = Vec::new();
        hashed.push(1); // present
        var_int(&mut hashed, i32::from(Item::STONE.id));
        var_int(&mut hashed, 1); // count
        var_int(&mut hashed, 1); // components added
        var_int(&mut hashed, i32::from(id));
        hashed.extend_from_slice(&0x1234_5678i32.to_be_bytes()); // any hash
        var_int(&mut hashed, 0); // components removed
        let hash = OptionalItemStackHash::read(&mut hashed.as_slice()).expect("valid hash");
        assert!(hash.hash_equals(&stack));
        assert!(!hash.hash_equals(&pumpkin_data::item_stack::ItemStack::new(1, &Item::STONE)));
    }
}
