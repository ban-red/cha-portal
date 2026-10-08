//! The themes available: the built-ins embedded in the binary, then the
//! user's own from `<data dir>/themes/*.json`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

use super::fonts::FontChoice;
use super::metrics::{Metrics, MetricsOverride};
use super::palette::{Palette, PaletteOverride};
use super::resolve::Variant;

pub const DEFAULT_THEME: &str = "cha-magenta";

/// Generated from the portal's CSS by `scripts/export-player-themes.ts`; compiled in
/// by `cha-ui-spec` from `web/packages/ui-spec/themes/`.
const BUILTIN: [(&str, &str); 2] = cha_ui_spec::THEMES;

/// A theme fully resolved: all four palettes, metrics, fonts.
#[derive(Clone, Debug)]
pub struct Theme {
    pub id: String,
    pub name: String,
    /// In [`Variant::ALL`] order.
    palettes: [Palette; 4],
    pub metrics: Metrics,
    pub fonts: FontChoice,
    pub builtin: bool,
}

impl Theme {
    pub fn palette(&self, variant: Variant) -> &Palette {
        &self.palettes[variant as usize]
    }
}

/// A theme file that was skipped, and why.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub file: String,
    pub reason: String,
}

#[derive(Debug)]
pub struct Registry {
    themes: Vec<Arc<Theme>>,
    default: Arc<Theme>,
    issues: Vec<Issue>,
}

// The files.

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct VariantsFile {
    dark: Option<PaletteOverride>,
    light: Option<PaletteOverride>,
    dark_more: Option<PaletteOverride>,
    light_more: Option<PaletteOverride>,
}

impl VariantsFile {
    fn get(&self, v: Variant) -> Option<&PaletteOverride> {
        match v {
            Variant::Dark => self.dark.as_ref(),
            Variant::Light => self.light.as_ref(),
            Variant::DarkMore => self.dark_more.as_ref(),
            Variant::LightMore => self.light_more.as_ref(),
        }
    }

    /// The same appearance without the contrast boost.
    fn normal_of(&self, v: Variant) -> Option<&PaletteOverride> {
        match v {
            Variant::DarkMore => self.dark.as_ref(),
            Variant::LightMore => self.light.as_ref(),
            _ => None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct FontsFile {
    proportional: Option<String>,
    monospace: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    id: String,
    name: Option<String>,
    extends: Option<String>,
    /// Written by the exporter; not used.
    #[allow(dead_code)]
    generated: Option<String>,
    #[serde(default)]
    variants: VariantsFile,
    #[serde(default)]
    metrics: MetricsOverride,
    #[serde(default)]
    fonts: FontsFile,
}

impl ThemeFile {
    fn parse(text: &str) -> Result<Self, String> {
        let file: ThemeFile = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let id = file.id.trim();
        if id.is_empty() || id.len() > 64 || id.chars().any(char::is_control) {
            return Err("\"id\" must be 1 to 64 characters".into());
        }
        Ok(file)
    }

    fn id(&self) -> &str {
        self.id.trim()
    }
}

/// Make a theme from a file over `base` (a built-in has none and must say
/// everything; `dark-more` and `light-more` then default to the plain
/// variant).
fn build(
    file: &ThemeFile,
    base: Option<&Theme>,
    dir: Option<&Path>,
    builtin: bool,
) -> Result<Theme, String> {
    let mut palettes = Vec::with_capacity(4);
    for v in Variant::ALL {
        let palette = match base {
            Some(base) => {
                let mut p = base.palette(v).clone();
                // A change to "dark" reaches "dark-more" too, unless that
                // says otherwise.
                if let Some(ov) = file.variants.normal_of(v) {
                    ov.apply_to(&mut p);
                }
                if let Some(ov) = file.variants.get(v) {
                    ov.apply_to(&mut p);
                }
                p
            }
            None => {
                let own = file
                    .variants
                    .get(v)
                    .or_else(|| file.variants.normal_of(v))
                    .ok_or_else(|| format!("no \"{}\" variant", variant_key(v)))?;
                own.complete().map_err(|missing| {
                    format!(
                        "variant \"{}\" lacks roles: {}",
                        variant_key(v),
                        missing.join(", ")
                    )
                })?
            }
        };
        palettes.push(palette);
    }
    let palettes: [Palette; 4] = palettes
        .try_into()
        .map_err(|_| "internal: variant count".to_string())?;

    let mut metrics = base.map(|b| b.metrics.clone()).unwrap_or_default();
    file.metrics.apply_to(&mut metrics);

    let mut fonts = base.map(|b| b.fonts.clone()).unwrap_or_default();
    let resolve_path = |name: &str| -> PathBuf {
        let p = PathBuf::from(name);
        match dir {
            Some(dir) if p.is_relative() => dir.join(p),
            _ => p,
        }
    };
    if let Some(name) = &file.fonts.proportional {
        fonts.proportional = Some(resolve_path(name));
    }
    if let Some(name) = &file.fonts.monospace {
        fonts.monospace = Some(resolve_path(name));
    }

    let id = file.id().to_string();
    Ok(Theme {
        name: file.name.clone().unwrap_or_else(|| id.clone()),
        id,
        palettes,
        metrics: metrics.sanitized(),
        fonts,
        builtin,
    })
}

fn variant_key(v: Variant) -> &'static str {
    match v {
        Variant::Dark => "dark",
        Variant::Light => "light",
        Variant::DarkMore => "dark-more",
        Variant::LightMore => "light-more",
    }
}

fn load_builtins() -> Vec<Arc<Theme>> {
    BUILTIN
        .iter()
        .map(|(id, text)| {
            let theme = ThemeFile::parse(text)
                .and_then(|f| build(&f, None, None, true))
                .unwrap_or_else(|e| panic!("built-in theme {id} is invalid: {e}"));
            Arc::new(theme)
        })
        .collect()
}

struct UserFile {
    name: String,
    file: ThemeFile,
    dir: PathBuf,
}

impl Registry {
    /// Only the built-ins.
    pub fn builtin() -> Self {
        let themes = load_builtins();
        Self::assemble(themes, Vec::new())
    }

    /// The built-ins and the user's themes in `<data_dir>/themes`.
    pub fn load(data_dir: &Path) -> Self {
        Self::from_dir(&data_dir.join("themes"))
    }

    /// The built-ins and every `*.json` in `dir`. A missing directory is not
    /// an issue; a bad file is one, and is skipped.
    pub fn from_dir(dir: &Path) -> Self {
        let builtins = load_builtins();
        let mut issues = Vec::new();

        let mut names: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.extension()
                        .is_some_and(|x| x.eq_ignore_ascii_case("json"))
                })
                .collect(),
            Err(e) => {
                if dir.exists() {
                    issues.push(Issue {
                        file: dir.display().to_string(),
                        reason: format!("cannot read the folder: {e}"),
                    });
                }
                Vec::new()
            }
        };
        names.sort();

        let mut users: HashMap<String, UserFile> = HashMap::new();
        let mut order = Vec::new();
        for path in names {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let parsed = std::fs::read_to_string(&path)
                .map_err(|e| e.to_string())
                .and_then(|t| ThemeFile::parse(&t));
            match parsed {
                Ok(file) => {
                    let id = file.id().to_string();
                    if let Some(first) = users.get(&id) {
                        issues.push(Issue {
                            file: name,
                            reason: format!("id \"{id}\" is already used by {}", first.name),
                        });
                        continue;
                    }
                    order.push(id.clone());
                    users.insert(
                        id,
                        UserFile {
                            name,
                            file,
                            dir: path.parent().map(Path::to_path_buf).unwrap_or_default(),
                        },
                    );
                }
                Err(reason) => issues.push(Issue { file: name, reason }),
            }
        }

        let builtin_by_id: HashMap<&str, &Arc<Theme>> =
            builtins.iter().map(|t| (t.id.as_str(), t)).collect();
        let mut done: HashMap<String, Arc<Theme>> = HashMap::new();
        let mut failed: HashMap<String, String> = HashMap::new();
        for id in &order {
            let mut stack = Vec::new();
            if let Err(reason) = resolve_user(
                id,
                &users,
                &builtin_by_id,
                &mut done,
                &mut failed,
                &mut stack,
            ) {
                issues.push(Issue {
                    file: users[id].name.clone(),
                    reason,
                });
            }
        }

        // Built-ins in their order (a user theme with the same id replaces
        // one in place), then the other user themes by name.
        let mut themes: Vec<Arc<Theme>> = builtins
            .iter()
            .map(|b| done.get(&b.id).cloned().unwrap_or_else(|| b.clone()))
            .collect();
        let mut extra: Vec<Arc<Theme>> = order
            .iter()
            .filter(|id| !builtin_by_id.contains_key(id.as_str()))
            .filter_map(|id| done.get(id).cloned())
            .collect();
        extra.sort_by_key(|t| t.name.to_lowercase());
        themes.extend(extra);

        Self::assemble(themes, issues)
    }

    fn assemble(themes: Vec<Arc<Theme>>, issues: Vec<Issue>) -> Self {
        // The default is always the built-in one, even if a user theme with
        // its id replaces it in the list: it is what a broken pick falls to.
        let default = load_builtins()
            .into_iter()
            .find(|t| t.id == DEFAULT_THEME)
            .expect("the default theme is built in");
        Self {
            themes,
            default,
            issues,
        }
    }

    pub fn themes(&self) -> &[Arc<Theme>] {
        &self.themes
    }

    pub fn get(&self, id: &str) -> Option<&Arc<Theme>> {
        self.themes.iter().find(|t| t.id == id)
    }

    pub fn default_theme(&self) -> &Arc<Theme> {
        self.get(DEFAULT_THEME).unwrap_or(&self.default)
    }

    /// Files that were skipped, and why.
    pub fn issues(&self) -> &[Issue] {
        &self.issues
    }
}

fn resolve_user(
    id: &str,
    users: &HashMap<String, UserFile>,
    builtins: &HashMap<&str, &Arc<Theme>>,
    done: &mut HashMap<String, Arc<Theme>>,
    failed: &mut HashMap<String, String>,
    stack: &mut Vec<String>,
) -> Result<Arc<Theme>, String> {
    if let Some(t) = done.get(id) {
        return Ok(t.clone());
    }
    if let Some(reason) = failed.get(id) {
        return Err(reason.clone());
    }
    let user = &users[id];
    stack.push(id.to_string());
    let result = (|| {
        let extends = user
            .file
            .extends
            .as_deref()
            .map(str::trim)
            .unwrap_or(DEFAULT_THEME);
        // A theme extending its own id extends the built-in it replaces.
        let base: Arc<Theme> = if extends == id {
            builtins
                .get(extends)
                .map(|t| (*t).clone())
                .ok_or_else(|| format!("extends \"{extends}\", which is itself"))?
        } else if users.contains_key(extends) {
            if stack.iter().any(|s| s == extends) {
                return Err(format!("extends loop: {} -> {extends}", stack.join(" -> ")));
            }
            resolve_user(extends, users, builtins, done, failed, stack)
                .map_err(|e| format!("extends \"{extends}\", which is invalid: {e}"))?
        } else if let Some(t) = builtins.get(extends) {
            (*t).clone()
        } else {
            return Err(format!("extends \"{extends}\", which is not a theme"));
        };
        build(&user.file, Some(&base), Some(&user.dir), false).map(Arc::new)
    })();
    stack.pop();
    match &result {
        Ok(t) => {
            done.insert(id.to_string(), t.clone());
        }
        Err(e) => {
            failed.insert(id.to_string(), e.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::Color32;

    fn tmp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("cha-player-themes-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn built_ins_have_every_role_in_all_four_variants() {
        for (id, text) in BUILTIN {
            // Each variant must be written out, not defaulted: the exporter
            // is meant to emit all four.
            let file = ThemeFile::parse(text).unwrap();
            assert_eq!(file.id, id);
            for v in Variant::ALL {
                let own = file
                    .variants
                    .get(v)
                    .unwrap_or_else(|| panic!("{id} lacks {v:?}"));
                own.complete()
                    .unwrap_or_else(|m| panic!("{id} {v:?} lacks {m:?}"));
            }
            build(&file, None, None, true).unwrap();
        }
        let registry = Registry::builtin();
        assert_eq!(registry.themes().len(), BUILTIN.len());
        assert_eq!(registry.default_theme().id, DEFAULT_THEME);
        assert!(registry.issues().is_empty());
    }

    #[test]
    fn built_in_missing_more_variants_fall_back_to_plain() {
        let mut file = ThemeFile::parse(BUILTIN[0].1).unwrap();
        file.variants.dark_more = None;
        file.variants.light_more = None;
        let theme = build(&file, None, None, true).unwrap();
        assert_eq!(
            theme.palette(Variant::DarkMore),
            theme.palette(Variant::Dark)
        );
        assert_eq!(
            theme.palette(Variant::LightMore),
            theme.palette(Variant::Light)
        );
    }

    #[test]
    fn user_theme_extends_the_default_and_carries_overrides_into_more() {
        let dir = tmp("extends");
        std::fs::write(
            dir.join("mine.json"),
            r##"{ "id": "mine", "name": "Mine",
                  "variants": { "dark": { "accent": "#00ff88" } },
                  "metrics": { "radius_medium": 2.0 } }"##,
        )
        .unwrap();
        let registry = Registry::from_dir(&dir);
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let mine = registry.get("mine").unwrap();
        let base = registry.get("cha-magenta").unwrap();
        assert_eq!(mine.name, "Mine");
        assert_eq!(
            mine.palette(Variant::Dark).accent,
            Color32::from_rgb(0, 255, 136)
        );
        assert_eq!(
            mine.palette(Variant::DarkMore).accent,
            Color32::from_rgb(0, 255, 136)
        );
        // Untouched roles and variants come from the base.
        assert_eq!(
            mine.palette(Variant::Dark).canvas,
            base.palette(Variant::Dark).canvas
        );
        assert_eq!(mine.palette(Variant::Light), base.palette(Variant::Light));
        assert_eq!(mine.metrics.radius_medium, 2.0);
        assert_eq!(mine.metrics.radius_large, Metrics::default().radius_large);
        // After the built-ins.
        assert_eq!(registry.themes().last().unwrap().id, "mine");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn extends_chain_and_explicit_more_variant() {
        let dir = tmp("chain");
        std::fs::write(
            dir.join("a.json"),
            r##"{ "id": "a", "extends": "cha-jade", "variants": { "light": { "ink": "#111111" } } }"##,
        )
        .unwrap();
        std::fs::write(
            dir.join("b.json"),
            r##"{ "id": "b", "extends": "a", "variants": { "light-more": { "ink": "#000000" } } }"##,
        )
        .unwrap();
        let registry = Registry::from_dir(&dir);
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        let b = registry.get("b").unwrap();
        assert_eq!(
            b.palette(Variant::Light).ink,
            Color32::from_rgb(0x11, 0x11, 0x11)
        );
        assert_eq!(b.palette(Variant::LightMore).ink, Color32::BLACK);
        let jade = registry.get("cha-jade").unwrap();
        assert_eq!(b.palette(Variant::Dark), jade.palette(Variant::Dark));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn user_theme_with_a_built_in_id_overrides_it() {
        let dir = tmp("override");
        std::fs::write(
            dir.join("magenta.json"),
            r##"{ "id": "cha-magenta", "name": "My magenta",
                  "variants": { "dark": { "canvas": "#000000" } } }"##,
        )
        .unwrap();
        let registry = Registry::from_dir(&dir);
        assert!(registry.issues().is_empty(), "{:?}", registry.issues());
        assert_eq!(registry.themes().len(), 2);
        let magenta = registry.get("cha-magenta").unwrap();
        assert_eq!(magenta.name, "My magenta");
        assert!(!magenta.builtin);
        assert_eq!(magenta.palette(Variant::Dark).canvas, Color32::BLACK);
        assert_eq!(registry.themes()[0].id, "cha-magenta");
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn bad_files_are_reported_not_fatal() {
        let dir = tmp("bad");
        let write = |name: &str, text: &str| std::fs::write(dir.join(name), text).unwrap();
        write("broken.json", "{ not json");
        write("noid.json", r#"{ "name": "x" }"#);
        write(
            "badcolour.json",
            r##"{ "id": "bc", "variants": { "dark": { "accent": "pink" } } }"##,
        );
        write(
            "typo.json",
            r##"{ "id": "ty", "variants": { "dark": { "acent": "#fff" } } }"##,
        );
        write("orphan.json", r#"{ "id": "orphan", "extends": "nope" }"#);
        write("loop1.json", r#"{ "id": "l1", "extends": "l2" }"#);
        write("loop2.json", r#"{ "id": "l2", "extends": "l1" }"#);
        write("zdup.json", r#"{ "id": "orphan" }"#);
        write("child.json", r#"{ "id": "child", "extends": "orphan" }"#);
        write(
            "good.json",
            r##"{ "id": "good", "variants": { "dark": { "accent": "#fff" } } }"##,
        );
        write("readme.txt", "not a theme");

        let registry = Registry::from_dir(&dir);
        let files: Vec<&str> = registry.issues().iter().map(|i| i.file.as_str()).collect();
        for expected in [
            "broken.json",
            "noid.json",
            "badcolour.json",
            "typo.json",
            "orphan.json",
            "loop1.json",
            "loop2.json",
            "zdup.json",
            "child.json",
        ] {
            assert!(
                files.contains(&expected),
                "{expected} not reported: {:?}",
                registry.issues()
            );
        }
        assert!(!files.contains(&"good.json") && !files.contains(&"readme.txt"));
        let ids: Vec<&str> = registry.themes().iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["cha-magenta", "cha-jade", "good"]);
        // Reasons are written for people.
        let reason = |f: &str| {
            registry
                .issues()
                .iter()
                .find(|i| i.file == f)
                .unwrap()
                .reason
                .clone()
        };
        assert!(
            reason("badcolour.json").contains("pink"),
            "{}",
            reason("badcolour.json")
        );
        assert!(reason("orphan.json").contains("not a theme"));
        assert!(
            reason("loop1.json").contains("loop"),
            "{}",
            reason("loop1.json")
        );
        // Loading again after fixing a file picks it up.
        write("orphan.json", r#"{ "id": "orphan" }"#);
        let again = Registry::from_dir(&dir);
        assert!(again.get("orphan").is_some());
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn font_paths_are_relative_to_the_theme_file() {
        let dir = tmp("fonts");
        std::fs::write(
            dir.join("f.json"),
            r#"{ "id": "f", "fonts": { "proportional": "Inter.ttf", "monospace": "/abs/Mono.ttf" } }"#,
        )
        .unwrap();
        let registry = Registry::from_dir(&dir);
        let f = registry.get("f").unwrap();
        assert_eq!(f.fonts.proportional, Some(dir.join("Inter.ttf")));
        assert_eq!(f.fonts.monospace, Some(PathBuf::from("/abs/Mono.ttf")));
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_missing_folder_is_fine() {
        let registry = Registry::from_dir(Path::new("/definitely/not/here"));
        assert!(registry.issues().is_empty());
        assert_eq!(registry.themes().len(), 2);
    }
}
