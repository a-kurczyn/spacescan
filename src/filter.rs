//! Filters: narrowing the chart and table to files matching name, size
//! and date criteria.

use super::*;

/// The filter panel's fields exactly as typed. An empty field means "no limit".
#[derive(Clone, Default, PartialEq)]
pub(crate) struct FilterForm {
    pub(crate) name: String,
    /// The "Aa" toggle: match names in exact case.
    pub(crate) case_sensitive: bool,
    pub(crate) min_size: String,
    pub(crate) max_size: String,
    pub(crate) min_created: String,
    pub(crate) max_created: String,
    pub(crate) min_modified: String,
    pub(crate) max_modified: String,
}

/// FilterForm parsed into something cheap to test every file against.
pub(crate) struct CompiledFilter {
    /// Name patterns (lowercased unless `case_sensitive`); a file matches if
    /// any one does. Patterns with `*` or `?` must match the whole name,
    /// others anywhere in it.
    pub(crate) names: Vec<String>,
    pub(crate) case_sensitive: bool,
    pub(crate) min_size: Option<u64>,
    pub(crate) max_size: Option<u64>,
    pub(crate) min_created: Option<i64>,
    pub(crate) max_created: Option<i64>,
    pub(crate) min_modified: Option<i64>,
    pub(crate) max_modified: Option<i64>,
}

impl CompiledFilter {
    /// Ok(None) when every field is empty (nothing to filter).
    pub(crate) fn compile(f: &FilterForm) -> Result<Option<Self>, String> {
        let size = |s: &str, what: &str| -> Result<Option<u64>, String> {
            if s.trim().is_empty() { Ok(None) } else { parse_size(s).map(Some).map_err(|e| trf("ERR_FIELD_PREFIX", &[what, &e])) }
        };
        let date = |s: &str, what: &str, end_of_day: bool| -> Result<Option<i64>, String> {
            if s.trim().is_empty() { Ok(None) } else { parse_date(s, end_of_day).map(Some).map_err(|e| trf("ERR_FIELD_PREFIX", &[what, &e])) }
        };
        let c = CompiledFilter {
            names: split_name_patterns(&f.name)
                .iter()
                .flat_map(|p| expand_alternatives(p))
                .map(|p| if f.case_sensitive { p } else { p.to_lowercase() })
                .collect(),
            case_sensitive: f.case_sensitive,
            min_size: size(&f.min_size, &tr("FILTER_ERR_MIN_SIZE"))?,
            max_size: size(&f.max_size, &tr("FILTER_ERR_MAX_SIZE"))?,
            min_created: date(&f.min_created, &tr("FILTER_ERR_CREATED_FROM"), false)?,
            max_created: date(&f.max_created, &tr("FILTER_ERR_CREATED_TO"), true)?,
            min_modified: date(&f.min_modified, &tr("FILTER_ERR_MODIFIED_FROM"), false)?,
            max_modified: date(&f.max_modified, &tr("FILTER_ERR_MODIFIED_TO"), true)?,
        };
        let empty = c.names.is_empty()
            && c.min_size.is_none() && c.max_size.is_none()
            && c.min_created.is_none() && c.max_created.is_none()
            && c.min_modified.is_none() && c.max_modified.is_none();
        Ok(if empty { None } else { Some(c) })
    }

    /// True if file `n` passes every filled-in field.
    pub(crate) fn matches_file(&self, n: &Node) -> bool {
        let in_range = |v: i64, lo: Option<i64>, hi: Option<i64>| lo.is_none_or(|lo| v >= lo) && hi.is_none_or(|hi| v <= hi);
        if self.min_size.is_some_and(|m| n.size < m) || self.max_size.is_some_and(|m| n.size > m) {
            return false;
        }
        // A file whose date is unknown fails any limit on that date.
        if (self.min_modified.is_some() || self.max_modified.is_some())
            && (n.mtime == NO_TIME || !in_range(n.mtime, self.min_modified, self.max_modified)) {
            return false;
        }
        if (self.min_created.is_some() || self.max_created.is_some())
            && (n.btime == 0 || !in_range(n.btime, self.min_created, self.max_created)) {
            return false;
        }
        if !self.names.is_empty() {
            let name = if self.case_sensitive { n.name.clone() } else { n.name.to_lowercase() };
            let hit = self.names.iter().any(|p| {
                if p.contains(['*', '?']) { glob_match(p, &name) } else { name.contains(p.as_str()) }
            });
            if !hit {
                return false;
            }
        }
        true
    }
}

/// Splits "*.iso, *.[mkv,mp4] backup" into patterns: whitespace, `,` and
/// `;` separate patterns, except inside `[...]`/`{...}` lists. Text in
/// double quotes is kept as one piece, spaces and commas included
/// (`"my file*"`), and `\` makes the next character literal (`my\ file`).
pub(crate) fn split_name_patterns(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut quoted = false;
    // Set once a pattern has begun, so a lone "" still counts as one.
    let mut started = false;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                if let Some(next) = chars.next() {
                    cur.push(next);
                }
                started = true;
            }
            '"' => {
                quoted = !quoted;
                started = true;
            }
            c if quoted => cur.push(c),
            '[' | '{' => { depth += 1; cur.push(ch); }
            ']' | '}' => { depth -= 1; cur.push(ch); }
            c if depth <= 0 && (c.is_whitespace() || c == ',' || c == ';') => {
                if started || !cur.is_empty() { out.push(std::mem::take(&mut cur)); }
                started = false;
            }
            c => cur.push(c),
        }
    }
    if started || !cur.is_empty() { out.push(cur); }
    out
}

/// Expands `*.[mkv,mp4]` (or `*.{mkv,mp4}`) into `*.mkv`, `*.mp4`. Brackets
/// here are comma-separated alternatives, not regex-style character classes.
pub(crate) fn expand_alternatives(p: &str) -> Vec<String> {
    let Some(open) = p.find(['[', '{']) else { return vec![p.to_string()] };
    let close_ch = if p.as_bytes()[open] == b'[' { ']' } else { '}' };
    let Some(close) = p[open..].find(close_ch).map(|i| open + i) else { return vec![p.to_string()] };
    let (head, inner, tail) = (&p[..open], &p[open + 1..close], &p[close + 1..]);
    inner
        .split(',')
        .map(str::trim)
        .flat_map(|alt| expand_alternatives(&format!("{head}{alt}{tail}")))
        .collect()
}

/// Whole-string wildcard match: `*` = any run of characters, `?` = one.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

/// "1.5G", "500 MB", "100k", "4096" (plain bytes). Units are powers of 1024.
pub(crate) fn parse_size(s: &str) -> Result<u64, String> {
    let s = s.trim().to_lowercase();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (num, unit) = (s[..split].trim(), s[split..].trim());
    let n: f64 = num.parse().map_err(|_| trf("ERR_SIZE_FORMAT", &[s.as_str()]))?;
    let mult: u64 = match unit.trim_end_matches("ib").trim_end_matches('b') {
        "" => 1,
        "k" => 1 << 10,
        "m" => 1 << 20,
        "g" => 1 << 30,
        "t" => 1 << 40,
        _ => return Err(trf("ERR_SIZE_UNIT", &[unit])),
    };
    Ok((n * mult as f64) as u64)
}

/// "2026-09-01" or "2026-09-01 14:30" in local time, as Unix seconds. A bare
/// date means the start of that day, or its last second for an upper limit.
pub(crate) fn parse_date(s: &str, end_of_day: bool) -> Result<i64, String> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};
    let s = s.trim();
    let dt = if let Ok(dt) = NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        dt
    } else {
        let d = NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| trf("ERR_DATE_FORMAT", &[s]))?;
        if end_of_day { d.and_hms_opt(23, 59, 59).unwrap() } else { d.and_hms_opt(0, 0, 0).unwrap() }
    };
    Local
        .from_local_datetime(&dt)
        .earliest()
        .map(|d| d.timestamp())
        .ok_or_else(|| trf("ERR_DATE_TZ", &[s]))
}

/// Copy of `n` keeping only files that match `f`, with folder sizes and file
/// counts recomputed from what's left. Folders with no matches are dropped.
pub(crate) fn filter_tree(n: &Node, f: &CompiledFilter) -> Option<Node> {
    filter_tree_by(n, &|file: &Node| f.matches_file(file))
}

/// Copy of `n` keeping only the files `keep` accepts, and the folders that
/// still contain one; folder sizes and counts cover what's kept.
pub(crate) fn filter_tree_by(n: &Node, keep: &(dyn Fn(&Node) -> bool + Sync)) -> Option<Node> {
    if !n.is_dir {
        return keep(n).then(|| n.clone());
    }
    let mut children: Vec<Node> = n.children.par_iter().filter_map(|c| deep(|| filter_tree_by(c, keep))).collect();
    if children.is_empty() {
        return None;
    }
    children.sort_by_key(|c| std::cmp::Reverse(c.size));
    Some(Node {
        name: n.name.clone(),
        path: n.path.clone(),
        size: children.iter().map(|c| c.size).fold(0u64, u64::saturating_add),
        file_count: children.iter().map(|c| c.file_count).sum(),
        is_dir: true,
        children,
        mode: n.mode,
        mtime: n.mtime,
        ctime: n.ctime,
        uid: n.uid,
        gid: n.gid,
        btime: n.btime,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_patterns_split_and_quote() {
        assert_eq!(split_name_patterns("*.iso, *.[mkv,mp4] backup"), vec!["*.iso", "*.[mkv,mp4]", "backup"]);
        assert_eq!(split_name_patterns(r#""sp ace*" x"#), vec!["sp ace*", "x"]);
        assert_eq!(split_name_patterns(r#"" leading space""#), vec![" leading space"]);
        assert_eq!(split_name_patterns(r"my\ file a\,b"), vec!["my file", "a,b"]);
    }
}
