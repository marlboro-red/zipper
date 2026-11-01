use std::path::Path;

pub fn make_tree_deletable(root: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(metadata) = std::fs::symlink_metadata(root) {
            let mode = metadata.permissions().mode() | 0o700;
            let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(mode));
        }
        for entry in std::fs::read_dir(root).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                make_tree_deletable(&entry.path());
            }
        }
    }
    #[cfg(not(unix))]
    let _ = root;
}
