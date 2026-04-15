//! News retreat state machine (Epic F, sub-component #2).
//!
//! Defensive predictive signal: trip a soft-widen / pull
//! flag on a high-priority news headline so the quoter
//! retreats in advance of the price move that historically
//! follows the headline. The retreat decays on a per-class
//! cooldown so a single old headline does not hold the bot
//! offline forever.
//!
//! v1 ships **no built-in feed source** — operators wire
//! their own (Telegram bot, file tail, paid Tiingo adapter,
//! their own scraper) and call [`NewsRetreatStateMachine::on_headline`]
//! for each item. The state machine itself is a pure
//! function of `(text, now_ms)`.
//!
//! Source attribution + state diagram in
//! `docs/research/defensive-layer-formulas.md`
//! §"Sub-component #2".
//!
//! # Stage-2: regex priority lists
//!
//! Stage-1 shipped case-insensitive substring keyword
//! lists because the workspace had no `regex` dependency.
//! Stage-2 swaps substring matching for compiled
//! [`regex::Regex`] priority lists — operators now get
//! word boundaries (`\bhack\b`), alternation
//! (`SEC|fraud|hack`), and wildcards (`crypto.*hack`)
//! for free. The public `NewsRetreatConfig` keeps its
//! `Vec<String>` fields so operators still configure with
//! raw pattern strings; compilation happens once in
//! [`NewsRetreatStateMachine::new`], which now returns
//! `anyhow::Result<Self>` so a malformed pattern surfaces
//! as an error instead of a panic.
//!
//! Case-insensitivity is baked in via the `(?i)` inline
//! flag on every compiled pattern, so the v1 canonical
//! keyword set (`"SEC"`, `"hack"`, `"FOMC"`) continues to
//! match regardless of headline case.

use anyhow::{Context, Result};
use regex::Regex;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;

/// Severity classification of an incoming headline. The
/// state machine promotes through these in priority order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NewsClass {
    /// Low-priority headline — alert only, no quote impact
    /// (multiplier stays at 1.0).
    Low,
    /// High-priority headline — soft widen via autotune
    /// multiplier (default 2.0).
    High,
    /// Critical headline — full retreat. Multiplier saturates
    /// (default 3.0) AND `should_stop_new_orders()` returns
    /// `true` so the engine routes through kill switch L2.
    Critical,
}

/// Current state of the retreat state machine. `Normal` is
/// the resting state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NewsRetreatState {
    Normal,
    Low,
    High,
    Critical,
}

impl NewsRetreatState {
    /// Numeric ordering for comparing severities. Used
    /// internally by the promotion ladder.
    fn rank(self) -> u8 {
        match self {
            Self::Normal => 0,
            Self::Low => 1,
            Self::High => 2,
            Self::Critical => 3,
        }
    }
}

impl From<NewsClass> for NewsRetreatState {
    fn from(class: NewsClass) -> Self {
        match class {
            NewsClass::Low => Self::Low,
            NewsClass::High => Self::High,
            NewsClass::Critical => Self::Critical,
        }
    }
}

/// Result of one [`NewsRetreatStateMachine::on_headline`]
/// call. Operators route this into audit + alert layers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NewsRetreatTransition {
    /// Headline did not match any priority list.
    NoMatch,
    /// Headline matched but was a lower (or equal) class
    /// than the active state — no promotion. The cooldown
    /// timer was NOT refreshed.
    Suppressed {
        class: NewsClass,
        current: NewsRetreatState,
    },
    /// Headline matched at the same class as the active
    /// state — state did not change but the cooldown timer
    /// reset to "now".
    Refreshed(NewsRetreatState),
    /// Headline promoted the state to a higher class.
    Promoted {
        from: NewsRetreatState,
        to: NewsRetreatState,
    },
}

/// Tuning knobs for [`NewsRetreatStateMachine::new`].
#[derive(Debug, Clone)]
pub struct NewsRetreatConfig {
    /// Regex patterns that promote to `Critical`. Each
    /// pattern is compiled once in `new` with a `(?i)` prefix
    /// so matching is case-insensitive. Empty list = never
    /// promotes to Critical. Substring-style keywords
    /// ("hack", "SEC") are valid regex and still work
    /// unchanged; richer patterns (`\bhack\b`, `SEC|fraud`,
    /// `crypto.*hack`) are now available.
    pub critical_keywords: Vec<String>,
    /// Regex patterns that promote to `High`.
    pub high_keywords: Vec<String>,
    /// Regex patterns that promote to `Low`.
    pub low_keywords: Vec<String>,
    /// Cooldown after entering `Critical`. After this many
    /// milliseconds with no fresh `Critical` headline the
    /// state reverts to `Normal`. Default: 30 minutes.
    pub critical_cooldown_ms: i64,
    /// Cooldown after entering `High`. Default: 5 minutes.
    pub high_cooldown_ms: i64,
    /// Cooldown after entering `Low`. Default: 0 (no
    /// cooldown — Low is alert-only and reverts on the
    /// next read).
    pub low_cooldown_ms: i64,
    /// Spread multiplier applied while in `High` state.
    /// Default 2.0.
    pub high_multiplier: Decimal,
    /// Spread multiplier applied while in `Critical` state.
    /// Default 3.0.
    pub critical_multiplier: Decimal,
}

impl Default for NewsRetreatConfig {
    fn default() -> Self {
        Self {
            critical_keywords: Vec::new(),
            high_keywords: Vec::new(),
            low_keywords: Vec::new(),
            critical_cooldown_ms: 30 * 60_000,
            high_cooldown_ms: 5 * 60_000,
            low_cooldown_ms: 0,
            high_multiplier: dec!(2),
            critical_multiplier: dec!(3),
        }
    }
}

/// State machine itself. Single-instance per process; for
/// multi-engine deployments share via
/// `Arc<Mutex<NewsRetreatStateMachine>>` (same pattern as
/// the asset-class kill switch).
#[derive(Debug, Clone)]
pub struct NewsRetreatStateMachine {
    config: NewsRetreatConfig,
    /// Compiled regex patterns. All are prefixed with the
    /// `(?i)` inline flag so matching is case-insensitive
    /// without the caller having to think about it.
    critical_re: Vec<Regex>,
    high_re: Vec<Regex>,
    low_re: Vec<Regex>,
    state: NewsRetreatState,
    entered_at_ms: i64,
}

impl NewsRetreatStateMachine {
    /// Build a fresh state machine. Compiles each pattern
    /// in the three priority lists exactly once, wrapping
    /// it in the `(?i)` inline flag so matching is
    /// case-insensitive. Returns `Err` if any pattern fails
    /// to compile, so operator config errors surface at
    /// startup instead of silently dropping headlines.
    pub fn new(config: NewsRetreatConfig) -> Result<Self> {
        let critical_re = compile_patterns("critical_keywords", &config.critical_keywords)?;
        let high_re = compile_patterns("high_keywords", &config.high_keywords)?;
        let low_re = compile_patterns("low_keywords", &config.low_keywords)?;
        Ok(Self {
            config,
            critical_re,
            high_re,
            low_re,
            state: NewsRetreatState::Normal,
            entered_at_ms: 0,
        })
    }

    /// Fold one headline into the state machine. Returns a
    /// transition tag the caller routes into audit + alert
    /// layers.
    ///
    /// Matching priority: `Critical → High → Low`. The
    /// first matching list wins; subsequent lists are not
    /// consulted.
    pub fn on_headline(&mut self, text: &str, now_ms: i64) -> NewsRetreatTransition {
        // Apply lazy cooldown expiry first so promotions
        // happen against an up-to-date state.
        let _ = self.current_state(now_ms);

        let class = self.classify(text);
        let Some(class) = class else {
            return NewsRetreatTransition::NoMatch;
        };
        let target = NewsRetreatState::from(class);

        if target.rank() < self.state.rank() {
            return NewsRetreatTransition::Suppressed {
                class,
                current: self.state,
            };
        }
        if target == self.state {
            self.entered_at_ms = now_ms;
            return NewsRetreatTransition::Refreshed(self.state);
        }
        let from = self.state;
        self.state = target;
        self.entered_at_ms = now_ms;
        NewsRetreatTransition::Promoted { from, to: target }
    }

    /// Read the current state, applying lazy cooldown
    /// expiry. Mutates state internally — declared `&mut`
    /// because cooldown expiry is a state transition the
    /// caller must observe consistently.
    pub fn current_state(&mut self, now_ms: i64) -> NewsRetreatState {
        if matches!(self.state, NewsRetreatState::Normal) {
            return NewsRetreatState::Normal;
        }
        let cooldown = match self.state {
            NewsRetreatState::Normal => return NewsRetreatState::Normal,
            NewsRetreatState::Low => self.config.low_cooldown_ms,
            NewsRetreatState::High => self.config.high_cooldown_ms,
            NewsRetreatState::Critical => self.config.critical_cooldown_ms,
        };
        let elapsed = now_ms.saturating_sub(self.entered_at_ms);
        if elapsed >= cooldown {
            self.state = NewsRetreatState::Normal;
        }
        self.state
    }

    /// Spread multiplier the autotuner applies while the
    /// retreat is active. Lazy-expires via [`Self::current_state`].
    pub fn current_multiplier(&mut self, now_ms: i64) -> Decimal {
        match self.current_state(now_ms) {
            NewsRetreatState::Normal | NewsRetreatState::Low => Decimal::ONE,
            NewsRetreatState::High => self.config.high_multiplier,
            NewsRetreatState::Critical => self.config.critical_multiplier,
        }
    }

    /// `true` only while the state machine is in
    /// `Critical`. The engine routes this through
    /// `KillSwitch::manual_trigger(StopNewOrders, ...)` on
    /// transition into `Critical`.
    pub fn should_stop_new_orders(&mut self, now_ms: i64) -> bool {
        matches!(self.current_state(now_ms), NewsRetreatState::Critical)
    }

    /// Force the state machine back to `Normal` regardless
    /// of cooldown. Operator-facing override (e.g. "I read
    /// the headline, it's a false alarm, resume quoting").
    pub fn force_clear(&mut self) {
        self.state = NewsRetreatState::Normal;
        self.entered_at_ms = 0;
    }

    fn classify(&self, text: &str) -> Option<NewsClass> {
        if self.critical_re.iter().any(|re| re.is_match(text)) {
            return Some(NewsClass::Critical);
        }
        if self.high_re.iter().any(|re| re.is_match(text)) {
            return Some(NewsClass::High);
        }
        if self.low_re.iter().any(|re| re.is_match(text)) {
            return Some(NewsClass::Low);
        }
        None
    }
}

/// Compile one priority list of raw pattern strings into
/// regexes. Each pattern is wrapped in `(?i)` so matching is
/// case-insensitive (operators don't have to normalise case
/// on either side). A compile failure surfaces with
/// `list_name` + the original pattern in the error context so
/// operators can pinpoint the offending config line.
fn compile_patterns(list_name: &str, patterns: &[String]) -> Result<Vec<Regex>> {
    patterns
        .iter()
        .map(|pat| {
            Regex::new(&format!("(?i){pat}"))
                .with_context(|| format!("{list_name}: failed to compile pattern `{pat}`"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_config() -> NewsRetreatConfig {
        NewsRetreatConfig {
            critical_keywords: vec!["hack".to_string(), "exploit".to_string(), "SEC".to_string()],
            high_keywords: vec!["FOMC".to_string(), "CPI".to_string()],
            low_keywords: vec!["partnership".to_string(), "listing".to_string()],
            critical_cooldown_ms: 30 * 60_000,
            high_cooldown_ms: 5 * 60_000,
            low_cooldown_ms: 0,
            high_multiplier: dec!(2),
            critical_multiplier: dec!(3),
        }
    }

    #[test]
    fn new_starts_in_normal() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        assert_eq!(sm.current_state(0), NewsRetreatState::Normal);
        assert_eq!(sm.current_multiplier(0), Decimal::ONE);
        assert!(!sm.should_stop_new_orders(0));
    }

    #[test]
    fn empty_config_classifies_everything_as_no_match() {
        let mut sm = NewsRetreatStateMachine::new(NewsRetreatConfig::default()).unwrap();
        let result = sm.on_headline("SEC charges Coinbase with fraud", 0);
        assert_eq!(result, NewsRetreatTransition::NoMatch);
        assert_eq!(sm.current_state(0), NewsRetreatState::Normal);
    }

    #[test]
    fn critical_keyword_promotes_to_critical() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        let result = sm.on_headline("Major exchange hack reported", 1000);
        assert_eq!(
            result,
            NewsRetreatTransition::Promoted {
                from: NewsRetreatState::Normal,
                to: NewsRetreatState::Critical,
            }
        );
        assert_eq!(sm.current_state(1000), NewsRetreatState::Critical);
        assert_eq!(sm.current_multiplier(1000), dec!(3));
        assert!(sm.should_stop_new_orders(1000));
    }

    #[test]
    fn high_keyword_promotes_to_high() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        let result = sm.on_headline("FOMC raises rates 25bp", 500);
        assert!(matches!(
            result,
            NewsRetreatTransition::Promoted {
                from: NewsRetreatState::Normal,
                to: NewsRetreatState::High,
            }
        ));
        assert_eq!(sm.current_multiplier(500), dec!(2));
        assert!(!sm.should_stop_new_orders(500));
    }

    #[test]
    fn low_keyword_does_not_widen_quotes() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        let result = sm.on_headline("New listing announcement", 0);
        assert!(matches!(
            result,
            NewsRetreatTransition::Promoted {
                from: NewsRetreatState::Normal,
                to: NewsRetreatState::Low,
            }
        ));
        // Low is alert-only — multiplier stays at 1.
        assert_eq!(sm.current_multiplier(0), Decimal::ONE);
        assert!(!sm.should_stop_new_orders(0));
    }

    #[test]
    fn classification_is_case_insensitive() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        // "sec" lowercase still hits "SEC" keyword.
        let result = sm.on_headline("sec investigation underway", 0);
        assert!(matches!(
            result,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::Critical,
                ..
            }
        ));
    }

    #[test]
    fn priority_ladder_critical_beats_high_keyword_match() {
        // A headline containing both "FOMC" (High) and
        // "hack" (Critical) classifies as Critical because
        // Critical is checked first.
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        let result = sm.on_headline("FOMC member's twitter hack", 0);
        assert!(matches!(
            result,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::Critical,
                ..
            }
        ));
    }

    #[test]
    fn lower_class_in_higher_state_is_suppressed() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("Critical hack just landed", 1000);
        // While in Critical, a fresh High headline does NOT
        // demote — it's a no-op suppression.
        let result = sm.on_headline("FOMC speaks at 2pm", 2000);
        assert_eq!(
            result,
            NewsRetreatTransition::Suppressed {
                class: NewsClass::High,
                current: NewsRetreatState::Critical,
            }
        );
        assert_eq!(sm.current_state(2000), NewsRetreatState::Critical);
    }

    #[test]
    fn refresh_resets_cooldown_clock() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("First hack alert", 1000);
        // Half the cooldown elapses.
        let halfway = 1000 + 15 * 60_000;
        // Fresh same-class headline at halfway → Refreshed.
        let result = sm.on_headline("Second hack alert", halfway);
        assert_eq!(
            result,
            NewsRetreatTransition::Refreshed(NewsRetreatState::Critical)
        );
        // 30 min after halfway → state still Critical
        // because the timer reset.
        let later = halfway + 29 * 60_000;
        assert_eq!(sm.current_state(later), NewsRetreatState::Critical);
    }

    #[test]
    fn cooldown_expiry_returns_to_normal() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("Critical hack now", 1000);
        assert_eq!(sm.current_state(1000), NewsRetreatState::Critical);
        // 30 min + 1 ms later → cooldown expired.
        let after = 1000 + 30 * 60_000 + 1;
        assert_eq!(sm.current_state(after), NewsRetreatState::Normal);
        assert_eq!(sm.current_multiplier(after), Decimal::ONE);
        assert!(!sm.should_stop_new_orders(after));
    }

    #[test]
    fn high_state_uses_5min_cooldown() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("FOMC press conference", 0);
        assert_eq!(sm.current_state(0), NewsRetreatState::High);
        // 5 min - 1 ms → still High.
        assert_eq!(sm.current_state(5 * 60_000 - 1), NewsRetreatState::High);
        // 5 min exactly → Normal.
        assert_eq!(sm.current_state(5 * 60_000), NewsRetreatState::Normal);
    }

    #[test]
    fn low_state_zero_cooldown_reverts_immediately() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("New listing announcement", 1000);
        // low_cooldown_ms is 0, so the very next read at the
        // SAME time-instant already expires the state.
        assert_eq!(sm.current_state(1000), NewsRetreatState::Normal);
    }

    #[test]
    fn force_clear_overrides_active_state() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("Major hack", 1000);
        assert_eq!(sm.current_state(1000), NewsRetreatState::Critical);
        sm.force_clear();
        assert_eq!(sm.current_state(2000), NewsRetreatState::Normal);
    }

    // ---------------- Stage-2 regex tests ----------------

    /// Word-boundary anchor: `\bhack\b` should match
    /// "exchange hack reported" but NOT "hackathon news".
    /// Stage-1 substring matcher would false-positive on
    /// "hackathon"; stage-2 fixes this.
    #[test]
    fn word_boundary_excludes_hackathon() {
        let config = NewsRetreatConfig {
            critical_keywords: vec![r"\bhack\b".to_string()],
            high_keywords: vec![],
            low_keywords: vec![],
            ..fixture_config()
        };
        let mut sm = NewsRetreatStateMachine::new(config).unwrap();
        let r1 = sm.on_headline("Major exchange hack reported", 1000);
        assert!(matches!(
            r1,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::Critical,
                ..
            }
        ));
        sm.force_clear();
        let r2 = sm.on_headline("ETHGlobal hackathon news roundup", 2000);
        assert_eq!(r2, NewsRetreatTransition::NoMatch);
    }

    /// Alternation: a single pattern can cover multiple
    /// literal keywords via `|`.
    #[test]
    fn alternation_pattern_matches_any_branch() {
        let config = NewsRetreatConfig {
            critical_keywords: vec![r"SEC|fraud|hack".to_string()],
            high_keywords: vec![],
            low_keywords: vec![],
            ..fixture_config()
        };
        let mut sm = NewsRetreatStateMachine::new(config).unwrap();
        for headline in [
            "SEC opens probe into major exchange",
            "fraud allegations surface",
            "another hack reported",
        ] {
            sm.force_clear();
            let r = sm.on_headline(headline, 0);
            assert!(
                matches!(
                    r,
                    NewsRetreatTransition::Promoted {
                        to: NewsRetreatState::Critical,
                        ..
                    }
                ),
                "headline {headline:?} should promote to Critical, got {r:?}"
            );
        }
    }

    /// Wildcard: `crypto.*hack` matches "crypto exchange
    /// hack" (text between the two literals is arbitrary).
    #[test]
    fn wildcard_pattern_matches_across_words() {
        let config = NewsRetreatConfig {
            critical_keywords: vec![r"crypto.*hack".to_string()],
            high_keywords: vec![],
            low_keywords: vec![],
            ..fixture_config()
        };
        let mut sm = NewsRetreatStateMachine::new(config).unwrap();
        let r = sm.on_headline("crypto exchange hack confirmed", 0);
        assert!(matches!(
            r,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::Critical,
                ..
            }
        ));
        sm.force_clear();
        assert_eq!(
            sm.on_headline("exchange hack confirmed", 0),
            NewsRetreatTransition::NoMatch
        );
    }

    /// Malformed regex must surface as an error from `new`
    /// rather than panicking or silently dropping the
    /// pattern.
    #[test]
    fn malformed_pattern_returns_error_from_new() {
        let config = NewsRetreatConfig {
            critical_keywords: vec!["[unclosed".to_string()],
            ..fixture_config()
        };
        let result = NewsRetreatStateMachine::new(config);
        assert!(result.is_err(), "malformed pattern should return Err");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("critical_keywords"),
            "error should mention list name, got: {err}"
        );
    }

    /// Case-insensitivity is preserved via the baked-in
    /// `(?i)` prefix — a mixed-case pattern still matches
    /// headlines in any case.
    #[test]
    fn regex_case_insensitive_by_default() {
        let config = NewsRetreatConfig {
            critical_keywords: vec!["HACK".to_string()],
            high_keywords: vec![],
            low_keywords: vec![],
            ..fixture_config()
        };
        let mut sm = NewsRetreatStateMachine::new(config).unwrap();
        let r = sm.on_headline("small-hack-reported overnight", 0);
        assert!(matches!(
            r,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::Critical,
                ..
            }
        ));
    }

    /// Legacy substring keywords are still valid regex and
    /// behave unchanged — operators who upgrade see no
    /// behaviour change on their v1 config.
    #[test]
    fn legacy_substring_keywords_still_work() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        let r = sm.on_headline("Major exchange hack reported", 1000);
        assert!(matches!(
            r,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::Critical,
                ..
            }
        ));
    }

    #[test]
    fn promotion_ladder_low_to_high_to_critical() {
        let mut sm = NewsRetreatStateMachine::new(fixture_config()).unwrap();
        sm.on_headline("Listing news", 1000);
        // Low has 0 cooldown so we re-enter via classify on
        // each call — but at the exact same moment the state
        // is Normal (cooldown=0 expires instantly). For the
        // promotion ladder test, use non-zero cooldowns or
        // call current_state at the same instant.
        let _ = sm.current_state(1000);
        // Promote to High at a fresh timestamp.
        let r1 = sm.on_headline("FOMC at 2pm", 2000);
        assert!(matches!(
            r1,
            NewsRetreatTransition::Promoted {
                to: NewsRetreatState::High,
                ..
            }
        ));
        let r2 = sm.on_headline("Big hack", 3000);
        assert!(matches!(
            r2,
            NewsRetreatTransition::Promoted {
                from: NewsRetreatState::High,
                to: NewsRetreatState::Critical,
            }
        ));
    }
}
