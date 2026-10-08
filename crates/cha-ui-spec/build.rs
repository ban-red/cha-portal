//! Turns the ids in `web/packages/ui-spec/icons.json` into the `Icon` enum, so a
//! misspelt icon in Rust is a compile error.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let json = PathBuf::from("../../web/packages/ui-spec/icons.json");
    println!("cargo:rerun-if-changed={}", json.display());
    let text = std::fs::read_to_string(&json).expect("web/packages/ui-spec/icons.json");
    let icons: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).expect("icons.json is a JSON object");

    let mut ids: Vec<(String, String)> = icons
        .keys()
        .map(|id| {
            let variant: String = id
                .split('-')
                .map(|w| {
                    let mut c = w.chars();
                    c.next()
                        .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                        .unwrap_or_default()
                })
                .collect();
            assert!(
                !variant.is_empty() && variant.chars().all(|c| c.is_ascii_alphanumeric()),
                "icon id {id:?} is not kebab-case ascii"
            );
            (id.clone(), variant)
        })
        .collect();
    ids.sort();

    let mut out = String::new();
    out.push_str("/// Every icon in `icons.json`, by id (`sound-muted` is `Icon::SoundMuted`).\n");
    out.push_str("#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]\n");
    out.push_str("pub enum Icon {\n");
    for (_, v) in &ids {
        writeln!(out, "    {v},").unwrap();
    }
    out.push_str("}\n\nimpl Icon {\n    pub const ALL: [Icon; ");
    write!(out, "{}] = [", ids.len()).unwrap();
    for (_, v) in &ids {
        write!(out, "Icon::{v}, ").unwrap();
    }
    out.push_str("];\n\n    /// The kebab-case id used in `icons.json`.\n");
    out.push_str("    pub const fn id(self) -> &'static str {\n        match self {\n");
    for (id, v) in &ids {
        writeln!(out, "            Icon::{v} => {id:?},").unwrap();
    }
    out.push_str("        }\n    }\n}\n");

    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("icon_enum.rs");
    std::fs::write(dest, out).unwrap();
}
