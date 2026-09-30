//! Locating the plugin's own folder.

use std::path::{Path, PathBuf};

/// Platform folders of X-Plane's fat-plugin layout.
const PLATFORM_DIRS: [&str; 3] = ["win_x64", "mac_x64", "lin_x64"];

/// The plugin root (`.../Resources/plugins/FlyXTogether`) from the path of
/// the loaded `.xpl`, which sits either in a platform folder or directly in
/// the root.
pub fn plugin_root(xpl_file: &Path) -> PathBuf {
    let dir = xpl_file.parent().unwrap_or(Path::new("."));
    let in_platform_dir = dir
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| PLATFORM_DIRS.iter().any(|p| p.eq_ignore_ascii_case(n)));
    if in_platform_dir {
        dir.parent().unwrap_or(dir).to_path_buf()
    } else {
        dir.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_from_platform_folder() {
        let xpl = Path::new("X-Plane 12/Resources/plugins/FlyXTogether/win_x64/FlyXTogether.xpl");
        assert_eq!(
            plugin_root(xpl),
            Path::new("X-Plane 12/Resources/plugins/FlyXTogether")
        );
        let xpl = Path::new("/xp/Resources/plugins/FlyXTogether/mac_x64/FlyXTogether.xpl");
        assert_eq!(
            plugin_root(xpl),
            Path::new("/xp/Resources/plugins/FlyXTogether")
        );
    }

    #[test]
    fn root_from_flat_layout() {
        let xpl = Path::new("/xp/Resources/plugins/FlyXTogether/FlyXTogether.xpl");
        assert_eq!(
            plugin_root(xpl),
            Path::new("/xp/Resources/plugins/FlyXTogether")
        );
    }
}
