mod black_and_white;
mod chrome;
mod cool;
mod fade;
mod golden;
mod matte;
mod noir;
mod none;
mod vintage;
mod vivid;
mod warm;

use crate::library::editing::EditState;

/// A named filter preset that produces a tuned `EditState` with specific
/// exposure and color values for a particular photographic look.
pub trait Filter: Send + Sync {
    /// Internal key used for serialisation (e.g. `"bw"`, `"vivid"`).
    fn name(&self) -> &'static str;

    /// User-facing display name (e.g. `"B&W"`, `"Vivid"`).
    fn display_name(&self) -> &'static str;

    /// Return an `EditState` with preset exposure/color values.
    fn preset(&self) -> EditState;
}

/// Return all built-in filters in display order, starting with the "none" filter.
pub fn filter_registry() -> Vec<Box<dyn Filter>> {
    vec![
        Box::new(none::None),
        Box::new(black_and_white::BlackAndWhite),
        Box::new(vivid::Vivid),
        Box::new(cool::Cool),
        Box::new(warm::Warm),
        Box::new(fade::Fade),
        Box::new(noir::Noir),
        Box::new(chrome::Chrome),
        Box::new(matte::Matte),
        Box::new(golden::Golden),
        Box::new(vintage::Vintage),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_by_name(name: &str) -> Option<Box<dyn Filter>> {
        filter_registry().into_iter().find(|f| f.name() == name)
    }

    #[test]
    fn registry_returns_all_filters() {
        let filters = filter_registry();
        assert_eq!(filters.len(), 11);
    }

    #[test]
    fn none_filter_is_first_and_identity() {
        let filters = filter_registry();
        let none = &filters[0];
        assert_eq!(none.name(), "none");
        assert!(none.preset().is_identity());
    }

    #[test]
    fn preset_filters_are_non_identity() {
        for filter in filter_registry().iter().skip(1) {
            let preset = filter.preset();
            assert!(
                !preset.is_identity(),
                "filter '{}' should not be identity",
                filter.name()
            );
        }
    }

    #[test]
    fn all_names_are_non_empty() {
        for filter in filter_registry() {
            assert!(!filter.name().is_empty());
            assert!(!filter.display_name().is_empty());
        }
    }

    #[test]
    fn preset_filters_have_filter_field_set() {
        for filter in filter_registry().iter().skip(1) {
            let preset = filter.preset();
            assert_eq!(
                preset.filter.as_deref(),
                Some(filter.name()),
                "filter '{}' preset should set filter field",
                filter.name()
            );
        }
    }

    #[test]
    fn filter_by_name_finds_known() {
        assert!(filter_by_name("bw").is_some());
        assert!(filter_by_name("vivid").is_some());
    }

    #[test]
    fn filter_by_name_returns_none_for_unknown() {
        assert!(filter_by_name("nonexistent").is_none());
    }
}
