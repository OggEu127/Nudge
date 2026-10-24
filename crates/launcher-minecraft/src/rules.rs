//! Pure evaluation of Mojang operating-system and launcher-feature rules.

use std::{cmp::Ordering, collections::BTreeMap, process::Command};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::Error;

/// Whether a matching rule permits or rejects an item.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Include this item.
    Allow,
    /// Exclude this item.
    Disallow,
}

/// Numeric operating-system version bounds used by recent Mojang metadata.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VersionRange {
    /// Inclusive lower bound.
    #[serde(default)]
    pub min: Option<String>,
    /// Exclusive upper bound.
    #[serde(default)]
    pub max: Option<String>,
}

/// Operating-system predicates. All supplied predicates must match.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OsCondition {
    /// Mojang OS name (`windows`, `linux`, or `osx`).
    #[serde(default)]
    pub name: Option<String>,
    /// Architecture regular expression.
    #[serde(default)]
    pub arch: Option<String>,
    /// Operating-system version regular expression.
    #[serde(default)]
    pub version: Option<String>,
    /// Numeric operating-system version bounds.
    #[serde(default)]
    pub version_range: Option<VersionRange>,
}

/// One ordered Mojang rule.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    /// Decision if every condition matches.
    pub action: Action,
    /// Optional operating-system conditions.
    #[serde(default)]
    pub os: Option<OsCondition>,
    /// Required launcher features. An unavailable feature is false.
    #[serde(default)]
    pub features: BTreeMap<String, bool>,
}

/// Explicit platform context, injectable for deterministic cross-platform tests.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Platform {
    /// Mojang OS name (`windows`, `linux`, or `osx`).
    pub os: String,
    /// Java-compatible architecture name.
    pub arch: String,
    /// Operating-system version, not the Minecraft version.
    pub version: String,
    /// Features enabled by the launch context.
    pub features: BTreeMap<String, bool>,
}

impl Platform {
    /// Detects the current operating system and CPU architecture.
    ///
    /// Version discovery uses fixed system commands without a shell. If discovery
    /// fails, version-specific predicates do not match the empty version.
    #[must_use]
    pub fn host() -> Self {
        let os = match std::env::consts::OS {
            "macos" => "osx",
            other => other,
        };
        let arch = match std::env::consts::ARCH {
            "x86" => "x86",
            "x86_64" if os == "windows" || os == "linux" => "amd64",
            "aarch64" => "aarch64",
            other => other,
        };
        Self {
            os: os.to_owned(),
            arch: arch.to_owned(),
            version: host_version(os),
            features: BTreeMap::new(),
        }
    }
}

fn host_version(os: &str) -> String {
    let result = match os {
        "windows" => Command::new("cmd.exe").args(["/D", "/C", "ver"]).output(),
        "osx" => Command::new("/usr/bin/sw_vers")
            .arg("-productVersion")
            .output(),
        _ => Command::new("uname").arg("-r").output(),
    };
    let Ok(output) = result else {
        return String::new();
    };
    if !output.status.success() {
        return String::new();
    }
    let value = String::from_utf8_lossy(&output.stdout);
    if os == "windows" {
        // `ver` is localized, but its dotted numeric build remains recognizable.
        value
            .split(|character: char| !character.is_ascii_digit() && character != '.')
            .find(|part| part.contains('.') && part.bytes().any(|byte| byte.is_ascii_digit()))
            .unwrap_or_default()
            .trim_matches('.')
            .to_owned()
    } else {
        value.trim().to_owned()
    }
}

/// Evaluates an ordered rule list using the official last-matching-rule policy.
///
/// An absent or empty rule list allows the item. A nonempty list starts denied;
/// every matching rule replaces that decision. Unknown features are false.
///
/// # Errors
///
/// Returns an error for invalid regular expressions or malformed version bounds.
pub fn allowed(rules: &[Rule], platform: &Platform) -> Result<bool, Error> {
    let mut decision = rules.is_empty();
    for rule in rules {
        if matches_rule(rule, platform)? {
            decision = rule.action == Action::Allow;
        }
    }
    Ok(decision)
}

fn matches_rule(rule: &Rule, platform: &Platform) -> Result<bool, Error> {
    if let Some(os) = &rule.os {
        if os.name.as_ref().is_some_and(|name| name != &platform.os) {
            return Ok(false);
        }
        if let Some(arch) = &os.arch
            && !regex_matches(arch, &platform.arch)?
        {
            return Ok(false);
        }
        if let Some(version) = &os.version
            && !regex_matches(version, &platform.version)?
        {
            return Ok(false);
        }
        if let Some(range) = &os.version_range
            && !range_matches(range, &platform.version)?
        {
            return Ok(false);
        }
    }
    Ok(rule
        .features
        .iter()
        .all(|(name, expected)| platform.features.get(name).copied().unwrap_or(false) == *expected))
}

fn regex_matches(expression: &str, value: &str) -> Result<bool, Error> {
    // Java's Matcher.matches() matches the whole value, unlike Regex::is_match.
    let regex = Regex::new(&format!("\\A(?:{expression})\\z")).map_err(|source| {
        Error::InvalidRuleExpression {
            expression: expression.to_owned(),
            source,
        }
    })?;
    Ok(regex.is_match(value))
}

fn range_matches(range: &VersionRange, version: &str) -> Result<bool, Error> {
    let min = range.min.as_deref().map(parse_bound).transpose()?;
    let max = range.max.as_deref().map(parse_bound).transpose()?;
    if let (Some(min), Some(max)) = (&min, &max)
        && compare_components(min, max) != Ordering::Less
    {
        return Err(Error::InvalidMetadata(
            "operating-system version range is empty or reversed".to_owned(),
        ));
    }
    // Kernel suffixes such as `6.8.0-generic` are not part of the numeric version.
    let numeric = version
        .split(|character: char| !character.is_ascii_digit() && character != '.')
        .next()
        .unwrap_or_default();
    let Ok(current) = parse_bound(numeric) else {
        return Ok(false);
    };
    Ok(min
        .as_ref()
        .is_none_or(|bound| compare_components(&current, bound) != Ordering::Less)
        && max
            .as_ref()
            .is_none_or(|bound| compare_components(&current, bound) == Ordering::Less))
}

fn parse_bound(value: &str) -> Result<Vec<u64>, Error> {
    if value.is_empty() {
        return Err(Error::InvalidMetadata(
            "operating-system version bound is empty".to_owned(),
        ));
    }
    value
        .split('.')
        .map(|part| {
            part.parse().map_err(|_| {
                Error::InvalidMetadata(format!(
                    "operating-system version bound `{value}` is not numeric"
                ))
            })
        })
        .collect()
}

fn compare_components(left: &[u64], right: &[u64]) -> Ordering {
    for index in 0..left.len().max(right.len()) {
        let result = left
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&right.get(index).copied().unwrap_or(0));
        if result != Ordering::Equal {
            return result;
        }
    }
    Ordering::Equal
}

#[cfg(test)]
mod tests {
    use super::{Platform, Rule, allowed};
    use std::collections::BTreeMap;

    fn windows() -> Platform {
        Platform {
            os: "windows".to_owned(),
            arch: "amd64".to_owned(),
            version: "10.0.17134".to_owned(),
            features: BTreeMap::new(),
        }
    }

    fn parse(json: &str) -> Vec<Rule> {
        serde_json::from_str(json).expect("valid test rules")
    }

    #[test]
    fn empty_allows_and_last_matching_rule_wins() {
        assert!(allowed(&[], &windows()).expect("empty rules"));
        let rules = parse(
            r#"[{"action":"allow"},{"action":"disallow","os":{"name":"windows"}},
                {"action":"allow","os":{"name":"linux"}}]"#,
        );
        assert!(!allowed(&rules, &windows()).expect("valid rules"));
    }

    #[test]
    fn features_default_false_and_all_conditions_must_match() {
        let rules = parse(
            r#"[{"action":"allow","os":{"name":"windows","arch":"amd64"},
                "features":{"is_demo_user":false}}]"#,
        );
        assert!(allowed(&rules, &windows()).expect("valid rules"));
        let mut platform = windows();
        platform.features.insert("is_demo_user".to_owned(), true);
        assert!(!allowed(&rules, &platform).expect("valid rules"));
    }

    #[test]
    fn regex_is_a_whole_string_match_and_invalid_patterns_fail() {
        let rules = parse(r#"[{"action":"allow","os":{"version":"10"}}]"#);
        assert!(!allowed(&rules, &windows()).expect("valid expression"));
        let rules = parse(r#"[{"action":"allow","os":{"version":"10\\..*"}}]"#);
        assert!(allowed(&rules, &windows()).expect("valid expression"));
        let rules = parse(r#"[{"action":"allow","os":{"arch":"["}}]"#);
        assert!(allowed(&rules, &windows()).is_err());
    }

    #[test]
    fn numeric_range_uses_inclusive_minimum_and_exclusive_maximum() {
        let newer = parse(r#"[{"action":"allow","os":{"versionRange":{"min":"10.0.17134"}}}]"#);
        let older = parse(r#"[{"action":"allow","os":{"versionRange":{"max":"10.0.17134"}}}]"#);
        assert!(allowed(&newer, &windows()).expect("valid range"));
        assert!(!allowed(&older, &windows()).expect("valid range"));
        let mut platform = windows();
        platform.version = "10.0.9999".to_owned();
        assert!(!allowed(&newer, &platform).expect("numeric ordering"));
        assert!(allowed(&older, &platform).expect("numeric ordering"));
    }

    #[test]
    fn rejects_malformed_numeric_range() {
        let rules = parse(r#"[{"action":"allow","os":{"versionRange":{"min":"ten"}}}]"#);
        assert!(allowed(&rules, &windows()).is_err());
    }
}
