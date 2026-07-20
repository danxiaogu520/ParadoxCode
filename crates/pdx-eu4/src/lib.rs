//! Temporary compatibility facade for callers migrating to `pdx-rules` and `pdx-game-eu4`.

pub use pdx_game_eu4::{Eu4Profile, GAME_ID, bootstrap_model, bootstrap_rules, profile};
pub use pdx_rules::*;

/// Transitional name for [`RuleSet`]. New runtime code should use the generic name.
pub type Eu4Rules = RuleSet;

/// Transitional name for [`RuleHash`]. New runtime code should use the generic name.
pub type Eu4RuleHash = RuleHash;

#[cfg(test)]
mod tests {
    use super::{Eu4RuleHash, Eu4Rules, GAME_ID, bootstrap_rules};

    #[test]
    fn compatibility_aliases_preserve_the_eu4_profile_entrypoint() {
        let rules: Eu4Rules = bootstrap_rules();
        let hash: Eu4RuleHash = rules.rule_hash();
        assert_eq!(GAME_ID, "eu4");
        assert_ne!(hash.as_bytes(), [0; 32]);
    }
}
