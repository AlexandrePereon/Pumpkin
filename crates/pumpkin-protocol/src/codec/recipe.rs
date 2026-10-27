use pumpkin_data::recipes::RecipeCategoryTypes;

use pumpkin_data::item::Item;
use pumpkin_data::tag::Taggable;

#[derive(Clone, Debug)]
pub enum OwnedRecipeIngredient {
    Simple(String),
    Tagged(String),
    OneOf(Vec<String>),
}

impl OwnedRecipeIngredient {
    #[must_use]
    pub fn match_item(&self, item: &Item) -> bool {
        match self {
            Self::Simple(id) => names_item(id, item),
            Self::Tagged(tag) => item.is_tagged_with(tag).unwrap_or(false),
            Self::OneOf(ids) => ids.iter().any(|id| names_item(id, item)),
        }
    }
}

/// Whether a namespaced item id names `item`. Vanilla items keep a bare path as
/// their registry key; items added at runtime keep their full `namespace:path`.
fn names_item(id: &str, item: &Item) -> bool {
    id.strip_prefix("minecraft:").unwrap_or(id) == item.registry_key
}

#[derive(Clone, Debug)]
pub struct OwnedRecipeResult {
    pub item_id: String,
    pub count: u8,
    // TODO: Add components/enchantments if needed for the display result
}

#[derive(Clone, Debug)]
pub enum OwnedCraftingRecipe {
    Shaped {
        recipe_id: Option<String>,
        category: RecipeCategoryTypes,
        group: Option<String>,
        show_notification: bool,
        key: Vec<(char, OwnedRecipeIngredient)>,
        pattern: Vec<String>,
        result: OwnedRecipeResult,
    },
    Shapeless {
        recipe_id: Option<String>,
        category: RecipeCategoryTypes,
        group: Option<String>,
        ingredients: Vec<OwnedRecipeIngredient>,
        result: OwnedRecipeResult,
    },
}

#[derive(Clone, Debug)]
pub struct OwnedCookingRecipe {
    pub recipe_id: String,
    pub category: RecipeCategoryTypes,
    pub group: Option<String>,
    pub ingredient: OwnedRecipeIngredient,
    pub cooking_time: i32,
    pub experience: f32,
    pub result: OwnedRecipeResult,
}

#[derive(Clone, Debug)]
pub enum OwnedCookingRecipeType {
    Blasting(OwnedCookingRecipe),
    Smelting(OwnedCookingRecipe),
    Smoking(OwnedCookingRecipe),
    CampfireCooking(OwnedCookingRecipe),
}

#[derive(Clone, Debug)]
pub struct OwnedBrewingRecipe {
    pub recipe_id: String,
    pub input_item: String,
    pub input_potion: Option<String>,
    pub reagent: String,
    pub output_item: String,
    pub output_potion: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OwnedStonecutterRecipe {
    pub recipe_id: String,
    pub group: Option<String>,
    pub ingredient: OwnedRecipeIngredient,
    pub result: OwnedRecipeResult,
}

#[derive(Clone, Debug)]
pub struct OwnedSmithingTransformRecipe {
    pub recipe_id: String,
    pub template: OwnedRecipeIngredient,
    pub base: OwnedRecipeIngredient,
    pub addition: OwnedRecipeIngredient,
    pub result: OwnedRecipeResult,
}

#[derive(Clone, Debug)]
pub struct OwnedSmithingTrimRecipe {
    pub recipe_id: String,
    pub template: OwnedRecipeIngredient,
    pub base: OwnedRecipeIngredient,
    pub addition: OwnedRecipeIngredient,
    pub pattern: String,
}

#[derive(Clone, Debug)]
pub struct OwnedSpecialCraftingRecipe {
    pub recipe_id: String,
    pub category: RecipeCategoryTypes,
}

#[derive(Clone, Debug)]
pub enum DynamicRecipe {
    Crafting(OwnedCraftingRecipe),
    Cooking(OwnedCookingRecipeType),
    Brewing(OwnedBrewingRecipe),
    Stonecutting(OwnedStonecutterRecipe),
    SmithingTransform(OwnedSmithingTransformRecipe),
    SmithingTrim(OwnedSmithingTrimRecipe),
    SpecialCrafting(OwnedSpecialCraftingRecipe),
}

impl DynamicRecipe {
    #[must_use]
    pub fn recipe_id(&self) -> Option<&str> {
        match self {
            Self::Crafting(
                OwnedCraftingRecipe::Shaped { recipe_id, .. }
                | OwnedCraftingRecipe::Shapeless { recipe_id, .. },
            ) => recipe_id.as_deref(),
            Self::Cooking(
                OwnedCookingRecipeType::Blasting(r)
                | OwnedCookingRecipeType::Smelting(r)
                | OwnedCookingRecipeType::Smoking(r)
                | OwnedCookingRecipeType::CampfireCooking(r),
            ) => Some(&r.recipe_id),
            Self::Brewing(r) => Some(&r.recipe_id),
            Self::Stonecutting(r) => Some(&r.recipe_id),
            Self::SmithingTransform(r) => Some(&r.recipe_id),
            Self::SmithingTrim(r) => Some(&r.recipe_id),
            Self::SpecialCrafting(r) => Some(&r.recipe_id),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingredients_match_vanilla_and_namespaced_items() {
        let stick = OwnedRecipeIngredient::Simple("minecraft:stick".to_string());
        assert!(stick.match_item(&Item::STICK));
        assert!(!stick.match_item(&Item::STONE));

        let marker = Item {
            id: u16::MAX,
            registry_key: "pumpkinverse_test:marker",
            components: &[],
        };
        let by_mod_id = OwnedRecipeIngredient::Simple("pumpkinverse_test:marker".to_string());
        assert!(by_mod_id.match_item(&marker));
        assert!(!by_mod_id.match_item(&Item::STICK));
        assert!(!stick.match_item(&marker));

        let either = OwnedRecipeIngredient::OneOf(vec![
            "minecraft:stick".to_string(),
            "pumpkinverse_test:marker".to_string(),
        ]);
        assert!(either.match_item(&Item::STICK));
        assert!(either.match_item(&marker));
        assert!(!either.match_item(&Item::STONE));
    }
}
