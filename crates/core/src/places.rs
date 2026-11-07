//! What a file or folder is, judged from its name and place: leftovers operating systems
//! sprinkle on drives, and copies that belong to an application rather than to you.

/// Files macOS and Windows leave behind that mean nothing to a person browsing a drive.
/// Hidden from search by default (`is:leftover` shows them).
pub fn is_os_leftover(name: &str) -> bool {
    is_cleanable_leftover(name) || name.eq_ignore_ascii_case("desktop.ini")
}

/// Leftovers that are safe to delete on Windows: macOS "AppleDouble" companions (`._name`,
/// holding Finder tags and resource forks), Finder's `.DS_Store`, and Windows' thumbnail
/// caches. `desktop.ini` is not one: Windows uses it for folder icons and names.
pub fn is_cleanable_leftover(name: &str) -> bool {
    (name.starts_with("._") && name.len() > 2)
        || [
            ".DS_Store",
            "Thumbs.db",
            "ehthumbs.db",
            "ehthumbs_vista.db",
            ".localized",
        ]
        .iter()
        .any(|n| name.eq_ignore_ascii_case(n))
}

/// The four bytes every AppleDouble file starts with.
pub const APPLEDOUBLE_MAGIC: [u8; 4] = [0x00, 0x05, 0x16, 0x07];

/// Whether a folder (a `/`-separated path relative to its drive) holds files an application
/// manages for itself: package caches, build output, tool folders, app data, system
/// folders. Duplicates there are the app's business; deleting one can break it. Temporary
/// folders are not included: copies left in Temp are fair game.
pub fn is_app_managed(dir_path: &str) -> bool {
    let segs: Vec<&str> = dir_path
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .collect();
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    let in_temp = segs.iter().any(|s| eq(s, "temp") || eq(s, "tmp"));
    if in_temp {
        return false;
    }
    // Tool and app folders: `.gradle`, `.vscode`, `.codex`, `.claude/worktrees`...
    if segs.iter().any(|s| s.starts_with('.') && s.len() > 1) {
        return true;
    }
    if segs.iter().any(|s| eq(s, "appdata")) {
        return true;
    }
    if crate::enrich::plan::is_bulk_path(dir_path, false) {
        return true;
    }
    // Build output.
    segs.windows(2).any(|w| {
        (eq(w[0], "target") && (eq(w[1], "debug") || eq(w[1], "release")))
            || (eq(w[0], "build") && eq(w[1], "intermediates"))
            || (eq(w[0], "obj") && (eq(w[1], "debug") || eq(w[1], "release")))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leftovers() {
        assert!(is_os_leftover("._Song.mp3"));
        assert!(is_os_leftover(".DS_Store"));
        assert!(is_os_leftover("Thumbs.db"));
        assert!(is_os_leftover("desktop.ini"));
        assert!(!is_cleanable_leftover("desktop.ini"));
        assert!(!is_os_leftover("._"));
        assert!(!is_os_leftover(".env"));
        assert!(!is_os_leftover("song.mp3"));
    }

    #[test]
    fn app_managed_places() {
        for p in [
            "Users/sam/.gradle/caches/8.8/transforms/x",
            "GitHub/site/.claude/worktrees/feature",
            "Users/sam/AppData/Roaming/Adobe/UXP",
            "Users/sam/AppData/Local/Packages/Clipchamp/LocalState",
            "GitHub/storage-search/target/debug/deps",
            "app/android/app/build/intermediates/cxx",
            "pentest/tool/.venv/Lib/site-packages/sdk",
            "Program Files/Adobe",
        ] {
            assert!(is_app_managed(p), "{p}");
        }
        for p in [
            "Users/sam/Desktop/PRINT_SAMPLES",
            "Users/sam/AppData/Local/Temp/abc_wetransfer",
            "Users/sam/Music/ALL MUSIC/USB BACKUP",
            "DCIM/100MEDIA",
            "Projects/target practice",
            "",
        ] {
            assert!(!is_app_managed(p), "{p}");
        }
    }
}
