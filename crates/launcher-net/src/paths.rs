//! Portable lexical path checks shared by downloads and archive consumers.

use std::path::PathBuf;

use crate::Error;

/// Validates a relative path against Windows naming and alias rules.
///
/// # Errors
/// Rejects traversal, absolute/drive paths, aliases, device names, controls and
/// ambiguous trailing spaces/dots. Filesystem link checks remain the executor's job.
pub fn relative_path(value: &str) -> Result<PathBuf, Error> {
    if value.is_empty() || value.len() > 4096 || value.contains('\\') {
        return Err(rejected(value));
    }
    if value.split('/').any(is_unsafe_component) {
        return Err(rejected(value));
    }
    Ok(PathBuf::from(value))
}

/// Applies the same Windows-compatible component checks to downloads and archives.
pub(crate) fn is_unsafe_component(component: &str) -> bool {
    // Windows resolves a device name before the first dot and ignores trailing
    // spaces/dots in that base name, so "CON .txt" is still the CON device.
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    let numbered_device = stem
        .strip_prefix("COM")
        .or_else(|| stem.strip_prefix("LPT"))
        .is_some_and(|suffix| {
            matches!(
                suffix,
                "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
            )
        });
    component.is_empty()
        || matches!(component, "." | "..")
        || component.len() > 255
        || component.ends_with([' ', '.'])
        || component
            .chars()
            .any(|c| c.is_control() || matches!(c, ':' | '<' | '>' | '"' | '|' | '?' | '*'))
        || matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        )
        || numbered_device
}

fn rejected(value: &str) -> Error {
    Error::InvalidSpec(format!("unsafe relative path `{value}`"))
}

#[cfg(test)]
mod tests {
    use super::{is_unsafe_component, relative_path};

    #[test]
    fn rejects_windows_device_names_in_every_spelling() {
        for component in [
            "CON",
            "con.txt",
            "CON .txt",
            "CON..txt",
            "PRN",
            "AUX",
            "NUL",
            "CONIN$",
            "conout$.log",
            "COM1",
            "lpt9.jar",
            "COM¹",
            "LPT³",
            "trailing. ",
            "trailing ",
        ] {
            assert!(
                is_unsafe_component(component),
                "accepted unsafe component {component:?}"
            );
        }
        for component in ["console.txt", "com0", "com10", "lpt0", "auxiliary"] {
            assert!(
                !is_unsafe_component(component),
                "rejected safe component {component:?}"
            );
        }
    }

    #[test]
    fn accepts_game_relative_paths_and_rejects_aliases() {
        assert_eq!(
            relative_path("mods/示例.jar").expect("Unicode path"),
            std::path::PathBuf::from("mods/示例.jar")
        );
        for value in [
            "",
            "../escape",
            "mods/../escape",
            "mods//file.jar",
            "C:/file.jar",
            "mods\\file.jar",
            "mods/CONIN$.txt",
            "mods/COM¹.txt",
            "mods/file.jar.",
        ] {
            assert!(
                relative_path(value).is_err(),
                "accepted unsafe path {value:?}"
            );
        }
    }
}
