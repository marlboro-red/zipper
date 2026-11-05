use anyhow::{Result, bail, ensure};
use std::collections::HashMap;

/// Leading path segments to remove from every entry before it is written.
/// An entry left with no segments, or outside the named prefix, is not
/// extracted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Strip<'a> {
    /// Keep the full archive path: `a/b/c.txt` lands at `<dest>/a/b/c.txt`.
    #[default]
    None,
    /// Extract only the subtree under this folder, at the destination root:
    /// with `a`, `a/b/c.txt` lands at `<dest>/b/c.txt`. The folder must exist
    /// in the archive, written as an entry or implied by one.
    Prefix(&'a str),
    /// Drop this many leading segments from every path, as tar's
    /// `--strip-components` does.
    Components(usize),
}

pub(crate) struct Placement<'a> {
    prefix: Option<&'a str>,
    components: usize,
}

impl<'a> Placement<'a> {
    pub(crate) fn new(strip: Strip<'a>) -> Result<Self> {
        Ok(match strip {
            Strip::None => Placement {
                prefix: None,
                components: 0,
            },
            Strip::Prefix(prefix) => {
                let mut prefix = prefix.trim_end_matches('/');
                while let Some(rest) = prefix.strip_prefix("./") {
                    prefix = rest;
                }
                crate::safe_path(prefix).map_err(|_| {
                    anyhow::anyhow!(
                        "the folder to strip, {prefix:?}, is not a relative archive path written with `/`"
                    )
                })?;
                Placement {
                    prefix: Some(prefix),
                    components: 0,
                }
            }
            Strip::Components(components) => Placement {
                prefix: None,
                components,
            },
        })
    }

    pub(crate) fn strips(&self) -> bool {
        self.prefix.is_some() || self.components > 0
    }

    pub(crate) fn place<'n>(&self, name: &'n str) -> Option<&'n str> {
        self.place_after_dots(name, 0)
    }

    pub(crate) fn place_after_dots<'n>(&self, name: &'n str, dots: usize) -> Option<&'n str> {
        let placed = match self.prefix {
            Some(prefix) => name.strip_prefix(prefix)?.strip_prefix('/')?,
            None => match self.components.saturating_sub(dots) {
                0 => name,
                count => {
                    let (slash, _) = name.match_indices('/').nth(count - 1)?;
                    &name[slash + 1..]
                }
            },
        };
        (!placed.is_empty()).then_some(placed)
    }

    pub(crate) fn is_prefix(&self, name: &str) -> bool {
        self.prefix == Some(name)
    }

    pub(crate) fn ensure_something_placed(&self, placed_any: bool) -> Result<()> {
        ensure!(
            placed_any || !self.strips(),
            "the strip leaves nothing to extract"
        );
        Ok(())
    }

    pub(crate) fn ensure_prefix_is_a_folder<'n>(
        &self,
        mut entries: impl Iterator<Item = (&'n str, bool)>,
    ) -> Result<()> {
        if let Some(prefix) = self.prefix {
            ensure!(
                entries.any(|(name, directory)| {
                    name == prefix && directory || self.place(name).is_some()
                }),
                "the folder to strip, {prefix}, is not a folder of the archive"
            );
        }
        Ok(())
    }
}

enum Placed {
    Folder,
    File { entry: Option<u32> },
}

#[derive(Default)]
pub(crate) struct PlacedNames {
    kinds: HashMap<String, Placed>,
}

impl PlacedNames {
    pub(crate) fn add(&mut self, name: &str, directory: bool) -> Result<()> {
        self.add_for(name, directory, None).map(|_| ())
    }

    pub(crate) fn add_entry(&mut self, name: &str, directory: bool, entry: u32) -> Result<bool> {
        self.add_for(name, directory, Some(entry))
    }

    fn add_for(&mut self, name: &str, directory: bool, entry: Option<u32>) -> Result<bool> {
        for (slash, _) in name.match_indices('/') {
            if let Some(Placed::File { .. }) =
                self.kinds.insert(name[..slash].to_owned(), Placed::Folder)
            {
                bail!("stripping the path leaves a file where {name:?} needs a folder");
            }
        }
        match self.kinds.get(name) {
            None => {
                let placed = if directory {
                    Placed::Folder
                } else {
                    Placed::File { entry }
                };
                self.kinds.insert(name.to_owned(), placed);
                Ok(false)
            }
            Some(Placed::Folder) if directory => Ok(true),
            Some(Placed::File {
                entry: Some(earlier),
            }) if !directory && entry == Some(*earlier) => Ok(true),
            Some(_) => bail!("stripping the path leaves two entries at {name:?}"),
        }
    }
}

pub(crate) fn ensure_placed_names_fit<'n>(
    entries: impl Iterator<Item = (&'n str, bool)>,
) -> Result<()> {
    let mut placed = PlacedNames::default();
    entries
        .into_iter()
        .try_for_each(|(name, directory)| placed.add(name, directory))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(depth: usize) -> Vec<String> {
        let mut all = Vec::new();
        let mut layer = vec![String::new()];
        for _ in 0..depth {
            layer = layer
                .iter()
                .flat_map(|parent| {
                    ["a", "b", "ab"].map(|part| {
                        if parent.is_empty() {
                            part.to_owned()
                        } else {
                            format!("{parent}/{part}")
                        }
                    })
                })
                .collect();
            all.extend(layer.clone());
        }
        all
    }

    #[test]
    fn every_short_path_lands_where_its_strip_says() {
        let paths = names(4);
        let mut checked = 0;
        for name in &paths {
            let parts: Vec<&str> = name.split('/').collect();
            for components in (0..=7).chain([usize::MAX - 1, usize::MAX]) {
                for dots in 0..=2 {
                    let placement = Placement::new(Strip::Components(components)).unwrap();
                    let placed = placement.place_after_dots(name, dots);
                    let mut with_dots = vec!["."; dots];
                    with_dots.extend(&parts);
                    let left: Vec<&str> = with_dots
                        .iter()
                        .skip(components)
                        .skip_while(|part| **part == ".")
                        .copied()
                        .collect();
                    let expected = (!left.is_empty()).then(|| left.join("/"));
                    assert_eq!(
                        placed.map(str::to_owned),
                        expected,
                        "{name} after {dots} dots less {components}"
                    );
                    checked += 1;
                }
            }
            for prefix in &paths {
                let placed = Placement::new(Strip::Prefix(prefix)).unwrap().place(name);
                let prefix_parts: Vec<&str> = prefix.split('/').collect();
                let expected = (parts.len() > prefix_parts.len()
                    && parts[..prefix_parts.len()] == prefix_parts[..])
                    .then(|| parts[prefix_parts.len()..].join("/"));
                assert_eq!(placed.map(str::to_owned), expected, "{name} under {prefix}");
                if let Some(placed) = placed {
                    assert_eq!(format!("{prefix}/{placed}"), *name);
                }
                checked += 1;
            }
            assert_eq!(
                Placement::new(Strip::None).unwrap().place(name),
                Some(name.as_str())
            );
        }
        assert!(checked > 10_000, "{checked}");
    }

    #[test]
    fn unsafe_prefixes_are_refused_and_a_trailing_slash_is_dropped() {
        for prefix in ["", "/a", "a/../b", "..", "a//b"] {
            assert!(Placement::new(Strip::Prefix(prefix)).is_err(), "{prefix:?}");
        }
        let placement = Placement::new(Strip::Prefix("a/")).unwrap();
        assert_eq!(placement.place("a/b"), Some("b"));
        assert_eq!(placement.place("a"), None);
        assert_eq!(placement.place("ab/c"), None);
    }

    #[test]
    fn a_file_lands_twice_at_one_path_only_when_one_archive_entry_repeats_there() {
        let paths = names(2);
        let mut checked = 0;
        let id = |name: &String| paths.iter().position(|path| path == name).unwrap() as u32;
        for first in &paths {
            for second in &paths {
                for (place_first, place_second) in [(first, first), (first, second)] {
                    let mut placed = PlacedNames::default();
                    assert!(!placed.add_entry(place_first, false, id(first)).unwrap());
                    let again = placed.add_entry(place_second, false, id(second));
                    let same_spot = place_first == place_second;
                    let under = place_second
                        .strip_prefix(place_first.as_str())
                        .is_some_and(|rest| rest.starts_with('/'))
                        || place_first
                            .strip_prefix(place_second.as_str())
                            .is_some_and(|rest| rest.starts_with('/'));
                    match (same_spot, first == second) {
                        (true, true) => assert!(again.unwrap(), "{first} twice"),
                        (true, false) => assert!(again.is_err(), "{first} then {second}"),
                        _ if under => assert!(again.is_err(), "{first} then {second}"),
                        _ => assert!(!again.unwrap(), "{first} then {second}"),
                    }
                    checked += 1;
                }
            }
        }
        let mut anonymous = PlacedNames::default();
        anonymous.add("a", false).unwrap();
        assert!(anonymous.add("a", false).is_err());
        assert!(checked > 100, "{checked}");
    }

    #[test]
    fn a_placed_file_holds_a_number_for_its_entry_not_a_copy_of_its_name() {
        assert!(size_of::<Placed>() <= 8, "{}", size_of::<Placed>());
    }

    #[test]
    fn placed_names_fit_exactly_when_no_file_is_doubled_or_used_as_a_folder() {
        let paths = names(2);
        let entries: Vec<(&str, bool)> = paths
            .iter()
            .flat_map(|name| [(name.as_str(), false), (name.as_str(), true)])
            .collect();
        let mut checked = 0;
        for first in &entries {
            for second in &entries {
                for third in &entries {
                    let set = [*first, *second, *third];
                    let doubled_file = (0..3).any(|i| {
                        (0..3).any(|j| i != j && set[i].0 == set[j].0 && !(set[i].1 && set[j].1))
                    });
                    let file_as_folder = set.iter().any(|&(file, directory)| {
                        !directory
                            && set.iter().any(|&(other, _)| {
                                other
                                    .strip_prefix(file)
                                    .is_some_and(|rest| rest.starts_with('/'))
                            })
                    });
                    assert_eq!(
                        ensure_placed_names_fit(set.into_iter()).is_ok(),
                        !doubled_file && !file_as_folder,
                        "{set:?}"
                    );
                    checked += 1;
                }
            }
        }
        assert!(checked > 10_000, "{checked}");
    }
}
