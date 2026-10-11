#![allow(
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::cargo,
    clippy::print_stdout
)]

//! Measures the vanilla lookup path after the runtime block extension replaced
//! `assert_unchecked` with a real bound. Only vanilla ids are exercised,
//! because that is the traffic chunk serialisation, lighting and redstone
//! actually produce, and the regression question is whether the added branch
//! costs anything there.

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use pumpkin_data::{Block, BlockId, BlockState, BlockStateId};

fn all_state_ids() -> Vec<BlockStateId> {
    (0..BlockStateId::COUNT)
        .map(BlockStateId::new_or_air)
        .collect()
}

fn all_block_ids() -> Vec<BlockId> {
    (0..BlockId::COUNT).map(BlockId::new_or_air).collect()
}

#[inline(never)]
fn run_block_from_state_id(ids: &[BlockStateId]) -> u32 {
    let mut acc = 0u32;
    for &id in ids {
        let block = Block::from_state_id(black_box(id));
        acc = acc.wrapping_add(u32::from(block.id.as_u16()));
    }
    black_box(acc)
}

#[inline(never)]
fn run_block_state_from_id(ids: &[BlockStateId]) -> u32 {
    let mut acc = 0u32;
    for &id in ids {
        let state = BlockState::from_id(black_box(id));
        acc = acc.wrapping_add(u32::from(state.opacity));
    }
    black_box(acc)
}

#[inline(never)]
fn run_block_from_id(ids: &[BlockId]) -> u32 {
    let mut acc = 0u32;
    for &id in ids {
        let block = Block::from_id(black_box(id));
        acc = acc.wrapping_add(u32::from(block.item_id));
    }
    black_box(acc)
}

pub fn bench_block_lookup(c: &mut Criterion) {
    let state_ids = all_state_ids();
    let block_ids = all_block_ids();

    c.bench_function("block_lookup/block_from_state_id", |b| {
        b.iter(|| run_block_from_state_id(&state_ids))
    });

    c.bench_function("block_lookup/block_state_from_id", |b| {
        b.iter(|| run_block_state_from_id(&state_ids))
    });

    c.bench_function("block_lookup/block_from_id", |b| {
        b.iter(|| run_block_from_id(&block_ids))
    });
}

criterion_group!(benches, bench_block_lookup);
criterion_main!(benches);
