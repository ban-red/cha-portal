//! Turns the ids in `web/packages/ui-spec/icons.json` into the `Icon` enum, so a
//! misspelt icon in Rust is a compile error; and the limits of `prefs.json`'s int fields into
//! constants (`prefs::limits`).

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    prefs_limits();
    icon_enum();
}

fn icon_enum() {
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

/// `STATS_PANEL_OPACITY_MIN: u8 = 30;` for every int field with a limit.
fn prefs_limits() {
    let json = PathBuf::from("../../web/packages/ui-spec/prefs.json");
    println!("cargo:rerun-if-changed={}", json.display());
    let text = std::fs::read_to_string(&json).expect("web/packages/ui-spec/prefs.json");
    let spec: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&text).expect("prefs.json is a JSON object");
    let mut out = String::new();
    for (group, g) in &spec {
        let Some(fields) = g.get("fields").and_then(|f| f.as_object()) else {
            continue;
        };
        for (name, field) in fields {
            if field.get("type").and_then(|t| t.as_str()) != Some("int") {
                continue;
            }
            for limit in ["min", "max"] {
                if let Some(n) = field.get(limit).and_then(|n| n.as_u64()) {
                    let ident = format!("{group}_{name}_{limit}").to_ascii_uppercase();
                    let n = u8::try_from(n).expect("an int limit fits u8");
                    writeln!(out, "pub const {ident}: u8 = {n};").unwrap();
                }
            }
        }
    }
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("prefs_limits.rs");
    std::fs::write(dest, out).unwrap();
}
