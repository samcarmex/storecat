//! The search box grammar. Everything that is not a `key:value` filter is free text.
//!
//! ```text
//! invoice ext:pdf size:>2mb modified:2023 drive:seagate "exact phrase" kind:video is:dir
//! ```
//!
//! Keys: `ext`, `type`/`kind`, `size` (`>`, `<`, `>=`, `<=`, `a..b`), `modified`/`date`
//! (`2023`, `2023-05`, `2023-05-14`, `>2023`, `<2024-01`, `7d`, `3m`, `2y`), `after`/`since`,
//! `before`, `drive`/`volume`, `path`/`in`, `is` (`dir`/`folder`/`file`/`archived`),
//! `content`/`text` (words inside documents; quote for a phrase), `has`
//! (`thumb`/`text`/`meta`/`hash`).

use crate::model::{flags, ItemType, Kind};
use time::{Date, Month, OffsetDateTime};

#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedQuery {
    pub terms: Vec<String>,
    pub phrases: Vec<String>,
    pub path_terms: Vec<String>,
    pub exts: Vec<String>,
    pub kinds: Vec<Kind>,
    pub drives: Vec<String>,
    pub item_type: Option<ItemType>,
    /// Whole tags (lowercased) that must all be present.
    pub tags: Vec<String>,
    pub content_terms: Vec<String>,
    pub content_phrases: Vec<String>,
    /// Flag bits that must be set (see `model::flags`).
    pub has_flags: Vec<u64>,
    pub size_min: Option<u64>,
    pub size_max: Option<u64>,
    pub mtime_min: Option<i64>,
    pub mtime_max: Option<i64>,
    /// Free text with filters removed, for display.
    pub text: String,
}

impl ParsedQuery {
    pub fn is_empty(&self) -> bool {
        self == &ParsedQuery::default()
            || (self.terms.is_empty()
                && self.phrases.is_empty()
                && self.path_terms.is_empty()
                && self.exts.is_empty()
                && self.kinds.is_empty()
                && self.drives.is_empty()
                && self.item_type.is_none()
                && self.tags.is_empty()
                && self.content_terms.is_empty()
                && self.content_phrases.is_empty()
                && self.has_flags.is_empty()
                && self.size_min.is_none()
                && self.size_max.is_none()
                && self.mtime_min.is_none()
                && self.mtime_max.is_none())
    }
}

pub fn parse(input: &str) -> ParsedQuery {
    parse_at(input, OffsetDateTime::now_utc().unix_timestamp())
}

/// `now` is injectable so relative dates are testable.
pub fn parse_at(input: &str, now: i64) -> ParsedQuery {
    let mut q = ParsedQuery::default();
    let mut text_parts: Vec<String> = Vec::new();

    for tok in tokenize(input) {
        match tok {
            Token::Phrase(p) => {
                let p = p.trim().to_lowercase();
                if !p.is_empty() {
                    text_parts.push(format!("\"{p}\""));
                    q.phrases.push(p);
                }
            }
            Token::Word(w) => {
                if let Some((key, value)) = split_filter(&w) {
                    if apply_filter(&mut q, key, value, now) {
                        continue;
                    }
                }
                let lw = w.to_lowercase();
                // Split on characters the word tokenizer splits on, so "a_b" matches "a" and "b".
                for piece in lw
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|s| !s.is_empty())
                {
                    q.terms.push(piece.to_string());
                }
                text_parts.push(lw);
            }
        }
    }
    q.text = text_parts.join(" ");
    q
}

enum Token {
    Word(String),
    Phrase(String),
}

fn tokenize(input: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = input.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' | '“' | '”' => {
                if in_quote {
                    out.push(Token::Phrase(std::mem::take(&mut cur)));
                    in_quote = false;
                } else {
                    // A quote opening mid-word (drive:"My Disk") keeps the prefix.
                    if cur.ends_with(':') {
                        let key = std::mem::take(&mut cur);
                        let mut phrase = String::new();
                        for c2 in chars.by_ref() {
                            if c2 == '"' || c2 == '”' {
                                break;
                            }
                            phrase.push(c2);
                        }
                        out.push(Token::Word(format!("{key}{phrase}")));
                    } else {
                        if !cur.is_empty() {
                            out.push(Token::Word(std::mem::take(&mut cur)));
                        }
                        in_quote = true;
                    }
                }
            }
            c if c.is_whitespace() && !in_quote => {
                if !cur.is_empty() {
                    out.push(Token::Word(std::mem::take(&mut cur)));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        if in_quote {
            out.push(Token::Phrase(cur));
        } else {
            out.push(Token::Word(cur));
        }
    }
    out
}

fn split_filter(w: &str) -> Option<(&str, &str)> {
    let i = w.find(':')?;
    let key = &w[..i];
    let value = &w[i + 1..];
    if key.is_empty() || value.is_empty() || !key.chars().all(|c| c.is_ascii_alphabetic()) {
        return None;
    }
    Some((key, value))
}

/// Returns true when the key was recognised (and consumed).
fn apply_filter(q: &mut ParsedQuery, key: &str, value: &str, now: i64) -> bool {
    let key = key.to_ascii_lowercase();
    let v = value.trim();
    match key.as_str() {
        "ext" | "extension" => {
            for e in v.split(',') {
                let e = e.trim().trim_start_matches('.').to_ascii_lowercase();
                if !e.is_empty() {
                    q.exts.push(e);
                }
            }
            true
        }
        "kind" | "type" => {
            let mut any = false;
            for k in v.split(',') {
                if let Some(kind) = Kind::parse(&k.trim().to_ascii_lowercase()) {
                    q.kinds.push(kind);
                    any = true;
                }
            }
            any
        }
        "is" => match v.to_ascii_lowercase().as_str() {
            "dir" | "folder" | "directory" => {
                q.item_type = Some(ItemType::Dir);
                true
            }
            "file" => {
                q.item_type = Some(ItemType::File);
                true
            }
            "archived" | "packed" | "inside" | "entry" => {
                q.item_type = Some(ItemType::ArchiveEntry);
                true
            }
            _ => false,
        },
        "has" | "with" => {
            let mut any = false;
            for h in v.split(',') {
                let bit = match h.trim().to_ascii_lowercase().as_str() {
                    "thumb" | "thumbnail" | "preview" => flags::HAS_THUMB,
                    "text" | "content" => flags::HAS_TEXT,
                    "meta" | "metadata" | "exif" => flags::HAS_META,
                    "hash" => flags::HASHED,
                    _ => continue,
                };
                q.has_flags.push(bit);
                any = true;
            }
            any
        }
        "content" | "text" | "body" | "inside" => {
            let lw = v.to_lowercase();
            let words: Vec<String> = lw
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect();
            if words.is_empty() {
                return false;
            }
            if words.len() > 1 && v.contains(' ') {
                q.content_phrases.push(words.join(" "));
            } else {
                q.content_terms.extend(words);
            }
            true
        }
        "tag" | "tagged" | "label" => {
            for t in v.split(',') {
                let t = t.trim().to_lowercase();
                if !t.is_empty() {
                    q.tags.push(t);
                }
            }
            true
        }
        "drive" | "volume" | "disk" | "on" => {
            q.drives.push(v.to_lowercase());
            true
        }
        "path" | "in" | "folder" => {
            for piece in v
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|s| !s.is_empty())
            {
                q.path_terms.push(piece.to_string());
            }
            true
        }
        "size" | "larger" | "smaller" | "bigger" => {
            let (op, rest) = split_op(v);
            let op = match key.as_str() {
                "larger" | "bigger" => ">=",
                "smaller" => "<=",
                _ => op,
            };
            if let Some((a, b)) = rest.split_once("..") {
                match (parse_size(a), parse_size(b)) {
                    (Some(a), Some(b)) => {
                        q.size_min = Some(a);
                        q.size_max = Some(b);
                        return true;
                    }
                    _ => return false,
                }
            }
            let Some(n) = parse_size(rest) else {
                return false;
            };
            match op {
                ">" => q.size_min = Some(n.saturating_add(1)),
                ">=" | "" => q.size_min = Some(n),
                "<" => q.size_max = Some(n.saturating_sub(1)),
                "<=" => q.size_max = Some(n),
                "=" => {
                    q.size_min = Some(n);
                    q.size_max = Some(n);
                }
                _ => return false,
            }
            true
        }
        "modified" | "date" | "mtime" | "when" => {
            let (op, rest) = split_op(v);
            let Some((start, end)) = parse_date_range(rest, now) else {
                return false;
            };
            match op {
                ">" => q.mtime_min = Some(end),
                ">=" | "" if is_relative(rest) => q.mtime_min = Some(start),
                ">=" => q.mtime_min = Some(start),
                "<" => q.mtime_max = Some(start),
                "<=" => q.mtime_max = Some(end),
                "=" => {
                    q.mtime_min = Some(start);
                    q.mtime_max = Some(end);
                }
                _ => {
                    q.mtime_min = Some(start);
                    q.mtime_max = Some(end);
                }
            }
            true
        }
        "after" | "since" | "newer" => {
            let Some((start, _end)) = parse_date_range(v, now) else {
                return false;
            };
            q.mtime_min = Some(start);
            true
        }
        "before" | "older" => {
            let Some((start, _end)) = parse_date_range(v, now) else {
                return false;
            };
            q.mtime_max = Some(start);
            true
        }
        _ => false,
    }
}

fn split_op(v: &str) -> (&str, &str) {
    for op in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = v.strip_prefix(op) {
            return (op, rest.trim());
        }
    }
    ("", v)
}

/// `1.5gb`, `500 mb`, `2048`, `3k`. Binary units.
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let num_end = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == ','))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(num_end);
    let num: f64 = num.replace(',', "").parse().ok()?;
    let mult: f64 = match unit.trim() {
        "" | "b" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024f64.powi(2),
        "g" | "gb" | "gib" => 1024f64.powi(3),
        "t" | "tb" | "tib" => 1024f64.powi(4),
        _ => return None,
    };
    Some((num * mult) as u64)
}

fn is_relative(s: &str) -> bool {
    let s = s.trim().to_ascii_lowercase();
    s.ends_with(['d', 'w', 'm', 'y']) && s[..s.len() - 1].chars().all(|c| c.is_ascii_digit())
        || matches!(
            s.as_str(),
            "today" | "yesterday" | "week" | "month" | "year"
        )
}

/// Returns `[start, end)` as unix seconds for a date expression.
fn parse_date_range(s: &str, now: i64) -> Option<(i64, i64)> {
    let s = s.trim().to_ascii_lowercase();
    const DAY: i64 = 86_400;
    match s.as_str() {
        "today" => {
            let start = now - now.rem_euclid(DAY);
            return Some((start, start + DAY));
        }
        "yesterday" => {
            let start = now - now.rem_euclid(DAY) - DAY;
            return Some((start, start + DAY));
        }
        "week" => return Some((now - 7 * DAY, now + 1)),
        "month" => return Some((now - 30 * DAY, now + 1)),
        "year" => return Some((now - 365 * DAY, now + 1)),
        _ => {}
    }
    if is_relative(&s) {
        let n: i64 = s[..s.len() - 1].parse().ok()?;
        let secs = match s.chars().last()? {
            'd' => n * DAY,
            'w' => n * 7 * DAY,
            'm' => n * 30 * DAY,
            'y' => n * 365 * DAY,
            _ => return None,
        };
        return Some((now - secs, now + 1));
    }
    let parts: Vec<&str> = s.split(['-', '/', '.']).collect();
    let year: i32 = parts.first()?.parse().ok()?;
    if !(1970..=2200).contains(&year) {
        return None;
    }
    match parts.len() {
        1 => {
            let start = Date::from_calendar_date(year, Month::January, 1).ok()?;
            let end = Date::from_calendar_date(year + 1, Month::January, 1).ok()?;
            Some((midnight(start), midnight(end)))
        }
        2 => {
            let m: u8 = parts[1].parse().ok()?;
            let month = Month::try_from(m).ok()?;
            let start = Date::from_calendar_date(year, month, 1).ok()?;
            let (ny, nm) = if m == 12 {
                (year + 1, 1)
            } else {
                (year, m + 1)
            };
            let end = Date::from_calendar_date(ny, Month::try_from(nm).ok()?, 1).ok()?;
            Some((midnight(start), midnight(end)))
        }
        3 => {
            let m: u8 = parts[1].parse().ok()?;
            let d: u8 = parts[2].parse().ok()?;
            let start = Date::from_calendar_date(year, Month::try_from(m).ok()?, d).ok()?;
            let end = start.next_day()?;
            Some((midnight(start), midnight(end)))
        }
        _ => None,
    }
}

fn midnight(d: Date) -> i64 {
    d.midnight().assume_utc().unix_timestamp()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn free_text_and_phrases() {
        let q = parse(r#"ACME invoice "final cut" tax_2023"#);
        assert_eq!(q.terms, vec!["acme", "invoice", "tax", "2023"]);
        assert_eq!(q.phrases, vec!["final cut"]);
        assert_eq!(q.text, r#"acme invoice "final cut" tax_2023"#);
    }

    #[test]
    fn filters() {
        let q =
            parse("ext:PDF,docx kind:video,image is:dir drive:\"Grey Seagate\" path:Clients/ACME");
        assert_eq!(q.exts, vec!["pdf", "docx"]);
        assert_eq!(q.kinds, vec![Kind::Video, Kind::Image]);
        assert_eq!(q.item_type, Some(ItemType::Dir));
        assert_eq!(q.drives, vec!["grey seagate"]);
        assert_eq!(q.path_terms, vec!["clients", "acme"]);
        assert!(q.terms.is_empty());
    }

    #[test]
    fn sizes() {
        assert_eq!(
            parse("size:>1gb").size_min,
            Some(1 << 30).map(|n: u64| n + 1)
        );
        assert_eq!(parse("size:<=500mb").size_max, Some(500 << 20));
        let q = parse("size:1mb..2mb");
        assert_eq!((q.size_min, q.size_max), (Some(1 << 20), Some(2 << 20)));
        assert_eq!(parse("larger:2k").size_min, Some(2048));
        assert_eq!(parse_size("1.5 GB"), Some((1.5 * 1024f64.powi(3)) as u64));
    }

    #[test]
    fn dates() {
        let q = parse("modified:2023");
        assert_eq!(q.mtime_min, Some(1_672_531_200));
        assert_eq!(q.mtime_max, Some(1_704_067_200));
        let q = parse("modified:2023-05");
        assert_eq!(q.mtime_min, Some(1_682_899_200));
        assert_eq!(q.mtime_max, Some(1_685_577_600));
        let q = parse("modified:2024-02-29");
        assert_eq!(q.mtime_max.unwrap() - q.mtime_min.unwrap(), 86_400);
        let q = parse("before:2020 after:2018-06");
        assert_eq!(q.mtime_max, Some(1_577_836_800));
        assert_eq!(q.mtime_min, Some(1_527_811_200));
        let now = 1_700_000_000;
        let q = parse_at("modified:7d", now);
        assert_eq!(q.mtime_min, Some(now - 7 * 86_400));
        let q = parse_at("modified:>2023", now);
        assert_eq!(q.mtime_min, Some(1_704_067_200));
        assert_eq!(q.mtime_max, None);
    }

    #[test]
    fn content_and_has() {
        let q = parse(r#"content:unpaid text:"grand total" has:thumb,text"#);
        assert_eq!(q.content_terms, vec!["unpaid"]);
        assert_eq!(q.content_phrases, vec!["grand total"]);
        assert_eq!(q.has_flags, vec![flags::HAS_THUMB, flags::HAS_TEXT]);
        assert!(q.terms.is_empty());
    }

    #[test]
    fn unknown_keys_are_text() {
        let q = parse("foo:bar http://x");
        assert!(q.terms.contains(&"foo".to_string()));
        assert!(q.terms.contains(&"bar".to_string()));
        assert!(q.terms.contains(&"http".to_string()));
    }
}
