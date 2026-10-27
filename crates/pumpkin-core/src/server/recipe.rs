use std::sync::RwLock;

use pumpkin_data::item_stack::ItemStack;
use pumpkin_inventory::crafting::recipe_provider::RecipeProvider;
use pumpkin_inventory::crafting::recipes::RecipeInputInventory;
pub use pumpkin_protocol::codec::recipe::DynamicRecipe;

/// A crafting recipe matched by code: the result for a grid, if it matches.
pub type SpecialCraftingRecipe =
    Box<dyn Fn(&dyn RecipeInputInventory) -> Option<ItemStack> + Send + Sync>;

pub struct RecipeManager {
    dynamic_recipes: RwLock<Vec<DynamicRecipe>>,
    /// Kept across datapack reloads, which replace only the data recipes.
    special_crafting: RwLock<Vec<SpecialCraftingRecipe>>,
}

impl Default for RecipeManager {
    fn default() -> Self {
        Self::new()
    }
}

impl RecipeManager {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            dynamic_recipes: RwLock::new(Vec::new()),
            special_crafting: RwLock::new(Vec::new()),
        }
    }

    /// Adds a crafting recipe matched by code, consulted after the data recipes.
    pub fn add_special_crafting(&self, recipe: SpecialCraftingRecipe) {
        self.special_crafting
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(recipe);
    }

    pub fn add_recipe(&self, recipe: DynamicRecipe) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recipes.push(recipe);
    }

    pub fn add_recipes(&self, new_recipes: impl IntoIterator<Item = DynamicRecipe>) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recipes.extend(new_recipes);
    }

    pub fn set_recipes(&self, new_recipes: Vec<DynamicRecipe>) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *recipes = new_recipes;
    }

    pub fn clear(&self) {
        let mut recipes = self
            .dynamic_recipes
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        recipes.clear();
    }

    pub fn get_dynamic_recipes_internal(&self) -> Vec<DynamicRecipe> {
        self.dynamic_recipes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

}

impl RecipeProvider for RecipeManager {
    fn get_dynamic_recipes(&self) -> Vec<DynamicRecipe> {
        self.dynamic_recipes
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn match_special_crafting(&self, inventory: &dyn RecipeInputInventory) -> Option<ItemStack> {
        self.special_crafting
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find_map(|recipe| recipe(inventory))
    }
}
