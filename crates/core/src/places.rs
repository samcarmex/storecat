//! What a file or folder is, judged from its name and place: leftovers operating systems
//! sprinkle on drives, and copies that belong to an application rather than to you.

/// Files macOS and Windows leave behind that mean nothing to a person browsing a drive.
/// Hidden from search by default (`is:leftover` shows them).
pub fn is_os_leftover(name: &str) -> bool {
    is_cleanable_leftover(name) || name.eq_ignore_ascii_case("desktop.ini")
}

/// Whether a catalogued item is an operating-system leftover, hidden from search. As well
/// as loose leftover files, that covers the same files inside archives and everything in
/// the `__MACOSX` folder macOS adds to the zips it makes (AppleDouble copies of each file),
/// and that folder itself. `path` is the folder the item sits in.
pub fn is_leftover_item(item_type: crate::model::ItemType, name: &str, path: &str) -> bool {
    use crate::model::ItemType;
    let macosx = |s: &str| s.eq_ignore_ascii_case("__MACOSX");
    if macosx(name) {
        return item_type != ItemType::File;
    }
    match item_type {
        ItemType::File => is_os_leftover(name),
        ItemType::ArchiveEntry => is_os_leftover(name) || path.split(['/', '\\']).any(macosx),
        ItemType::Dir => false,
    }
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

/// Where an item sits, for ranking search results: most searches are for a person's own
/// files, not what Windows and apps keep for themselves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Place {
    /// Desktop, Documents, Downloads, Pictures and the like under a user's home folder.
    User,
    /// System, program and app-managed folders.
    System,
    Other,
}

/// The folders people keep their own things in, inside `Users/<name>/`.
const USER_FOLDERS: &[&str] = &[
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "videos",
    "music",
    "movies",
    "onedrive",
    "dropbox",
    "google drive",
    "icloud drive",
    "iclouddrive",
    "creative cloud files",
    "projects",
];

/// `path` is the item's full path relative to its drive, `/`-separated.
pub fn search_place(path: &str) -> Place {
    if is_app_managed(path) {
        return Place::System;
    }
    let segs: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    let eq = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    match segs.as_slice() {
        [first, ..]
            if [
                "$recycle.bin",
                "perflogs",
                "$winreagent",
                "msocache",
                "intel",
                "drivers",
            ]
            .iter()
            .any(|b| eq(first, b)) =>
        {
            Place::System
        }
        [users, name, folder, ..]
            if (eq(users, "users") || eq(users, "home"))
                && !["public", "default", "all users", "default user"]
                    .iter()
                    .any(|b| eq(name, b))
                && USER_FOLDERS.iter().any(|f| {
                    let l = folder.to_ascii_lowercase();
                    l == *f || l.starts_with(&format!("{f} - ")) || l.starts_with(&format!("{f}-"))
                }) =>
        {
            Place::User
        }
        _ => Place::Other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_places() {
        assert_eq!(search_place("Users/sam/Documents/Invoices"), Place::User);
        assert_eq!(
            search_place("Users/sam/OneDrive - Unknown Vision/Clients"),
            Place::User
        );
        assert_eq!(search_place("Users/sam/Desktop"), Place::User);
        assert_eq!(
            search_place("Users/sam/AppData/Local/Packages/x"),
            Place::System
        );
        assert_eq!(search_place("Windows/System32"), Place::System);
        assert_eq!(search_place("Program Files/Adobe"), Place::System);
        assert_eq!(search_place("$Recycle.Bin/S-1-5"), Place::System);
        assert_eq!(
            search_place("Users/sam/source/node_modules/x"),
            Place::System
        );
        assert_eq!(search_place("Users/Public/Documents"), Place::Other);
        assert_eq!(search_place("Clients/Equinox"), Place::Other);
        assert_eq!(search_place(""), Place::Other);
    }

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
    fn leftovers_inside_archives() {
        use crate::model::ItemType::*;
        let zip = "Music/Zeph vs Dot.zip";
        assert!(is_leftover_item(ArchiveEntry, "._Track.mp3", zip));
        assert!(is_leftover_item(
            ArchiveEntry,
            "Track.mp3",
            "Music/x.zip/__MACOSX/Zeph"
        ));
        assert!(is_leftover_item(ArchiveEntry, "__MACOSX", zip));
        assert!(is_leftover_item(Dir, "__MACOSX", "Downloads"));
        assert!(!is_leftover_item(
            ArchiveEntry,
            "Track.mp3",
            "Music/x.zip/Zeph"
        ));
        assert!(!is_leftover_item(Dir, "Zeph", "Music"));
        assert!(!is_leftover_item(File, "__MACOSX", "Music"));
        assert!(is_leftover_item(File, "._Track.mp3", "Music"));
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
