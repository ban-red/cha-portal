//! Following the portal's theme: where a player's look comes from, the last
//! look a portal sent (kept in `config.json` so an offline start still shows
//! it), and when to ask the portal again.
//!
//! The portal saves a theme, light or dark and contrast per user
//! (`GET /api/me/prefs`). Its `motion` and `transparency` are not read.

use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::resolve::{Appearance, Contrast, ThemePrefs};

/// How often the launcher asks a followed portal for its theme.
pub const REFRESH: Duration = Duration::from_secs(5 * 60);
/// After a failed fetch, wait this long before trying again.
pub const RETRY: Duration = Duration::from_secs(60);
/// Regaining focus fetches again, but not more often than this.
pub const FOCUS_DEBOUNCE: Duration = Duration::from_secs(20);

/// Where the look comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ThemeSource {
    /// This Mac's own choice (`theme`, `appearance`, `contrast` in the config).
    Local,
    /// The look of the user's account on this portal (an origin).
    Portal { origin: String },
}

/// The three things that make up a look.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeLook {
    pub theme: String,
    pub appearance: Appearance,
    pub contrast: Contrast,
}

impl Default for ThemeLook {
    fn default() -> Self {
        Self {
            theme: super::registry::DEFAULT_THEME.into(),
            appearance: Appearance::System,
            contrast: Contrast::System,
        }
    }
}

/// The last look a portal sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalLook {
    pub origin: String,
    pub look: ThemeLook,
}

impl ThemePrefs {
    /// The portal being followed, if any.
    pub fn following(&self) -> Option<&str> {
        match &self.source {
            Some(ThemeSource::Portal { origin }) => Some(origin),
            _ => None,
        }
    }

    /// This Mac's own look.
    pub fn local_look(&self) -> ThemeLook {
        ThemeLook {
            theme: self.theme.clone(),
            appearance: self.appearance,
            contrast: self.contrast,
        }
    }

    /// The look in use: the followed portal's last one, or this Mac's own.
    /// Until a followed portal has answered once, this Mac's own shows.
    pub fn look(&self) -> ThemeLook {
        match (self.following(), &self.portal_look) {
            (Some(origin), Some(cached)) if cached.origin == origin => cached.look.clone(),
            _ => self.local_look(),
        }
    }

    /// Prefs that resolve to [`Self::look`] with no source involved.
    pub fn effective(&self) -> ThemePrefs {
        let look = self.look();
        ThemePrefs {
            theme: look.theme,
            appearance: look.appearance,
            contrast: look.contrast,
            scale: self.scale,
            source: None,
            portal_look: None,
        }
    }

    /// The user chose a look here: this Mac's own from now on.
    pub fn set_local(&mut self, look: ThemeLook) {
        self.theme = look.theme;
        self.appearance = look.appearance;
        self.contrast = look.contrast;
        self.source = Some(ThemeSource::Local);
    }

    /// Follow `origin`.
    pub fn follow(&mut self, origin: &str) {
        self.source = Some(ThemeSource::Portal {
            origin: origin.to_string(),
        });
    }

    /// A player that has never chosen follows the portal it is signed in to.
    /// True when it began to.
    pub fn follow_by_default(&mut self, origin: &str) -> bool {
        if self.source.is_none() {
            self.follow(origin);
            return true;
        }
        false
    }

    /// Remember what `origin` sent. True when that changes what is shown or
    /// kept.
    pub fn store_portal_look(&mut self, origin: &str, look: ThemeLook) -> bool {
        let new = Some(PortalLook {
            origin: origin.to_string(),
            look,
        });
        if self.portal_look == new {
            return false;
        }
        self.portal_look = new;
        true
    }

    /// Signed out of (or lost) `origin`: if it was followed, keep its last
    /// look as this Mac's own. True when anything changed.
    pub fn leave_portal(&mut self, origin: &str) -> bool {
        if self.following() != Some(origin) {
            return false;
        }
        let look = self.look();
        self.set_local(look);
        self.portal_look = None;
        true
    }
}

/// `https://portal.example:8443/` as `portal.example:8443`.
pub fn origin_label(origin: &str) -> String {
    origin
        .strip_prefix("https://")
        .or_else(|| origin.strip_prefix("http://"))
        .unwrap_or(origin)
        .trim_end_matches('/')
        .to_string()
}

/// A one-line description of a look for Settings ("Cha Jade · Dark · Contrast: More").
pub fn describe(look: &ThemeLook, theme_name: &str) -> String {
    let appearance = match look.appearance {
        Appearance::System => "System light or dark",
        Appearance::Dark => "Dark",
        Appearance::Light => "Light",
    };
    let contrast = match look.contrast {
        Contrast::System => "System contrast",
        Contrast::Standard => "Standard contrast",
        Contrast::More => "More contrast",
    };
    format!("{theme_name} · {appearance} · {contrast}")
}

/// When to fetch the followed portal's look. Pure: callers pass the time.
#[derive(Debug, Default)]
pub struct FollowClock {
    /// The portal the last fetch (or the one in flight) was for.
    origin: Option<String>,
    last_attempt: Option<Instant>,
    due: Option<Instant>,
    in_flight: bool,
    focus_pending: bool,
    /// A failure was logged and no success has followed.
    logged_failure: bool,
}

impl FollowClock {
    /// The window regained focus.
    pub fn focused(&mut self) {
        self.focus_pending = true;
    }

    /// Fetch again at the next [`Self::poll`] (a sign-in just happened).
    pub fn ask_now(&mut self) {
        self.due = None;
        self.origin = None;
    }

    /// Whether to start a fetch of `origin` (the followed portal, if any).
    /// When true the fetch counts as started.
    pub fn poll(&mut self, origin: Option<&str>, now: Instant) -> bool {
        let Some(origin) = origin else {
            self.origin = None;
            self.focus_pending = false;
            return false;
        };
        if self.in_flight {
            return false;
        }
        let changed = self.origin.as_deref() != Some(origin);
        let periodic = self.due.is_none_or(|due| now >= due);
        let on_focus = self.focus_pending
            && self
                .last_attempt
                .is_none_or(|t| now.duration_since(t) >= FOCUS_DEBOUNCE);
        if !(changed || periodic || on_focus) {
            return false;
        }
        self.focus_pending = false;
        self.origin = Some(origin.to_string());
        self.last_attempt = Some(now);
        self.in_flight = true;
        true
    }

    /// A fetch ended. True when a failure should be logged (the first since
    /// the last success).
    pub fn finished(&mut self, ok: bool, now: Instant) -> bool {
        self.in_flight = false;
        self.due = Some(now + if ok { REFRESH } else { RETRY });
        if ok {
            self.logged_failure = false;
            return false;
        }
        !std::mem::replace(&mut self.logged_failure, true)
    }
}

#[cfg(feature = "portal")]
impl From<Option<&cha_client_portal::PortalTheme>> for ThemeLook {
    /// What the portal chose; what it didn't choose is the default, as in the
    /// browser.
    fn from(portal: Option<&cha_client_portal::PortalTheme>) -> Self {
        use cha_client_portal::{PortalAppearance, PortalContrast};
        let mut look = ThemeLook::default();
        if let Some(p) = portal {
            if let Some(theme) = &p.theme {
                look.theme = theme.clone();
            }
            look.appearance = match p.appearance {
                Some(PortalAppearance::Dark) => Appearance::Dark,
                Some(PortalAppearance::Light) => Appearance::Light,
                Some(PortalAppearance::System) | None => Appearance::System,
            };
            look.contrast = match p.contrast {
                Some(PortalContrast::Standard) => Contrast::Standard,
                Some(PortalContrast::More) => Contrast::More,
                Some(PortalContrast::System) | None => Contrast::System,
            };
        }
        look
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::resolve::{SystemEnv, Variant};

    fn jade_dark() -> ThemeLook {
        ThemeLook {
            theme: "cha-jade".into(),
            appearance: Appearance::Dark,
            contrast: Contrast::More,
        }
    }

    const P: &str = "https://portal.example";

    #[test]
    fn an_old_config_loads_and_counts_as_a_choice_only_if_it_differs() {
        let old: ThemePrefs = serde_json::from_str(r#"{"theme":"cha-jade","scale":1.0}"#).unwrap();
        assert_eq!(old.source, None);
        assert_eq!(old.clone().sanitized().source, Some(ThemeSource::Local));
        let fresh: ThemePrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(fresh.sanitized().source, None);
    }

    #[test]
    fn config_round_trips_with_a_cache() {
        let mut prefs = ThemePrefs::default();
        prefs.follow(P);
        prefs.store_portal_look(P, jade_dark());
        let text = serde_json::to_string(&prefs).unwrap();
        let back: ThemePrefs = serde_json::from_str(&text).unwrap();
        assert_eq!(back, prefs);
        assert_eq!(back.look(), jade_dark());
        assert_eq!(back.following(), Some(P));
    }

    #[test]
    fn following_shows_the_portals_look_and_local_is_kept() {
        let mut prefs = ThemePrefs {
            theme: "mine".into(),
            appearance: Appearance::Light,
            ..ThemePrefs::default()
        };
        prefs.follow(P);
        // Not fetched yet: this Mac's own look stays on screen.
        assert_eq!(prefs.look().theme, "mine");
        assert!(prefs.store_portal_look(P, jade_dark()));
        assert!(!prefs.store_portal_look(P, jade_dark()));
        assert_eq!(prefs.look(), jade_dark());
        assert_eq!(prefs.effective().theme, "cha-jade");
        assert_eq!(prefs.local_look().theme, "mine");
        // A cache from another portal isn't used.
        prefs.follow("https://other.example");
        assert_eq!(prefs.look().theme, "mine");
    }

    #[test]
    fn effective_prefs_resolve_to_the_portals_variant() {
        use crate::theme::registry::Registry;
        let mut prefs = ThemePrefs::default();
        prefs.follow(P);
        prefs.store_portal_look(P, jade_dark());
        let env = SystemEnv {
            dark: false,
            increase_contrast: false,
        };
        let r = crate::theme::resolve::resolve(&Registry::builtin(), &prefs.effective(), &env);
        assert_eq!(r.theme_id, "cha-jade");
        assert_eq!(r.variant, Variant::DarkMore);
    }

    #[test]
    fn choosing_a_look_switches_to_local() {
        let mut prefs = ThemePrefs::default();
        prefs.follow(P);
        prefs.store_portal_look(P, jade_dark());
        let mut look = prefs.look();
        look.appearance = Appearance::Light;
        prefs.set_local(look.clone());
        assert_eq!(prefs.source, Some(ThemeSource::Local));
        assert_eq!(prefs.look(), look);
        assert_eq!(prefs.theme, "cha-jade");
    }

    #[test]
    fn a_never_chosen_player_follows_the_first_portal_once() {
        let mut prefs = ThemePrefs::default();
        assert!(prefs.follow_by_default(P));
        assert!(!prefs.follow_by_default("https://other.example"));
        assert_eq!(prefs.following(), Some(P));
        // Someone who chose Local is left alone.
        let mut local = ThemePrefs::default();
        local.set_local(ThemeLook::default());
        assert!(!local.follow_by_default(P));
        assert_eq!(local.following(), None);
    }

    #[test]
    fn signing_out_keeps_the_last_look_as_local() {
        let mut prefs = ThemePrefs::default();
        prefs.follow(P);
        prefs.store_portal_look(P, jade_dark());
        assert!(!prefs.leave_portal("https://other.example"));
        assert!(prefs.leave_portal(P));
        assert_eq!(prefs.source, Some(ThemeSource::Local));
        assert_eq!(prefs.portal_look, None);
        assert_eq!(prefs.look(), jade_dark());
        assert!(!prefs.leave_portal(P));
    }

    #[test]
    fn origin_labels() {
        assert_eq!(origin_label("https://portal.example/"), "portal.example");
        assert_eq!(origin_label("http://127.0.0.1:8090"), "127.0.0.1:8090");
        assert_eq!(origin_label("portal.home.lan"), "portal.home.lan");
    }

    #[test]
    fn the_clock_fetches_on_start_every_few_minutes_and_on_focus() {
        let t0 = Instant::now();
        let mut clock = FollowClock::default();
        // Nothing followed: nothing to fetch.
        assert!(!clock.poll(None, t0));
        // On start.
        assert!(clock.poll(Some(P), t0));
        // Not twice while one is out.
        assert!(!clock.poll(Some(P), t0));
        assert!(!clock.finished(true, t0));
        assert!(!clock.poll(Some(P), t0 + Duration::from_secs(60)));
        // Focus before the debounce is passed over...
        clock.focused();
        assert!(!clock.poll(Some(P), t0 + Duration::from_secs(5)));
        // ...and taken once it has passed.
        assert!(clock.poll(Some(P), t0 + FOCUS_DEBOUNCE));
        clock.finished(true, t0 + FOCUS_DEBOUNCE);
        // Every few minutes.
        let later = t0 + FOCUS_DEBOUNCE + REFRESH;
        assert!(!clock.poll(Some(P), later - Duration::from_secs(1)));
        assert!(clock.poll(Some(P), later));
        clock.finished(true, later);
        // A different portal is fetched at once; so is a fresh sign-in.
        assert!(clock.poll(Some("https://other.example"), later));
        clock.finished(true, later);
        clock.ask_now();
        assert!(clock.poll(Some("https://other.example"), later));
    }

    #[test]
    fn a_failure_is_logged_once_and_retried_sooner() {
        let t0 = Instant::now();
        let mut clock = FollowClock::default();
        assert!(clock.poll(Some(P), t0));
        assert!(clock.finished(false, t0), "first failure is logged");
        assert!(!clock.poll(Some(P), t0 + RETRY - Duration::from_secs(1)));
        assert!(clock.poll(Some(P), t0 + RETRY));
        assert!(!clock.finished(false, t0 + RETRY), "second is not");
        assert!(clock.poll(Some(P), t0 + RETRY * 2));
        assert!(!clock.finished(true, t0 + RETRY * 2));
        assert!(clock.poll(Some(P), t0 + RETRY * 2 + REFRESH));
        assert!(
            clock.finished(false, t0 + RETRY * 2 + REFRESH),
            "after a success, again"
        );
    }
}
