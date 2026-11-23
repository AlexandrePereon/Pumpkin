pub mod biome_datapack;
pub mod configured_features;
pub mod datapack;
pub mod extension;
// So first we go trough all the placed features and check if we should place a feature
// somewhere using `placed_features`. Then if we want to place a feature we place it
// using the `configured_features`, there is the logic for how we are going to place the
// feature.
pub mod placed_features;

pub mod features;
mod size;

use std::cell::Cell;

thread_local! {
    /// The seed of the world whose chunk is being decorated, for the features that read
    /// vanilla's `WorldGenLevel.getSeed()` (the geode's noise).
    static DECORATED_WORLD_SEED: Cell<u64> = const { Cell::new(0) };
    /// The id of the dimension whose chunk is being decorated (`minecraft:overworld`…), for
    /// the features of registered types that read vanilla's `ServerLevel.dimension()`.
    static DECORATED_DIMENSION: Cell<&'static str> = const { Cell::new("") };
}

/// Runs `f`, the decoration of a chunk of the world with this seed.
pub fn decorating_world<R>(seed: u64, f: impl FnOnce() -> R) -> R {
    let previous = DECORATED_WORLD_SEED.replace(seed);
    let result = f();
    DECORATED_WORLD_SEED.set(previous);
    result
}

/// The seed of the world being decorated (see [`decorating_world`]).
#[must_use]
pub fn decorated_world_seed() -> u64 {
    DECORATED_WORLD_SEED.get()
}

/// Runs `f`, the decoration of a chunk of the dimension with this id.
pub fn decorating_dimension<R>(dimension: &'static str, f: impl FnOnce() -> R) -> R {
    let previous = DECORATED_DIMENSION.replace(dimension);
    let result = f();
    DECORATED_DIMENSION.set(previous);
    result
}

/// The id of the dimension being decorated (see [`decorating_dimension`]); empty outside a
/// decoration.
#[must_use]
pub fn decorated_dimension() -> &'static str {
    DECORATED_DIMENSION.get()
}
