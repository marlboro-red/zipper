const CREATION_BOUND_FILES: usize = 1024;
pub(crate) const CREATION_BOUND_WORKERS: usize = 4;
#[cfg(feature = "zip-deflate")]
const TINY_FILE: u64 = 16 * 1024;
const SMALL_MEAN_FILE: u64 = 64 * 1024;

#[derive(Clone, Copy)]
pub(crate) enum CreationBound {
    #[cfg(feature = "zip-deflate")]
    Tiny,
    SmallMean,
}

impl CreationBound {
    pub(crate) fn holds(self, file_sizes: &[u64]) -> bool {
        if file_sizes.len() < CREATION_BOUND_FILES {
            return false;
        }
        match self {
            #[cfg(feature = "zip-deflate")]
            Self::Tiny => file_sizes.iter().all(|&size| size <= TINY_FILE),
            Self::SmallMean => {
                let total = file_sizes
                    .iter()
                    .fold(0u64, |sum, &size| sum.saturating_add(size));
                total / file_sizes.len() as u64 <= SMALL_MEAN_FILE
            }
        }
    }
}

#[cfg(all(test, feature = "zip-deflate"))]
mod tests {
    use super::*;

    #[test]
    fn small_files_cap_workers_only_when_many_and_small_on_average() {
        let small = vec![TINY_FILE; CREATION_BOUND_FILES];
        assert!(CreationBound::Tiny.holds(&small));
        assert!(CreationBound::SmallMean.holds(&small));
        assert!(!CreationBound::SmallMean.holds(&small[1..]));
        let mut with_two_large = vec![1_000; CREATION_BOUND_FILES];
        with_two_large[..2].fill(10 * 1024 * 1024);
        assert!(!CreationBound::Tiny.holds(&with_two_large));
        assert!(CreationBound::SmallMean.holds(&with_two_large));
        let mut over = vec![SMALL_MEAN_FILE; CREATION_BOUND_FILES];
        over[0] += CREATION_BOUND_FILES as u64;
        assert!(!CreationBound::SmallMean.holds(&over));
        let silesia_like = vec![17 * 1024 * 1024; 12];
        assert!(!CreationBound::SmallMean.holds(&silesia_like));
        let declared_huge = vec![u64::MAX; CREATION_BOUND_FILES];
        assert!(!CreationBound::SmallMean.holds(&declared_huge));
    }
}
