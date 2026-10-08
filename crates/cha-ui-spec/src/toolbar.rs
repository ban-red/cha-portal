//! The toolbar's spec (`web/packages/ui-spec/toolbar.json`) and its model: [`build_toolbar`] turns
//! the session's state and the capabilities it reports into the controls, menus and countdown both
//! players draw. `web/packages/ui-spec/toolbar.ts` is the TypeScript twin; the cases in
//! `toolbar-cases.json` keep them equal. No egui here: the renderers only draw the result.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::health::{Platform, PlatformText};
use crate::panel::{Tone, Values, fill_panel};

const TOOLBAR_JSON: &str = include_str!("../../../web/packages/ui-spec/toolbar.json");

/// What a colour means; the renderer maps it to a theme colour. `Faint` is the tertiary ink and
/// `Dim` the secondary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolbarTone {
    #[default]
    None,
    Ok,
    Accent,
    Warn,
    Danger,
    Dim,
    Faint,
}

impl From<Tone> for ToolbarTone {
    fn from(t: Tone) -> Self {
        match t {
            Tone::None => ToolbarTone::None,
            Tone::Ok => ToolbarTone::Ok,
            Tone::Warn => ToolbarTone::Warn,
            Tone::Danger => ToolbarTone::Danger,
            Tone::Dim => ToolbarTone::Dim,
        }
    }
}

/// The fields a variant may override.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct Fields {
    pub icon: Option<String>,
    pub label: Option<PlatformText>,
    pub aria_label: Option<PlatformText>,
    pub tooltip: Option<PlatformText>,
    pub tone: Option<ToolbarTone>,
    pub disabled: Option<bool>,
    pub read_only: Option<PlatformText>,
    pub aria_text: Option<PlatformText>,
}

impl Fields {
    fn overlay(&mut self, v: &Fields) {
        macro_rules! over {
            ($($f:ident),*) => { $( if v.$f.is_some() { self.$f = v.$f.clone(); } )* };
        }
        over!(
            icon, label, aria_label, tooltip, tone, disabled, read_only, aria_text
        );
    }
}

/// Fields laid over a control, row or option while its conditions hold.
#[derive(Clone, Debug, Deserialize)]
pub struct Variant {
    #[serde(default)]
    pub when: Vec<String>,
    pub platforms: Option<Vec<Platform>>,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BadgeSpec {
    #[serde(default)]
    pub when: Vec<String>,
    pub text: String,
    pub tone: Option<ToolbarTone>,
    pub tone_from: Option<String>,
    pub tooltip: Option<PlatformText>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OptionSpec {
    pub value: String,
    pub platforms: Option<Vec<Platform>>,
    #[serde(default)]
    pub when: Vec<String>,
    #[serde(default)]
    pub states: Vec<Variant>,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(untagged)]
pub enum Options {
    Items { items: Vec<OptionSpec> },
    From { from: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RowKind {
    Choice,
    Button,
    Slider,
    List,
    Note,
    Link,
}

#[derive(Clone, Debug, Deserialize)]
pub struct RowSpec {
    pub id: String,
    pub kind: RowKind,
    pub platforms: Option<Vec<Platform>>,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub lacks: Vec<String>,
    #[serde(default)]
    pub when: Vec<String>,
    #[serde(default)]
    pub states: Vec<Variant>,
    pub group: Option<String>,
    pub dom_id: Option<String>,
    pub role: Option<String>,
    pub text: Option<PlatformText>,
    pub value: Option<String>,
    pub options: Option<Options>,
    #[serde(default)]
    pub edit_needs: Vec<String>,
    #[serde(default)]
    pub zero_when: Vec<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub display: Option<String>,
    pub from: Option<String>,
    pub item_detail: Option<String>,
    pub item_detail_none: Option<String>,
    pub to: Option<String>,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    Left,
    Right,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MenuSpec {
    pub id: String,
    pub dom_id: String,
    pub align: Align,
    pub rows: Vec<RowSpec>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ControlKind {
    Button,
    Toggle,
    Menu,
    Label,
    Badge,
    List,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Group {
    Left,
    Right,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemAction {
    pub label: String,
    pub tooltip: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ControlSpec {
    pub id: String,
    pub kind: ControlKind,
    pub group: Group,
    pub platforms: Option<Vec<Platform>>,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub when: Vec<String>,
    #[serde(default)]
    pub droppable: bool,
    pub hover_tone: Option<ToolbarTone>,
    #[serde(default)]
    pub pressed: Vec<String>,
    #[serde(default)]
    pub states: Vec<Variant>,
    pub badge: Option<BadgeSpec>,
    pub menu: Option<MenuSpec>,
    pub from: Option<String>,
    pub item_action: Option<ItemAction>,
    #[serde(flatten)]
    pub fields: Fields,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timing {
    pub fold_after_ms: u64,
    pub near_top_px: f32,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CancelSpec {
    pub label: String,
    pub tooltip: PlatformText,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PowerOffSpec {
    pub seconds: u32,
    pub default_name: String,
    pub title: String,
    pub stopping: String,
    pub failed: String,
    pub note: String,
    pub cancel: CancelSpec,
    pub close: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldedBar {
    pub aria_label: String,
    pub tooltip: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldedTab {
    pub aria_label: String,
    pub tooltip: String,
    pub disabled: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FoldedSpec {
    pub bar: FoldedBar,
    pub tab: FoldedTab,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub platforms: Option<Vec<Platform>>,
    pub when: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisibilitySpec {
    pub hide: Vec<Rule>,
    pub show: Vec<Rule>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapabilitySpec {
    pub doc: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reports {
    pub web: Vec<String>,
    pub native: Vec<String>,
}

impl Reports {
    /// The capabilities a platform can ever report.
    pub fn for_platform(&self, platform: Platform) -> &[String] {
        match platform {
            Platform::Web => &self.web,
            Platform::Native => &self.native,
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSpec {
    pub kind: String,
    pub platforms: Option<Vec<Platform>>,
    pub values: Option<Vec<String>>,
    #[serde(default)]
    pub derived: bool,
    pub doc: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolbarSpec {
    pub notes: String,
    pub timing: Timing,
    pub power_off: PowerOffSpec,
    pub folded: FoldedSpec,
    pub visibility: VisibilitySpec,
    pub capabilities: BTreeMap<String, CapabilitySpec>,
    pub reports: Reports,
    pub state: BTreeMap<String, StateSpec>,
    pub codec_labels: BTreeMap<String, String>,
    pub controls: Vec<ControlSpec>,
}

static SPEC: LazyLock<ToolbarSpec> =
    LazyLock::new(|| serde_json::from_str(TOOLBAR_JSON).expect("toolbar.json parses"));

/// The toolbar spec, parsed once.
pub fn spec() -> &'static ToolbarSpec {
    &SPEC
}

// --- state ---

/// What the session is doing, by state key (declared in the spec). A key that is null, false,
/// empty, an empty list or missing is absent; a number, even 0, is present.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct State(BTreeMap<String, Json>);

impl State {
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets a key: a bool, a number, a string, null or a list of JSON values.
    pub fn set(&mut self, key: &str, value: impl Into<Json>) -> &mut Self {
        self.0.insert(key.to_owned(), value.into());
        self
    }

    pub fn from_map(map: &BTreeMap<String, Json>) -> Self {
        Self(map.clone())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        self.0.get(key)
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    fn present(&self, key: &str) -> bool {
        self.0.get(key).is_some_and(is_present)
    }
}

fn is_present(v: &Json) -> bool {
    match v {
        Json::Null | Json::Bool(false) => false,
        Json::String(s) => !s.is_empty(),
        Json::Array(a) => !a.is_empty(),
        Json::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        _ => true,
    }
}

/// A value as `key=value` compares it: a number without a fraction reads as an integer.
fn plain(v: Option<&Json>) -> String {
    match v {
        None | Some(Json::Null) => String::new(),
        Some(Json::String(s)) => s.clone(),
        Some(Json::Number(n)) => match n.as_f64() {
            Some(f) if f.fract() == 0.0 && f.abs() < 1e15 => format!("{}", f as i64),
            Some(f) => format!("{f}"),
            None => n.to_string(),
        },
        Some(other) => other.to_string(),
    }
}

fn holds(cond: &str, state: &State) -> bool {
    if let Some(at) = cond.find(['=', '>', '!'])
        && at > 0
    {
        let (key, rest) = cond.split_at(at);
        if let Some(want) = rest.strip_prefix("!=") {
            return plain(state.get(key)) != want;
        }
        if let Some(want) = rest.strip_prefix('=') {
            return state.present(key) && plain(state.get(key)) == want;
        }
        if let Some(want) = rest.strip_prefix('>') {
            let limit: f64 = want.parse().unwrap_or(f64::NAN);
            return state
                .get(key)
                .and_then(Json::as_f64)
                .is_some_and(|v| v > limit);
        }
    }
    match cond.strip_prefix('!') {
        Some(key) => !state.present(key),
        None => state.present(cond),
    }
}

fn all(conds: &[String], state: &State) -> bool {
    conds.iter().all(|c| holds(c, state))
}

fn on(platforms: &Option<Vec<Platform>>, platform: Platform) -> bool {
    platforms.as_ref().is_none_or(|p| p.contains(&platform))
}

// --- the model ---

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarBadge {
    pub text: String,
    pub tone: ToolbarTone,
    pub tooltip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarOption {
    pub value: String,
    pub label: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ItemButton {
    pub label: String,
    pub tooltip: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarItem {
    /// The entry's words: a controller's name, a watcher's label.
    pub text: String,
    /// A controller's slot ("slot 2", "no slot").
    pub detail: Option<String>,
    /// A watcher's id.
    pub id: Option<String>,
    /// The button beside the entry, when it has one.
    pub action: Option<ItemButton>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarSlider {
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub value: f64,
    pub display: String,
    pub aria_text: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarRow {
    pub id: String,
    pub kind: RowKind,
    pub group: Option<String>,
    pub dom_id: Option<String>,
    pub role: Option<String>,
    pub label: Option<String>,
    pub tooltip: Option<String>,
    /// A note's, link's or button's words.
    pub text: Option<String>,
    pub tone: ToolbarTone,
    pub disabled: bool,
    // choice
    pub value: Option<String>,
    pub options: Option<Vec<ToolbarOption>>,
    pub editable: bool,
    pub read_only: Option<String>,
    // slider
    pub slider: Option<ToolbarSlider>,
    // list
    pub items: Option<Vec<ToolbarItem>>,
    // link
    pub to: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarMenu {
    pub id: String,
    pub dom_id: String,
    pub align: Align,
    pub rows: Vec<ToolbarRow>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarControl {
    pub id: String,
    pub kind: ControlKind,
    pub group: Group,
    /// Left out first when the bar is too narrow.
    pub droppable: bool,
    pub icon: Option<String>,
    pub label: Option<String>,
    pub aria_label: Option<String>,
    pub tooltip: Option<String>,
    /// A toggle's state; `None` for the rest.
    pub pressed: Option<bool>,
    /// A menu's button while its menu is open.
    pub active: bool,
    pub disabled: bool,
    pub tone: ToolbarTone,
    pub hover_tone: ToolbarTone,
    pub badge: Option<ToolbarBadge>,
    /// The open menu, resolved; `None` while closed.
    pub menu: Option<ToolbarMenu>,
    /// A menu button's menu id, open or not.
    pub menu_dom_id: Option<String>,
    /// A list control's entries.
    pub items: Option<Vec<ToolbarItem>>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ToolbarCountdown {
    pub seconds: u32,
    /// seconds / total, 1 at the start.
    pub fraction: f64,
    pub title: String,
    pub note: String,
    pub cancel_label: String,
    pub cancel_tooltip: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct FoldedBarModel {
    pub aria_label: String,
    pub tooltip: String,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HideTab {
    pub aria_label: String,
    pub tooltip: String,
    pub disabled: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Toolbar {
    /// The bar is up (otherwise only its folded bar shows).
    pub visible: bool,
    pub controls: Vec<ToolbarControl>,
    pub countdown: Option<ToolbarCountdown>,
    pub folded_bar: FoldedBarModel,
    pub hide_tab: HideTab,
}

impl Toolbar {
    pub fn control(&self, id: &str) -> Option<&ToolbarControl> {
        self.controls.iter().find(|c| c.id == id)
    }
}

struct Ctx<'a> {
    spec: &'a ToolbarSpec,
    state: &'a State,
    caps: &'a [String],
    platform: Platform,
    values: Values,
}

impl Ctx<'_> {
    fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c == cap)
    }

    fn shown(
        &self,
        platforms: &Option<Vec<Platform>>,
        needs: &[String],
        lacks: &[String],
        when: &[String],
    ) -> bool {
        on(platforms, self.platform)
            && needs.iter().all(|n| self.has(n))
            && !lacks.iter().any(|n| self.has(n))
            && all(when, self.state)
    }

    /// The base fields with every variant that applies laid over them, in order.
    fn resolve(&self, base: &Fields, states: &[Variant]) -> Fields {
        let mut out = base.clone();
        for s in states {
            if on(&s.platforms, self.platform) && all(&s.when, self.state) {
                out.overlay(&s.fields);
            }
        }
        out
    }

    /// The text for the platform, filled; `None` when the spec has none for it.
    fn words(&self, t: Option<&PlatformText>) -> Option<String> {
        self.words_with(t, &[])
    }

    fn words_with(&self, t: Option<&PlatformText>, extra: &[(&str, f64)]) -> Option<String> {
        let s = t?.get(self.platform)?;
        if extra.is_empty() {
            return Some(fill_panel(s, &self.values));
        }
        let mut values = self.values.clone();
        for (k, v) in extra {
            values.num(k, *v);
        }
        Some(fill_panel(s, &values))
    }

    fn fill(&self, template: &str, extra: &[(&str, f64)]) -> String {
        let mut values = self.values.clone();
        for (k, v) in extra {
            values.num(k, *v);
        }
        fill_panel(template, &values)
    }
}

fn list_items(
    list: Option<&Json>,
    detail: Option<&str>,
    detail_none: Option<&str>,
    action: Option<&ItemAction>,
    x: &Ctx,
) -> Vec<ToolbarItem> {
    let Some(Json::Array(entries)) = list else {
        return Vec::new();
    };
    entries
        .iter()
        .map(|e| {
            let Json::Object(o) = e else {
                return ToolbarItem {
                    text: plain(Some(e)),
                    detail: None,
                    id: None,
                    action: None,
                };
            };
            let text = |k: &str| o.get(k).and_then(Json::as_str).map(str::to_owned);
            let detail = detail.and_then(|d| match o.get("slot").and_then(Json::as_f64) {
                Some(slot) => Some(x.fill(d, &[("n", slot + 1.0)])),
                None => detail_none.map(str::to_owned),
            });
            let can_hand = o.get("can_hand").and_then(Json::as_bool).unwrap_or(false);
            ToolbarItem {
                text: text("label").or_else(|| text("name")).unwrap_or_default(),
                detail,
                id: text("id"),
                action: action.filter(|_| can_hand).map(|a| ItemButton {
                    label: a.label.clone(),
                    tooltip: a.tooltip.clone(),
                }),
            }
        })
        .collect()
}

fn options(r: &RowSpec, x: &Ctx) -> Vec<ToolbarOption> {
    match &r.options {
        None => Vec::new(),
        Some(Options::From { .. }) => match x.state.get("codecs") {
            Some(Json::Array(list)) => list
                .iter()
                .map(|c| {
                    let id = plain(Some(c));
                    ToolbarOption {
                        label: x
                            .spec
                            .codec_labels
                            .get(&id)
                            .cloned()
                            .unwrap_or_else(|| id.to_uppercase()),
                        value: id,
                        disabled: false,
                    }
                })
                .collect(),
            _ => Vec::new(),
        },
        Some(Options::Items { items }) => items
            .iter()
            .filter(|i| on(&i.platforms, x.platform) && all(&i.when, x.state))
            .map(|i| {
                let m = x.resolve(&i.fields, &i.states);
                ToolbarOption {
                    value: i.value.clone(),
                    label: x.words(m.label.as_ref()).unwrap_or_default(),
                    disabled: m.disabled == Some(true),
                }
            })
            .collect(),
    }
}

fn row(r: &RowSpec, x: &Ctx) -> Option<ToolbarRow> {
    if !x.shown(&r.platforms, &r.needs, &r.lacks, &r.when) {
        return None;
    }
    let m = x.resolve(&r.fields, &r.states);
    let editable = r.kind == RowKind::Choice && r.edit_needs.iter().all(|n| x.has(n));
    let read_only = x.words(m.read_only.as_ref());
    if r.kind == RowKind::Choice && !editable && read_only.is_none() {
        return None;
    }
    let mut out = ToolbarRow {
        id: r.id.clone(),
        kind: r.kind,
        group: r.group.clone(),
        dom_id: r.dom_id.clone(),
        role: r.role.clone(),
        label: if r.kind == RowKind::Note {
            None
        } else {
            x.words(m.label.as_ref())
        },
        tooltip: x.words(m.tooltip.as_ref()),
        text: x.words(r.text.as_ref()),
        tone: m.tone.unwrap_or_default(),
        disabled: m.disabled == Some(true),
        value: None,
        options: None,
        editable: false,
        read_only: None,
        slider: None,
        items: None,
        to: r.to.clone(),
    };
    match r.kind {
        RowKind::Choice => {
            let key = r.value.as_deref().unwrap_or_default();
            out.value = x.state.present(key).then(|| plain(x.state.get(key)));
            out.options = Some(options(r, x));
            out.editable = editable;
            out.read_only = read_only;
        }
        RowKind::Slider => {
            let key = r.value.as_deref().unwrap_or_default();
            let level = if all(&r.zero_when, x.state) {
                0.0
            } else {
                x.state.get(key).and_then(Json::as_f64).unwrap_or(0.0)
            };
            let extra = [("shown", level)];
            out.slider = Some(ToolbarSlider {
                min: r.min.unwrap_or_default(),
                max: r.max.unwrap_or_default(),
                step: r.step.unwrap_or_default(),
                value: level,
                display: x.fill(r.display.as_deref().unwrap_or_default(), &extra),
                aria_text: x
                    .words_with(m.aria_text.as_ref(), &extra)
                    .unwrap_or_default(),
            });
        }
        RowKind::List => {
            let from = r.from.as_deref().unwrap_or_default();
            out.items = Some(list_items(
                x.state.get(from),
                r.item_detail.as_deref(),
                r.item_detail_none.as_deref(),
                None,
                x,
            ));
        }
        RowKind::Button | RowKind::Note | RowKind::Link => {}
    }
    Some(out)
}

fn control(c: &ControlSpec, x: &Ctx) -> Option<ToolbarControl> {
    if !x.shown(&c.platforms, &c.needs, &[], &c.when) {
        return None;
    }
    let m = x.resolve(&c.fields, &c.states);
    let active = c
        .menu
        .as_ref()
        .is_some_and(|menu| plain(x.state.get("menu_open")) == menu.id);
    let badge = c.badge.as_ref().filter(|b| all(&b.when, x.state)).map(|b| {
        let tone = if b.tone_from.as_deref() == Some("grade") {
            grade_tone(&plain(x.state.get("grade")))
        } else {
            b.tone.unwrap_or_default()
        };
        ToolbarBadge {
            text: x.fill(&b.text, &[]),
            tone,
            tooltip: x.words(b.tooltip.as_ref()),
        }
    });
    let menu = match (&c.menu, active) {
        (Some(menu), true) => Some(ToolbarMenu {
            id: menu.id.clone(),
            dom_id: menu.dom_id.clone(),
            align: menu.align,
            rows: menu.rows.iter().filter_map(|r| row(r, x)).collect(),
        }),
        _ => None,
    };
    let items = (c.kind == ControlKind::List).then(|| {
        list_items(
            x.state.get(c.from.as_deref().unwrap_or_default()),
            None,
            None,
            c.item_action.as_ref(),
            x,
        )
    });
    Some(ToolbarControl {
        id: c.id.clone(),
        kind: c.kind,
        group: c.group,
        droppable: c.droppable,
        icon: m.icon.clone(),
        label: x.words(m.label.as_ref()),
        aria_label: x.words(m.aria_label.as_ref()),
        tooltip: x.words(m.tooltip.as_ref()),
        pressed: (c.kind == ControlKind::Toggle).then(|| all(&c.pressed, x.state)),
        active,
        disabled: m.disabled == Some(true),
        tone: m.tone.unwrap_or_default(),
        hover_tone: c.hover_tone.unwrap_or_default(),
        badge,
        menu,
        menu_dom_id: c.menu.as_ref().map(|menu| menu.dom_id.clone()),
        items,
    })
}

/// The stats panel's tone for a grade letter (A and B ok, C and D warn, F danger, none dim).
fn grade_tone(grade: &str) -> ToolbarTone {
    let t = &crate::panel::spec().grade_tone;
    ToolbarTone::from(match grade {
        "A" => t.a,
        "B" => t.b,
        "C" => t.c,
        "D" => t.d,
        "F" => t.f,
        _ => t.none,
    })
}

/// The toolbar a player draws: the controls it shows now, with their icon, words and state
/// resolved. Pure, so both players agree.
pub fn build_toolbar(
    spec: &ToolbarSpec,
    state: &State,
    capabilities: &[String],
    platform: Platform,
) -> Toolbar {
    let mut values = Values::new();
    for (k, v) in &state.0 {
        match v {
            Json::Number(n) => {
                if let Some(f) = n.as_f64() {
                    values.num(k, f);
                }
            }
            Json::String(s) => {
                values.text(k, s);
            }
            _ => {}
        }
    }
    let count = match state.get("controllers") {
        Some(Json::Array(a)) => a.len(),
        _ => 0,
    };
    values.num("controllers_count", count as f64);
    values.num("power_off_seconds", f64::from(spec.power_off.seconds));
    let x = Ctx {
        spec,
        state,
        caps: capabilities,
        platform,
        values,
    };

    let applies = |r: &Rule| on(&r.platforms, platform) && all(&r.when, state);
    let visible =
        !spec.visibility.hide.iter().any(applies) && spec.visibility.show.iter().any(applies);

    let controls = spec
        .controls
        .iter()
        .filter_map(|c| control(c, &x))
        .collect();

    let countdown = state
        .get("countdown")
        .and_then(Json::as_f64)
        .filter(|v| v.is_finite())
        .map(|left| {
            let p = &spec.power_off;
            let name = match plain(state.get("title")) {
                t if t.is_empty() => p.default_name.clone(),
                t => t,
            };
            let mut v = Values::new();
            v.text("name", &name).num("seconds", left);
            ToolbarCountdown {
                seconds: left as u32,
                fraction: (left / f64::from(p.seconds)).clamp(0.0, 1.0),
                title: fill_panel(&p.title, &v),
                note: p.note.clone(),
                cancel_label: p.cancel.label.clone(),
                cancel_tooltip: x.words(Some(&p.cancel.tooltip)),
            }
        });

    Toolbar {
        visible,
        controls,
        countdown,
        folded_bar: FoldedBarModel {
            aria_label: spec.folded.bar.aria_label.clone(),
            tooltip: spec.folded.bar.tooltip.clone(),
        },
        hide_tab: HideTab {
            aria_label: spec.folded.tab.aria_label.clone(),
            tooltip: spec.folded.tab.tooltip.clone(),
            disabled: all(&spec.folded.tab.disabled, state),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;
    use crate::Icon;

    fn variants_of(c: &ControlSpec) -> impl Iterator<Item = &Fields> {
        std::iter::once(&c.fields)
            .chain(c.states.iter().map(|s| &s.fields))
            .chain(c.menu.iter().flat_map(|m| {
                m.rows.iter().flat_map(|r| {
                    std::iter::once(&r.fields)
                        .chain(r.states.iter().map(|s| &s.fields))
                        .chain(
                            match &r.options {
                                Some(Options::Items { items }) => items.iter().collect(),
                                _ => Vec::new(),
                            }
                            .into_iter()
                            .flat_map(|i| {
                                std::iter::once(&i.fields).chain(i.states.iter().map(|s| &s.fields))
                            }),
                        )
                })
            }))
    }

    #[test]
    fn every_icon_the_spec_names_exists() {
        for c in &spec().controls {
            for f in variants_of(c) {
                if let Some(id) = &f.icon {
                    assert!(Icon::from_id(id).is_some(), "{}: no icon {id}", c.id);
                }
            }
        }
    }

    #[test]
    fn every_capability_is_declared_reported_somewhere_and_used() {
        let s = spec();
        let declared: BTreeSet<&str> = s.capabilities.keys().map(String::as_str).collect();
        let mut used = BTreeSet::new();
        for c in &s.controls {
            used.extend(c.needs.iter().map(String::as_str));
            for r in c.menu.iter().flat_map(|m| &m.rows) {
                used.extend(
                    r.needs
                        .iter()
                        .chain(&r.lacks)
                        .chain(&r.edit_needs)
                        .map(String::as_str),
                );
            }
        }
        assert!(
            used.is_subset(&declared),
            "undeclared: {:?}",
            used.difference(&declared)
        );
        assert!(
            declared.is_subset(&used),
            "unused: {:?}",
            declared.difference(&used)
        );
        for platform in [Platform::Web, Platform::Native] {
            for cap in s.reports.for_platform(platform) {
                assert!(
                    declared.contains(cap.as_str()),
                    "{platform:?} reports {cap}"
                );
            }
        }
    }

    #[test]
    fn ids_are_unique() {
        let s = spec();
        let mut ids = BTreeSet::new();
        for c in &s.controls {
            assert!(ids.insert(c.id.as_str()), "{} twice", c.id);
            for r in c.menu.iter().flat_map(|m| &m.rows) {
                assert!(ids.insert(r.id.as_str()), "{} twice", r.id);
            }
        }
    }

    #[test]
    fn the_stats_button_wears_the_panels_grade_tones() {
        let tone = |grade: &str| {
            let mut state = State::new();
            state.set("grade", grade).set("grade_summary", "x");
            state.set("connected", true).set("expanded", true);
            build_toolbar(spec(), &state, &[], Platform::Native)
                .control("stats")
                .and_then(|c| c.badge.as_ref().map(|b| b.tone))
        };
        assert_eq!(tone("A"), Some(ToolbarTone::Ok));
        assert_eq!(tone("C"), Some(ToolbarTone::Warn));
        assert_eq!(tone("F"), Some(ToolbarTone::Danger));
        assert_eq!(tone(""), None);
    }
}
