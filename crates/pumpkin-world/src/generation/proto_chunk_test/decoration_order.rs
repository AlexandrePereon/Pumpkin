//! Decorates a region of chunks in a given order, one chunk at a time, to tell an
//! order-dependent difference from an algorithmic one.
//!
//! Vanilla's `FEATURES` step writes into the 3×3 chunks around the one it decorates, and its
//! trees and ore blobs read and overwrite what the neighbours already placed. Which chunk of
//! two neighbours is decorated first therefore changes the result, and that order follows
//! thread timing on both servers. This test takes an order (one `x z` chunk per line, as a
//! server logged it), brings those chunks and their neighbours to carvers, decorates them in
//! that order and writes each listed chunk's blocks to a dump, to compare with the server's:
//! `VTER`, a little-endian `u32` chunk count, then per chunk its `i32` x and z and its block
//! state ids as `u16`, y-major from the bottom, then z, then x.
//!
//! `DECORATION_ORDER=<file> DECORATION_SEED=<seed> DECORATION_OUT=<dump>
//!  cargo test -p pumpkin-world --lib decoration_order -- --ignored`

use std::collections::HashMap;
use std::io::Write;

use pumpkin_data::dimension::Dimension;
use pumpkin_util::world_seed::Seed;

use super::stage_parity::{TestBlockRegistry, TestGridCache};
use super::surface_biomes;
use crate::generation::{generator::WorldGenerator, get_world_gen, proto_chunk::ProtoChunk};

#[test]
#[ignore = "a tool: needs DECORATION_ORDER, DECORATION_SEED and DECORATION_OUT"]
fn decoration_order_replay() {
    let (Ok(order), Ok(seed), Ok(out)) = (
        std::env::var("DECORATION_ORDER"),
        std::env::var("DECORATION_SEED"),
        std::env::var("DECORATION_OUT"),
    ) else {
        return;
    };
    let order: Vec<(i32, i32)> = std::fs::read_to_string(order)
        .unwrap()
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace().map(str::parse::<i32>);
            Some((parts.next()?.ok()?, parts.next()?.ok()?))
        })
        .collect();
    let seed: u64 = seed.parse().unwrap();
    let world_gen = get_world_gen(
        Seed(seed),
        Dimension::OVERWORLD,
        false,
        Vec::new(),
        String::new(),
    );
    let WorldGenerator::Noise(generator) = &*world_gen else {
        unreachable!()
    };
    let registry = TestBlockRegistry;

    let (mut min_x, mut max_x, mut min_z, mut max_z) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for &(x, z) in &order {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_z = min_z.min(z);
        max_z = max_z.max(z);
    }
    // Structure starts within 8 of every decorated chunk, terrain within 1.
    let mut chunks = HashMap::new();
    for x in min_x - 9..=max_x + 9 {
        for z in min_z - 9..=max_z + 9 {
            let mut chunk = ProtoChunk::new(x, z, &world_gen);
            chunk.step_to_biomes(generator);
            chunk.set_structure_starts(generator);
            chunk.set_structure_references(generator);
            chunks.insert((x, z), chunk);
        }
    }
    for x in min_x - 1..=max_x + 1 {
        for z in min_z - 1..=max_z + 1 {
            let chunk = chunks.get_mut(&(x, z)).unwrap();
            chunk.step_to_noise(generator);
            chunk.step_to_surface(generator, &surface_biomes(&world_gen, x, z));
            chunk.step_to_carvers(generator);
        }
    }
    let mut cache = TestGridCache {
        center_pos: (0, 0),
        chunks,
    };
    for &pos in &order {
        cache.center_pos = pos;
        ProtoChunk::generate_features_and_structure(
            &mut cache,
            &registry,
            &generator.random_config,
        );
    }

    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"VTER");
    bytes.extend_from_slice(&u32::try_from(order.len()).unwrap().to_le_bytes());
    for pos in &order {
        let chunk = &cache.chunks[pos];
        bytes.extend_from_slice(&pos.0.to_le_bytes());
        bytes.extend_from_slice(&pos.1.to_le_bytes());
        for y in 0..384 {
            for z in 0..16 {
                for x in 0..16 {
                    let state = chunk.get_block_state_raw(x, y, z);
                    bytes.extend_from_slice(&state.as_u16().to_le_bytes());
                }
            }
        }
    }
    std::fs::File::create(out)
        .unwrap()
        .write_all(&bytes)
        .unwrap();
}
