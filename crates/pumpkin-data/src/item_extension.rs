//! Runtime extension of the generated item registry.
//!
//! The counterpart of [`crate::block_extension`] for items. Vanilla items are
//! generated at build time into `generated/item.rs`; items contributed at
//! startup get raw ids continuing the vanilla range, so `Item::from_id` and
//! `Item::from_registry_key` can resolve ids and names the generated code knows
//! nothing about.
//!
//! The extension is installed once, before the server accepts connections, and
//! is read-only afterwards. Reads are therefore lock-free.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use crate::item::Item;

/// Why [`install`] rejected an extension.
#[derive(Debug)]
pub enum InstallError {
    /// An extension is already installed.
    AlreadyInstalled,
    /// Raw item ids must continue the vanilla range without gaps.
    IdNotContiguous { expected: u16, found: u16 },
    /// The name is already taken, either by vanilla or earlier in this batch.
    DuplicateName { name: &'static str },
    /// Raw ids would exceed what `u16` can carry.
    TooMany,
}

impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyInstalled => write!(f, "an item extension is already installed"),
            Self::IdNotContiguous { expected, found } => {
                write!(f, "expected item id {expected}, found {found}")
            }
            Self::DuplicateName { name } => write!(f, "item name {name} is already registered"),
            Self::TooMany => write!(f, "too many items to address with a u16 id"),
        }
    }
}

impl std::error::Error for InstallError {}

struct Extension {
    /// Indexed by `raw_id - Item::VANILLA_COUNT`.
    items: Vec<&'static Item>,
    by_name: BTreeMap<&'static str, &'static Item>,
}

static EXTENSION: OnceLock<Extension> = OnceLock::new();

/// Installs the runtime item extension.
///
/// Every item must already be leaked to `'static` and carry a raw id
/// continuing the vanilla range, gapless and ascending, because lookups index
/// into them directly. An item's `registry_key` is its full `namespace:path`.
///
/// # Errors
///
/// Returns [`InstallError`] when an id is out of sequence, a name collides, or
/// an extension is already installed. Nothing is installed on error.
pub fn install(entries: Vec<&'static Item>) -> Result<(), InstallError> {
    if EXTENSION.get().is_some() {
        return Err(InstallError::AlreadyInstalled);
    }

    let mut by_name = BTreeMap::new();
    let mut next_id = Item::VANILLA_COUNT;

    for &item in &entries {
        if item.id != next_id {
            return Err(InstallError::IdNotContiguous {
                expected: next_id,
                found: item.id,
            });
        }
        // `Item::from_registry_key` only sees vanilla here, as the extension is
        // not installed yet; a collision would otherwise be silent.
        if Item::from_registry_key(item.registry_key).is_some()
            || by_name.insert(item.registry_key, item).is_some()
        {
            return Err(InstallError::DuplicateName {
                name: item.registry_key,
            });
        }
        next_id = next_id.checked_add(1).ok_or(InstallError::TooMany)?;
    }

    EXTENSION
        .set(Extension {
            items: entries,
            by_name,
        })
        .map_err(|_| InstallError::AlreadyInstalled)
}

/// Total number of items, vanilla plus extension.
#[must_use]
pub fn item_count() -> u16 {
    let extra = EXTENSION.get().map_or(0, |e| e.items.len());
    // `install` rejects anything that would not fit.
    Item::VANILLA_COUNT.saturating_add(extra.try_into().unwrap_or(u16::MAX))
}

/// Items of the extension, in raw-id order.
#[must_use]
pub fn items() -> &'static [&'static Item] {
    EXTENSION.get().map_or(&[], |e| e.items.as_slice())
}

// Off the vanilla path: only ids past the generated range get here.
#[cold]
#[inline(never)]
pub(crate) fn item_by_raw_id(raw: u16) -> Option<&'static Item> {
    let index = usize::from(raw.checked_sub(Item::VANILLA_COUNT)?);
    EXTENSION.get()?.items.get(index).copied()
}

// Off the vanilla path: only names the generated match misses get here.
#[cold]
#[inline(never)]
pub(crate) fn item_by_name(name: &str) -> Option<&'static Item> {
    EXTENSION.get()?.by_name.get(name).copied()
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use super::{install, item_count};
    use crate::data_component::DataComponent;
    use crate::data_component_impl::{DataComponentImpl, ItemNameImpl, MaxStackSizeImpl};
    use crate::item::Item;
    use crate::item_stack::ItemStack;

    fn leak_item(id: u16, key: &'static str, stack: u8) -> &'static Item {
        let components: Vec<(DataComponent, &'static dyn DataComponentImpl)> = vec![
            (
                DataComponent::MaxStackSize,
                Box::leak(Box::new(MaxStackSizeImpl { size: stack })),
            ),
            (
                DataComponent::ItemName,
                Box::leak(Box::new(ItemNameImpl {
                    name: Cow::Borrowed("item.pumpkinverse_test.token"),
                })),
            ),
        ];
        Box::leak(Box::new(Item {
            id,
            registry_key: key,
            components: Box::leak(components.into_boxed_slice()),
        }))
    }

    /// One test for the whole module: the extension is a process-wide
    /// `OnceLock`, so it can only be installed once per test binary.
    #[test]
    fn resolves_items_beyond_the_vanilla_range() {
        let vanilla = Item::VANILLA_COUNT;
        assert_eq!(Item::from_id(vanilla), None);

        let marker = leak_item(vanilla, "pumpkinverse_test:marker", 64);
        let token = leak_item(vanilla + 1, "pumpkinverse_test:token", 16);
        install(vec![marker, token]).expect("contiguous ids install");

        assert_eq!(item_count(), vanilla + 2);
        assert_eq!(Item::from_id(vanilla + 1), Some(token));
        assert_eq!(Item::from_id(vanilla + 2), None);
        assert_eq!(
            Item::from_registry_key("pumpkinverse_test:token"),
            Some(token)
        );
        assert_eq!(Item::from_registry_key("token"), None);
        // Vanilla lookups are unchanged.
        assert_eq!(
            Item::from_registry_key("minecraft:stone"),
            Some(&Item::STONE)
        );
        assert_eq!(Item::from_id(Item::STONE.id), Some(&Item::STONE));
        // Components are read through the same path as vanilla's.
        assert_eq!(ItemStack::new(1, token).get_max_stack_size(), 16);

        // A second install is refused.
        assert!(install(vec![]).is_err());
    }
}
