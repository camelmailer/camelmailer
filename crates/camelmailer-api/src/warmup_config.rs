//! The warm-up profiles and provider groups as the installation configured
//! them (`warmup.profiles`, `provider_groups`), shared by the API and the
//! worker.

use camelmailer_core::{ProviderGroup, ProviderGroupRule, ProviderGroups, WarmupProfiles};

/// The built-in profiles with the `warmup.profiles` changes applied.
pub fn profiles_from_config(config: &camelmailer_config::Warmup) -> WarmupProfiles {
    let mut profiles = WarmupProfiles::default();
    for (name, changes) in &config.profiles {
        let groups: Vec<(String, i64, i64)> = changes
            .groups
            .iter()
            .map(|(group, limits)| (group.clone(), limits.start, limits.cap))
            .collect();
        profiles.override_profile(name, changes.start, changes.growth, changes.cap, &groups);
    }
    profiles
}

/// The built-in provider groups with the `provider_groups` entries merged
/// in.
pub fn provider_groups_from_config(
    config: &[camelmailer_config::ProviderGroupConfig],
) -> ProviderGroups {
    let rules: Vec<ProviderGroupRule> = config
        .iter()
        .filter_map(|group| {
            Some(ProviderGroupRule {
                name: ProviderGroup::parse(&group.name)?,
                domains: group.domains.clone(),
                mx_patterns: group.mx_patterns.clone(),
            })
        })
        .collect();
    ProviderGroups::with_rules(&rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_changes_reach_the_profiles_and_groups() {
        let config = camelmailer_config::Config::from_yaml(
            "provider_groups:\n  - name: seznam\n    domains: [seznam.cz]\nwarmup:\n  profiles:\n    normal:\n      groups:\n        seznam: { start: 80, cap: 25000 }\n",
        )
        .unwrap();
        let profiles = profiles_from_config(&config.warmup);
        let seznam = ProviderGroup::parse("seznam").unwrap();
        assert_eq!(profiles.normal.ramp(&seznam).start, 80);
        assert_eq!(profiles.slow.ramp(&seznam).start, 50, "base of slow");
        let groups = provider_groups_from_config(&config.provider_groups);
        assert_eq!(groups.from_recipient_domain("seznam.cz"), Some(seznam));
    }
}
