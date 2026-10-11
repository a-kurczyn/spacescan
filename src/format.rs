//! Formatting values for display: sizes, counts, dates, permissions,
//! owners, file names and paths, and the natural name order.

use super::*;

/// "755 (rwxr-xr-x)" style permission summary.
pub(crate) fn format_perms(mode: u32) -> String {
    let bit = |m: u32, r: u32, w: u32, x: u32| {
        format!(
            "{}{}{}",
            if m & r != 0 { "r" } else { "-" },
            if m & w != 0 { "w" } else { "-" },
            if m & x != 0 { "x" } else { "-" },
        )
    };
    let sym = format!(
        "{}{}{}",
        bit(mode, 0o400, 0o200, 0o100),
        bit(mode, 0o040, 0o020, 0o010),
        bit(mode, 0o004, 0o002, 0o001),
    );
    format!("{:o} ({})", mode & 0o777, sym)
}

/// "drwxr-xr-x"-style mode string, as `ls -l` shows it (including the
/// setuid/setgid/sticky bits). `is_dir` covers a mode without type bits.
pub(crate) fn format_mode_ls(mode: u32, is_dir: bool) -> String {
    let kind = match mode & 0o170000 {
        0o040000 => 'd',
        0o120000 => 'l',
        0o060000 => 'b',
        0o020000 => 'c',
        0o010000 => 'p',
        0o140000 => 's',
        _ if is_dir => 'd',
        _ => '-',
    };
    let mut s = String::with_capacity(10);
    s.push(kind);
    for (shift, special, set_x, set_no_x) in [
        (6, 0o4000, 's', 'S'),
        (3, 0o2000, 's', 'S'),
        (0, 0o1000, 't', 'T'),
    ] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(match (mode & special != 0, bits & 1 != 0) {
            (true, true) => set_x,
            (true, false) => set_no_x,
            (false, true) => 'x',
            (false, false) => '-',
        });
    }
    s
}

/// Unix seconds as local "YYYY-MM-DD HH:MM", or "-" if unknown.
pub(crate) fn format_epoch(secs: i64) -> String {
    use chrono::TimeZone;
    if secs == NO_TIME {
        return "-".to_string();
    }
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => "-".to_string(),
    }
}

/// User and group names for uids and gids, cached.
pub(crate) fn format_owner(
    uid: u32,
    gid: u32,
    user_cache: &mut std::collections::HashMap<u32, String>,
    group_cache: &mut std::collections::HashMap<u32, String>,
) -> String {
    let user = user_cache
        .entry(uid)
        .or_insert_with(|| {
            uzers::get_user_by_uid(uid)
                .map(|u| u.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| uid.to_string())
        })
        .clone();
    let group = group_cache
        .entry(gid)
        .or_insert_with(|| {
            uzers::get_group_by_gid(gid)
                .map(|g| g.name().to_string_lossy().into_owned())
                .unwrap_or_else(|| gid.to_string())
        })
        .clone();
    format!("{user}:{group}")
}

/// `p`'s last part as shown (see `show_os`), or the whole path for "/".
pub(crate) fn file_name_of(p: &Path) -> String {
    p.file_name().map(show_os).unwrap_or_else(|| show_path(p))
}

/// A name for display that can't be mistaken for another, like `ls -b`:
/// invalid UTF-8 bytes appear as `\xFF`, control characters as `\n`,
/// `\t` or `\x1B` (`\u{85}` above ASCII, so it can't pass for a raw
/// byte), characters that show as nothing or rearrange the text around
/// them as `\u{200B}` (see `is_invisible`), and a backslash as `\\`.
pub(crate) fn show_os(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    // Nearly every name is valid UTF-8 with nothing to escape: a plain copy.
    if let Ok(text) = std::str::from_utf8(s.as_bytes()) {
        let plain = |c: char| c != '\\' && !c.is_control() && !is_invisible(c);
        let clean = if text.is_ascii() {
            text.bytes().all(|b| b >= 0x20 && b != 0x7f && b != b'\\')
        } else {
            text.chars().all(plain)
        };
        if clean {
            return text.to_owned();
        }
    }
    show_os_escaped(s)
}

/// The name `shown` stands for, read back from `show_os`'s escapes; None
/// if it has none, or one `show_os` never writes.
pub(crate) fn unshow_os(shown: &str) -> Option<std::ffi::OsString> {
    use std::os::unix::ffi::OsStringExt;
    if !shown.contains('\\') {
        return None;
    }
    let mut out: Vec<u8> = Vec::with_capacity(shown.len());
    let mut rest = shown;
    while let Some(at) = rest.find('\\') {
        out.extend_from_slice(&rest.as_bytes()[..at]);
        let esc = &rest[at + 1..];
        let (bytes, used): (Vec<u8>, usize) = match esc.chars().next()? {
            '\\' => (vec![b'\\'], 1),
            'n' => (vec![b'\n'], 1),
            't' => (vec![b'\t'], 1),
            'r' => (vec![b'\r'], 1),
            'x' => (vec![u8::from_str_radix(esc.get(1..3)?, 16).ok()?], 3),
            'u' => {
                let hex = esc.strip_prefix("u{")?.split_once('}')?.0;
                let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
                (c.to_string().into_bytes(), 3 + hex.len())
            }
            _ => return None,
        };
        out.extend_from_slice(&bytes);
        rest = &esc[used..];
    }
    out.extend_from_slice(rest.as_bytes());
    Some(std::ffi::OsString::from_vec(out))
}

/// `show_os` for a name that may need escapes.
fn show_os_escaped(s: &std::ffi::OsStr) -> String {
    use std::os::unix::ffi::OsStrExt;
    let mut out = String::with_capacity(s.len());
    for chunk in s.as_bytes().utf8_chunks() {
        let mut chars = chunk.valid().chars().peekable();
        // The character before, emoji presentation selectors skipped.
        let mut prev: Option<char> = None;
        while let Some(c) = chars.next() {
            match c {
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\t' => out.push_str("\\t"),
                '\r' => out.push_str("\\r"),
                c if c.is_control() && c.is_ascii() => {
                    out.push_str(&format!("\\x{:02X}", c as u32))
                }
                c if c.is_control() || (is_invisible(c) && !in_emoji(prev, c, chars.peek())) => {
                    out.push_str(&format!("\\u{{{:X}}}", c as u32))
                }
                c => out.push(c),
            }
            if !matches!(c, '\u{FE0E}' | '\u{FE0F}') {
                prev = Some(c);
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02X}"));
        }
    }
    out
}

/// True for a character that shows as nothing, or changes how the text
/// around it shows without showing itself: Unicode's default-ignorable
/// characters (zero-width spaces and joiners, direction controls, the soft
/// hyphen, the byte order mark, variation selectors, Hangul fillers, tags)
/// and the line and paragraph separators. Two names differing only in one
/// would look the same (SM-79).
fn is_invisible(c: char) -> bool {
    matches!(
        c as u32,
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x2028..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFFB
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
    )
}

/// True for an emoji, or a symbol that can show as one.
fn is_pictographic(c: char) -> bool {
    matches!(
        c as u32,
        0x00A9
            | 0x00AE
            | 0x203C
            | 0x2049
            | 0x2122
            | 0x2139
            | 0x2194..=0x21AA
            | 0x231A..=0x23FF
            | 0x24C2
            | 0x25AA..=0x27BF
            | 0x2934..=0x2935
            | 0x2B05..=0x2B55
            | 0x3030
            | 0x303D
            | 0x3297
            | 0x3299
            | 0x1F000..=0x1FAFF
    )
}

/// True if invisible `c`, between `prev` and `next`, is part of an emoji
/// and shows there: a joiner between two emoji (👨‍👩‍👧), or a presentation
/// selector after an emoji (❤️) or in a keycap (1️⃣).
fn in_emoji(prev: Option<char>, c: char, next: Option<&char>) -> bool {
    let emoji_before = prev.is_some_and(is_pictographic);
    match c {
        '\u{200D}' => emoji_before && next.is_some_and(|&n| is_pictographic(n)),
        '\u{FE0E}' | '\u{FE0F}' => {
            emoji_before
                || (prev.is_some_and(|p| p.is_ascii_digit() || p == '#' || p == '*')
                    && next == Some(&'\u{20E3}'))
        }
        _ => false,
    }
}

/// Sort key for file-manager name order: case-insensitive, numbers by value
/// ("file2" before "file10", "007" = "7"). Keys compare as plain bytes.
///
/// A run of digits becomes '0', its length without leading zeros (two
/// bytes), then the digits, so shorter numbers sort first.
pub(crate) fn natural_key(name: &str) -> Vec<u8> {
    let bytes = name.as_bytes();
    let mut key = Vec::with_capacity(bytes.len() + 4);
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &bytes[start..i];
            let digits = &run[run.iter().take_while(|&&c| c == b'0').count()..];
            let len = digits.len().min(u16::MAX as usize) as u16;
            key.push(b'0');
            key.extend_from_slice(&len.to_be_bytes());
            key.extend_from_slice(&digits[..len as usize]);
            continue;
        }
        let c = name[i..].chars().next().unwrap();
        if c.is_ascii() {
            key.push(c.to_ascii_lowercase() as u8);
        } else {
            // é sorts as e (just after plain e).
            let mut buf = [0u8; 4];
            for l in c.to_lowercase() {
                match l {
                    'ß' => key.extend_from_slice(b"ss"),
                    'æ' => key.extend_from_slice(b"ae"),
                    'œ' => key.extend_from_slice(b"oe"),
                    'ø' => key.push(b'o'),
                    'ł' => key.push(b'l'),
                    'đ' => key.push(b'd'),
                    'þ' => key.extend_from_slice(b"th"),
                    _ => {
                        use unicode_normalization::UnicodeNormalization;
                        for base in std::iter::once(l)
                            .nfd()
                            .filter(|b| !unicode_normalization::char::is_combining_mark(*b))
                        {
                            key.extend_from_slice(base.encode_utf8(&mut buf).as_bytes());
                        }
                    }
                }
            }
        }
        i += c.len_utf8();
    }
    key
}

/// Natural, case-insensitive name order (see `natural_key`), then byte
/// order for names that tie.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    natural_key(a).cmp(&natural_key(b)).then_with(|| a.cmp(b))
}

/// `s` cut to at most `max` characters by replacing its middle with "…"
/// (keeping the end, where a path's name is).
pub(crate) fn shorten_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head = max / 3;
    let tail = max - head - 1;
    let mut out: String = s.chars().take(head).collect();
    out.push('…');
    out.extend(s.chars().skip(n - tail));
    out
}

/// A path for labels and dialogs: escaped like `show_path` and shortened to
/// about 240 characters.
pub(crate) fn short_path(p: &Path) -> String {
    shorten_middle(&show_path(p), 240)
}

/// A whole path for display, escaped like `show_os`.
pub(crate) fn show_path(p: &Path) -> String {
    show_os(p.as_os_str())
}

/// An I/O error as a plain-language line naming `path`.
pub(crate) fn friendly_io_error(path: &Path, e: &std::io::Error) -> String {
    trf("ERR_IO_LINE", &[&show_path(path), &io_reason(e)])
}

/// Plain-language reason for an I/O error ("not found", "don't have
/// permission…"), for error lines that name their subject themselves.
pub(crate) fn io_reason(e: &std::io::Error) -> String {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => tr("ERR_IO_NOT_FOUND"),
        PermissionDenied => tr("ERR_IO_PERMISSION"),
        _ => trf("ERR_IO_OTHER", &[&e.to_string()]),
    }
}

pub(crate) fn human_size(bytes: u64) -> String {
    // A total that hit the u64 ceiling (sums saturate) is only a lower bound.
    if bytes == u64::MAX {
        return "≥ 16 EiB".to_string();
    }
    // Powers of 1024, so binary (IEC) unit names.
    let units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB", "EiB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    // At most one decimal, and none when it would be ".0": "4 KiB",
    // "1.5 GiB", "95.4 MiB". Rounding can reach 1024 ("1024 KiB"): move up
    // a unit then.
    let mut rounded = (v * 10.0).round() / 10.0;
    if u > 0 && rounded >= 1024.0 && u < units.len() - 1 {
        rounded = (rounded / 1024.0 * 10.0).round() / 10.0;
        u += 1;
    }
    if u == 0 || rounded.fract() == 0.0 {
        format!("{} {}", rounded as u64, units[u])
    } else {
        format!("{:.1} {}", rounded, units[u])
    }
}

/// Thousands-separated integer, e.g. 1050824 -> "1,050,824".
pub(crate) fn format_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn names_are_unambiguous() {
        let bad = std::ffi::OsStr::from_bytes(b"x\xffy");
        let lookalike = std::ffi::OsStr::new("x\u{FFFD}y");
        assert_eq!(show_os(bad), "x\\xFFy");
        assert_eq!(show_os(lookalike), "x\u{FFFD}y");
        assert_eq!(
            show_os(std::ffi::OsStr::new("a\nb\tc\\d")),
            "a\\nb\\tc\\\\d"
        );
        // A control character above ASCII and the raw byte of the same
        // value look different (SM-76).
        assert_eq!(show_os(std::ffi::OsStr::new("a\u{80}b")), "a\\u{80}b");
        assert_eq!(show_os(std::ffi::OsStr::from_bytes(b"a\x80b")), "a\\x80b");
        assert_eq!(show_os(std::ffi::OsStr::new("c\u{9f}d")), "c\\u{9F}d");
        assert_eq!(show_os(std::ffi::OsStr::from_bytes(b"c\x9fd")), "c\\x9Fd");
        assert_eq!(show_os(std::ffi::OsStr::new("e\u{1b}")), "e\\x1B");
        assert_eq!(
            show_os(std::ffi::OsStr::new("ünïcödé 日本語")),
            "ünïcödé 日本語"
        );
    }

    /// The quick copy for plain names gives exactly what the careful,
    /// escaping path gives, for every kind of odd name.
    #[test]
    fn plain_names_take_the_quick_path_safely() {
        let names: &[&[u8]] = &[
            b"",
            b"plain.txt",
            b"with space",
            b"back\\slash",
            b"del\x7f",
            b"bell\x07",
            "next-line\u{85}".as_bytes(),
            "caf\u{e9}\u{9f}".as_bytes(),
            "\u{d55c}\u{ae00}.txt".as_bytes(),
            b"\xc3\xa9t\xc3\xa9\xff",
            b"\xc2",
            b"\xc3\x28",
            "emoji \u{1f600}".as_bytes(),
            "rtl \u{202e}".as_bytes(),
            "zero\u{200b}width".as_bytes(),
            "family \u{1f468}\u{200d}\u{1f469}".as_bytes(),
            "heart \u{2764}\u{fe0f}".as_bytes(),
            "\u{3164}".as_bytes(),
        ];
        for name in names {
            let os = std::ffi::OsStr::from_bytes(name);
            assert_eq!(show_os(os), show_os_escaped(os), "{name:?}");
        }
    }

    /// Every shown name reads back to exactly the name it stands for, so
    /// the escapes lose nothing; text that isn't one of them reads as None.
    #[test]
    fn shown_names_read_back() {
        let names: &[&[u8]] = &[
            b"back\\slash",
            b"\\u{200B} typed literally",
            b"tab\there\nand\rthere",
            b"bell\x07 del\x7f",
            b"raw \xff\x80 bytes",
            "c1 \u{85}\u{9f}".as_bytes(),
            "zero\u{200b}width \u{202e}rtl \u{2028}".as_bytes(),
            "\u{1F468}\u{200D}x and 1\u{FE0F}".as_bytes(),
        ];
        for name in names {
            let os = std::ffi::OsStr::from_bytes(name);
            let shown = show_os(os);
            assert_eq!(unshow_os(&shown).as_deref(), Some(os), "{shown}");
        }
        for not_escapes in ["plain", "a\\q", "a\\x4", "a\\u{110000}", "a\\u{20", "end\\"] {
            assert_eq!(unshow_os(not_escapes), None, "{not_escapes}");
        }
    }

    /// Two names differing only in a character that shows as nothing, or
    /// that rearranges the text around it, never look the same (SM-79);
    /// emoji built with joiners and presentation selectors stay as they
    /// are, and so does ordinary text.
    #[test]
    fn invisible_characters_are_shown() {
        let plain = show_os(std::ffi::OsStr::new("pq"));
        for x in [
            '\u{200B}',
            '\u{200C}',
            '\u{200D}',
            '\u{FEFF}',
            '\u{200E}',
            '\u{202E}',
            '\u{00AD}',
            '\u{2060}',
            '\u{2028}',
            '\u{2029}',
            '\u{2066}',
            '\u{FE0F}',
            '\u{034F}',
            '\u{3164}',
            '\u{E0041}',
        ] {
            let name = format!("p{x}q");
            let shown = show_os(std::ffi::OsStr::new(&name));
            assert_ne!(shown, plain, "{x:?}");
            assert_eq!(shown, format!("p\\u{{{:X}}}q", x as u32));
        }
        for kept in [
            "\u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}",
            "\u{2764}\u{FE0F}",
            "\u{2764}\u{FE0F}\u{200D}\u{1F525}",
            "1\u{FE0F}\u{20E3}",
            "\u{1F44D}\u{1F3FD}",
            "caf\u{E9} na\u{EF}ve \u{65E5}\u{672C}\u{8A9E} \u{D55C}\u{AE00} \u{1F600}",
            "e\u{301}",
        ] {
            assert_eq!(show_os(std::ffi::OsStr::new(kept)), kept);
        }
        // A joiner or selector outside an emoji is shown.
        assert_eq!(
            show_os(std::ffi::OsStr::new("\u{1F468}\u{200D}x")),
            "\u{1F468}\\u{200D}x"
        );
        assert_eq!(show_os(std::ffi::OsStr::new("1\u{FE0F}")), "1\\u{FE0F}");
    }

    #[test]
    fn natural_name_order() {
        let mut names = vec![
            "file10", "File2", "file1", ".dotfile", "Beta", "alpha", "b", "Ärger", "a007", "a7",
            "a07x", "Zed",
        ];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            names,
            vec![
                ".dotfile", "a007", "a7", "a07x", "alpha", "Ärger", "b", "Beta", "file1", "File2",
                "file10", "Zed"
            ]
        );
        assert_eq!(natural_cmp("abc", "abc"), std::cmp::Ordering::Equal);
        let mut accented = vec![
            "Zed", "émile", "Árbol", "abc", "Ñandú", "emile", "Øre", "nube", "Straße", "strasse",
        ];
        accented.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(
            accented,
            vec![
                "abc", "Árbol", "emile", "émile", "Ñandú", "nube", "Øre", "Straße", "strasse",
                "Zed"
            ]
        );
        assert_eq!(natural_cmp("x9", "x10"), std::cmp::Ordering::Less);
    }
}

#[cfg(test)]
mod format_tests {
    use super::*;
    #[test]
    fn huge_sizes_and_epoch_dates() {
        assert_eq!(human_size(u64::MAX), "≥ 16 EiB");
        assert_eq!(human_size(3 << 60), "3 EiB");
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1000), "1000 B");
        assert_eq!(human_size(4096), "4 KiB");
        assert_eq!(human_size(10 << 20), "10 MiB");
        assert_eq!(human_size(1536 << 20), "1.5 GiB");
        assert_eq!(human_size(100_033_331), "95.4 MiB");
        assert_eq!(human_size(759_069_900), "723.9 MiB");
        assert_eq!(human_size((1 << 20) - 1), "1 MiB"); // rounds up across the unit
        assert_eq!(human_size(12_000), "11.7 KiB");
        assert_eq!(u64::MAX.saturating_add(5), u64::MAX);
        assert_eq!(format_epoch(NO_TIME), "-");
        assert!(
            format_epoch(0).starts_with("1970-01-01") || format_epoch(0).starts_with("1969-12-31")
        );
    }
}

#[cfg(test)]
mod hangul_tests {
    use super::*;
    #[test]
    fn scan_notices_korean_names() {
        let dir = std::env::temp_dir().join(format!("spacescan-hangul-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("한국어.txt"), "x").unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let seen = std::sync::atomic::AtomicBool::new(false);
        let ctx = ScanCtx {
            mounts: &HashSet::new(),
            progress: &tx,
            cancel: &Default::default(),
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &seen,
            in_file_order: false,
            live: None,
        };
        let _ = scan_dir(&dir, &ctx);
        assert!(seen.load(std::sync::atomic::Ordering::Relaxed));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
