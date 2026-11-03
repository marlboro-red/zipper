use anyhow::{Result, ensure};

pub(crate) struct Pattern {
    text: Vec<char>,
    whole_path: bool,
}

impl Pattern {
    pub(crate) fn new(text: &str) -> Result<Self> {
        let whole_path = text.contains('/');
        let text = text.trim_end_matches('/');
        ensure!(
            !text.is_empty() && !text.starts_with('/') && !text.contains('\0'),
            "unusable pattern {text:?}"
        );
        Ok(Self {
            text: text.chars().collect(),
            whole_path,
        })
    }

    pub(crate) fn text(&self) -> String {
        self.text.iter().collect()
    }

    pub(crate) fn matches_entry_or_folder_above(&self, name: &str) -> bool {
        let mut ends = name
            .match_indices('/')
            .map(|(slash, _)| slash)
            .collect::<Vec<_>>();
        ends.push(name.len());
        ends.into_iter().any(|end| {
            let path = &name[..end];
            let subject = if self.whole_path {
                path
            } else {
                path.rsplit('/').next().unwrap_or(path)
            };
            glob(&self.text, &subject.chars().collect::<Vec<_>>())
        })
    }
}

fn glob(pattern: &[char], name: &[char]) -> bool {
    let mut matched = vec![vec![false; name.len() + 1]; pattern.len() + 1];
    matched[pattern.len()][name.len()] = true;
    for p in (0..pattern.len()).rev() {
        if pattern[p] == '*' && pattern.get(p + 1) == Some(&'*') {
            let after = p + 2;
            let skips_slash = pattern.get(after) == Some(&'/');
            let mut later = false;
            for n in (0..=name.len()).rev() {
                later |= matched[after][n];
                matched[p][n] = later || (skips_slash && matched[after + 1][n]);
            }
            continue;
        }
        for n in (0..=name.len()).rev() {
            matched[p][n] = if pattern[p] == '*' {
                matched[p + 1][n] || (n < name.len() && name[n] != '/' && matched[p][n + 1])
            } else if pattern[p] == '?' {
                n < name.len() && name[n] != '/' && matched[p + 1][n + 1]
            } else {
                n < name.len() && name[n] == pattern[p] && matched[p + 1][n + 1]
            };
        }
    }
    matched[0][0]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(pattern: &[char], name: &[char]) -> bool {
        match pattern {
            [] => name.is_empty(),
            ['*', '*', '/', rest @ ..] => {
                reference(rest, name)
                    || (0..=name.len()).any(|skip| reference(&pattern[2..], &name[skip..]))
            }
            ['*', '*', rest @ ..] => (0..=name.len()).any(|skip| reference(rest, &name[skip..])),
            ['*', rest @ ..] => {
                let segment = name.iter().position(|&c| c == '/').unwrap_or(name.len());
                (0..=segment).any(|skip| reference(rest, &name[skip..]))
            }
            ['?', rest @ ..] => {
                name.first().is_some_and(|&c| c != '/') && reference(rest, &name[1..])
            }
            [c, rest @ ..] => name.first() == Some(c) && reference(rest, &name[1..]),
        }
    }

    fn every(alphabet: &[u8], longest: usize) -> Vec<Vec<u8>> {
        let mut all = vec![Vec::new()];
        let mut previous = vec![Vec::new()];
        for _ in 0..longest {
            previous = previous
                .iter()
                .flat_map(|text| {
                    alphabet.iter().map(move |&byte| {
                        let mut next = text.clone();
                        next.push(byte);
                        next
                    })
                })
                .collect();
            all.extend(previous.iter().cloned());
        }
        all
    }

    #[test]
    fn glob_agrees_with_a_table_matcher_on_every_short_pattern_and_name() {
        let chars = |bytes: &Vec<u8>| bytes.iter().map(|&byte| byte as char).collect::<Vec<_>>();
        let patterns: Vec<Vec<char>> = every(b"a*?/", 5).iter().map(chars).collect();
        let names: Vec<Vec<char>> = every(b"ab/", 5).iter().map(chars).collect();
        for pattern in &patterns {
            for name in &names {
                assert_eq!(
                    glob(pattern, name),
                    reference(pattern, name),
                    "{:?} against {:?}",
                    pattern.iter().collect::<String>(),
                    name.iter().collect::<String>()
                );
            }
        }
    }

    #[test]
    fn a_folder_match_covers_its_subtree_and_a_bare_name_matches_at_any_depth() {
        let bare = Pattern::new("*.tmp").unwrap();
        assert!(bare.matches_entry_or_folder_above("a/b/c.tmp"));
        assert!(bare.matches_entry_or_folder_above("cache.tmp/inner/file"));
        assert!(!bare.matches_entry_or_folder_above("a/b/c.tmpx"));
        let folder = Pattern::new("build/*").unwrap();
        assert!(folder.matches_entry_or_folder_above("build/out/file"));
        assert!(!folder.matches_entry_or_folder_above("src/build/out"));
        let deep = Pattern::new("**/node_modules").unwrap();
        assert!(deep.matches_entry_or_folder_above("a/b/node_modules/x/y"));
        assert!(deep.matches_entry_or_folder_above("node_modules/x"));
        assert!(
            Pattern::new("a/**/b")
                .unwrap()
                .matches_entry_or_folder_above("a/b")
        );
        let top = Pattern::new("build/").unwrap();
        assert!(top.matches_entry_or_folder_above("build/a.o"));
        assert!(!top.matches_entry_or_folder_above("src/build/b.o"));
        assert!(
            Pattern::new("caf?.txt")
                .unwrap()
                .matches_entry_or_folder_above("café.txt")
        );
        let long: String = "a".repeat(400);
        assert!(
            !Pattern::new("*a*a*a*a*a*a*a*b")
                .unwrap()
                .matches_entry_or_folder_above(&long)
        );
        assert!(
            !Pattern::new("a/?")
                .unwrap()
                .matches_entry_or_folder_above("a/bc")
        );
        for unusable in ["", "/abs", "a\0b"] {
            assert!(Pattern::new(unusable).is_err(), "{unusable:?}");
        }
    }
}
