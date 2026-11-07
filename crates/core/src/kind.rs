//! Extension → content kind. Deliberately a static table: it runs once per file during
//! scans, and users can search `kind:video` without us opening anything.

use crate::model::Kind;

pub fn kind_for_ext(ext: &str) -> Kind {
    // `ext` is already lowercase without the dot.
    match ext {
        // Video
        "mp4" | "mov" | "mkv" | "avi" | "m4v" | "mxf" | "mts" | "m2ts" | "webm" | "wmv" | "flv"
        | "mpg" | "mpeg" | "3gp" | "r3d" | "braw" | "ari" | "dv" | "vob" | "ts" | "prores"
        | "insv" | "360" => Kind::Video,
        // Images
        "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "tif" | "tiff"
        | "bmp" | "svg" | "ico" | "jxl" | "dng" | "cr2" | "cr3" | "nef" | "arw" | "raf" | "orf"
        | "rw2" | "pef" | "srw" | "x3f" | "exr" | "hdr" | "tga" | "psb" => Kind::Image,
        // Audio
        "wav" | "mp3" | "flac" | "aac" | "m4a" | "ogg" | "opus" | "aif" | "aiff" | "wma"
        | "alac" | "caf" | "mid" | "midi" | "ape" | "dsf" | "dff" => Kind::Audio,
        // Documents
        "pdf" | "doc" | "docx" | "xls" | "xlsx" | "ppt" | "pptx" | "odt" | "ods" | "odp"
        | "rtf" | "txt" | "md" | "markdown" | "csv" | "tsv" | "epub" | "mobi" | "pages"
        | "numbers" | "key" | "eml" | "msg" | "tex" | "log" | "json" | "xml" | "yaml" | "yml"
        | "toml" | "ini" | "cfg" => Kind::Document,
        // Archives
        "zip" | "7z" | "rar" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "lz" | "lzma"
        | "cab" | "arj" | "z" => Kind::Archive,
        // Code
        "rs" | "py" | "js" | "tsx" | "jsx" | "mjs" | "cjs" | "svelte" | "vue" | "html" | "htm"
        | "css" | "scss" | "less" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "java" | "kt"
        | "swift" | "go" | "rb" | "php" | "sh" | "ps1" | "bat" | "cmd" | "sql" | "lua" | "r"
        | "m" | "mm" | "dart" | "scala" | "zig" | "nim" | "ex" | "exs" | "erl" | "hs" | "jl"
        | "pl" | "vb" | "fs" | "ipynb" | "cts" => Kind::Code,
        // Disk images
        "iso" | "img" | "dmg" | "vhd" | "vhdx" | "vmdk" | "qcow2" | "wim" | "esd" | "bin"
        | "cue" | "nrg" => Kind::DiskImage,
        // Fonts
        "ttf" | "otf" | "woff" | "woff2" | "ttc" | "pfb" | "pfm" => Kind::Font,
        // Design / print / project files
        "ai" | "psd" | "indd" | "idml" | "eps" | "sketch" | "fig" | "xd" | "afdesign"
        | "afphoto" | "afpub" | "cdr" | "blend" | "c4d" | "aep" | "prproj" | "drp" | "fcpxml"
        | "als" | "logicx" | "ptx" | "flp" | "rpp" | "cpr" | "procreate" | "clip" | "kra"
        | "xcf" | "lrcat" | "cube" => Kind::Design,
        _ => Kind::Other,
    }
}

/// Kind for a real file: the extension table plus the few cases where size disambiguates.
/// `.ts` and `.mts` are both TypeScript and MPEG transport streams; no source file is
/// 20 MB. `.bin` is a CD image when it is big and any old blob of data when it is not.
pub fn kind_for_file(ext: &str, size: u64) -> Kind {
    match ext {
        "ts" | "mts" if size < 20 * 1024 * 1024 => Kind::Code,
        "bin" if size < 32 * 1024 * 1024 => Kind::Other,
        _ => kind_for_ext(ext),
    }
}

/// Lowercase extension without the dot, or empty. Treats dot-files as having no extension.
pub fn split_ext(name: &str) -> String {
    match name.rfind('.') {
        Some(0) | None => String::new(),
        Some(i) => {
            let ext = &name[i + 1..];
            if ext.len() > 12 || ext.is_empty() {
                return String::new();
            }
            ext.to_ascii_lowercase()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_extensions() {
        assert_eq!(kind_for_ext("mov"), Kind::Video);
        assert_eq!(kind_for_ext("cr3"), Kind::Image);
        assert_eq!(kind_for_ext("wav"), Kind::Audio);
        assert_eq!(kind_for_ext("pdf"), Kind::Document);
        assert_eq!(kind_for_ext("7z"), Kind::Archive);
        assert_eq!(kind_for_ext("ai"), Kind::Design);
        assert_eq!(kind_for_ext("weird"), Kind::Other);
    }

    #[test]
    fn ts_depends_on_size() {
        assert_eq!(kind_for_file("ts", 4_000), Kind::Code);
        assert_eq!(kind_for_file("ts", 900_000_000), Kind::Video);
        assert_eq!(kind_for_file("mov", 10), Kind::Video);
        assert_eq!(kind_for_file("mts", 4_000), Kind::Code);
        assert_eq!(kind_for_file("mts", 2_000_000_000), Kind::Video);
        assert_eq!(kind_for_file("bin", 9_000), Kind::Other);
        assert_eq!(kind_for_file("bin", 700_000_000), Kind::DiskImage);
    }

    #[test]
    fn splits_extensions() {
        assert_eq!(split_ext("Invoice.PDF"), "pdf");
        assert_eq!(split_ext(".gitignore"), "");
        assert_eq!(split_ext("archive.tar.gz"), "gz");
        assert_eq!(split_ext("noext"), "");
        assert_eq!(split_ext("trailing."), "");
    }
}
