use std::{fs, path::Path};

use proc_macro2::TokenStream;
use quote::quote;

/// The vanilla worldgen files a datapack can build on at runtime, embedded as JSON text.
const REGISTRIES: [(&str, &str); 6] = [
    ("DENSITY_FUNCTIONS", "density_function"),
    ("NOISES", "noise"),
    ("NOISE_SETTINGS", "noise_settings"),
    ("MATERIAL_RULES", "material_rule"),
    ("MATERIAL_CONDITIONS", "material_condition"),
    ("BLOCK_STATE_PROVIDERS", "block_state_provider"),
];

fn collect(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(base, &path, out);
        } else if path.extension().is_some_and(|ext| ext == "json") {
            let rel = path
                .strip_prefix(base)
                .expect("worldgen file outside its registry folder")
                .with_extension("");
            let rel = rel
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            out.push(rel);
        }
    }
}

/// Embeds the vanilla `density_function`, `noise`, `noise_settings`, `material_rule`,
/// `material_condition` and `block_state_provider` JSON, so that a datapack overriding one of them can be resolved
/// against the rest at runtime.
pub fn build() -> TokenStream {
    let worldgen = Path::new("../../assets/datapack/data/minecraft/worldgen");
    let tables = REGISTRIES.iter().map(|(name, folder)| {
        let base = worldgen.join(folder);
        let mut files = Vec::new();
        collect(&base, &base, &mut files);
        files.sort();
        let entries = files.iter().map(|rel| {
            let id = format!("minecraft:{rel}");
            // Relative to `crates/pumpkin-data/src/generated/`.
            let path =
                format!("../../../../assets/datapack/data/minecraft/worldgen/{folder}/{rel}.json");
            quote! { (#id, include_str!(#path)) }
        });
        let ident = quote::format_ident!("{}", name);
        quote! {
            pub const #ident: &[(&str, &str)] = &[#(#entries),*];
        }
    });
    quote! {
        #(#tables)*
    }
}
