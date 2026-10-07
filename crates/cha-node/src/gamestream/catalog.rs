//! The apps a node offers Moonlight clients: the templates of the catalog this
//! agent was built with (`images/catalog.json`), the way Sunshine lists what it
//! can run. An app is a template, not an environment: launching it starts the
//! user's environment of it if none runs ([`super::directory`]).

use std::sync::OnceLock;

use serde::Deserialize;

/// The class of a template that streams another Moonlight host's app through a
/// gateway (the portal's `moonlight::CLASS`). Never an app of this host: it
/// would stream a stream.
const GATEWAY_CLASS: &str = "moonlight";

/// A catalog template, as far as the app list needs it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Template {
    pub id: String,
    pub name: String,
    class: String,
    /// It never runs on the CPU, so it needs a GPU on this node.
    #[serde(default)]
    needs_gpu: bool,
}

impl Template {
    /// Whether an environment of this template can run on a node with (or
    /// without, `gpu: false`) a GPU. The portal's placement has the last word
    /// (which device, how loaded); this only keeps the list honest.
    pub fn runs_on(&self, gpu: bool) -> bool {
        self.class != GATEWAY_CLASS && (gpu || !self.needs_gpu)
    }
}

#[derive(Deserialize)]
struct Catalog {
    templates: Vec<Template>,
}

/// Every template of the built-in catalog.
pub fn templates() -> &'static [Template] {
    static CATALOG: OnceLock<Vec<Template>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        serde_json::from_str::<Catalog>(include_str!("../../../../images/catalog.json"))
            .map(|c| c.templates)
            .unwrap_or_default()
    })
}

/// The templates this node can offer: the catalog's, less gateways and what
/// needs a GPU the node hasn't.
pub fn offered(gpu: bool) -> impl Iterator<Item = &'static Template> {
    templates().iter().filter(move |t| t.runs_on(gpu))
}

/// A template's name, or its id when the catalog doesn't know it.
pub fn name(template: &str) -> String {
    templates()
        .iter()
        .find(|t| t.id == template)
        .map_or_else(|| template.to_string(), |t| t.name.clone())
}

/// The app id Moonlight sees for a template: FNV-1a (32 bit) of its id with
/// the top bit cleared, so it is positive as clients expect, and never 0,
/// which they take to mean "nothing". It depends on nothing but the id, so an
/// app keeps its id across restarts, environments and nodes, and a client's
/// remembered apps stay put.
pub fn app_id(template: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in template.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    (hash & 0x7fff_ffff).max(1)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn ids(gpu: bool) -> Vec<&'static str> {
        offered(gpu).map(|t| t.id.as_str()).collect()
    }

    #[test]
    fn the_catalog_parses_and_names_its_templates() {
        assert!(templates().len() >= 5);
        assert_eq!(name("chrome"), "Google Chrome");
        assert_eq!(name("not-in-the-catalog"), "not-in-the-catalog");
    }

    #[test]
    fn what_needs_a_gpu_is_offered_only_on_a_node_with_one() {
        let with = ids(true);
        let without = ids(false);
        assert!(with.contains(&"steam") && !without.contains(&"steam"));
        for plain in ["chrome", "firefox", "xfce"] {
            assert!(with.contains(&plain) && without.contains(&plain), "{plain}");
        }
    }

    #[test]
    fn gateway_templates_are_never_offered() {
        let gateway = Template {
            id: "moonlight:abc:1".into(),
            name: "Elsewhere".into(),
            class: GATEWAY_CLASS.into(),
            needs_gpu: false,
        };
        assert!(!gateway.runs_on(true) && !gateway.runs_on(false));
        assert!(templates().iter().all(|t| t.class != GATEWAY_CLASS));
    }

    #[test]
    fn app_ids_are_a_fixed_function_of_the_template() {
        // Pinned: clients remember apps by id, so the function mustn't drift.
        assert_eq!(app_id(""), 0x011c_9dc5);
        assert_eq!(app_id("a"), 0xe40c_292c & 0x7fff_ffff);
        assert_eq!(app_id("chrome"), app_id("chrome"));
        // Positive, non-zero, and distinct across the catalog.
        let all: Vec<u32> = templates().iter().map(|t| app_id(&t.id)).collect();
        assert!(all.iter().all(|&i| (1..=i32::MAX as u32).contains(&i)));
        assert_eq!(
            all.iter().collect::<HashSet<_>>().len(),
            all.len(),
            "two templates share an app id"
        );
    }
}
