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
                let prefix = prefix.trim_end_matches('/');
                crate::safe_path(prefix)?;
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
        let placed = match self.prefix {
            Some(prefix) => name.strip_prefix(prefix)?.strip_prefix('/')?,
            None => match self.components {
                0 => name,
                count => name.splitn(count + 1, '/').nth(count)?,
            },
        };
        (!placed.is_empty()).then_some(placed)
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

#[derive(Default)]
pub(crate) struct PlacedNames {
    kinds: HashMap<String, bool>,
}

impl PlacedNames {
    pub(crate) fn add(&mut self, name: &str, directory: bool) -> Result<()> {
        for (slash, _) in name.match_indices('/') {
            if self.kinds.insert(name[..slash].to_owned(), true) == Some(false) {
                bail!("stripping the path leaves a file where {name:?} needs a folder");
            }
        }
        match self.kinds.insert(name.to_owned(), directory) {
            Some(true) if directory => Ok(()),
            Some(_) => bail!("stripping the path leaves two entries at {name:?}"),
            None => Ok(()),
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
            for components in 0..=5 {
                let placed = Placement::new(Strip::Components(components))
                    .unwrap()
                    .place(name);
                let expected = (parts.len() > components).then(|| parts[components..].join("/"));
                assert_eq!(
                    placed.map(str::to_owned),
                    expected,
                    "{name} less {components}"
                );
                checked += 1;
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
