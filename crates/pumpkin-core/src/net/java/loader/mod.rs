//! Fork-only glue that lets modded clients talk to Pumpkin.
//!
//! Nothing in this module is meant for upstream. It reimplements the protocol
//! conventions of individual mod loaders, which a server aiming at vanilla
//! parity has no reason to carry. The primitives these modules need from the
//! core — an extensible registry, a configuration-phase hook — are generic and
//! belong upstream; the loader-specific encoding does not.

pub mod fabric;
