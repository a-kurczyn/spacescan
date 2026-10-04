//! Command-line use: `spacemap --help` lists the commands. A command scans
//! a folder and prints what the app's tables show, as text, CSV or JSON.

use super::*;
use std::ffi::OsString;
use std::io::Write;

const HELP: &str = "\
spacemap — see what takes the space on your disks

Usage:
  spacemap                 start the app
  spacemap PATH            start the app and scan PATH
  spacemap COMMAND PATH [OPTIONS]

Commands:
  list   the folders and files directly in PATH, with their totals
  flat   every file under PATH, in all its subfolders
  exts   file extensions under PATH, with their sizes and file counts

Options:
  --format txt|csv|json    output format (default: txt)
  --sort size|files|name|modified|changed
                           order of list and flat (default: size; files: list only)
  --by bytes|files         order of exts (default: bytes)
  --reverse                reverse the order
  --limit N                flat: only the first N files
  --apparent-size          count file lengths instead of disk space used
  -h, --help               show this help
  -V, --version            show the version

Sizes in CSV and JSON are in bytes; times are local, as YYYY-MM-DDTHH:MM:SS±HH:MM.
Names that aren't valid UTF-8 are shown escaped, as in the app (\\xFF).
";

/// What the command line asks for.
#[derive(Debug, PartialEq)]
pub(crate) enum Run {
    /// Start the app, scanning this folder first if given.
    App(Option<PathBuf>),
    /// Done: exit with this code.
    Exit(i32),
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Command {
    List,
    Flat,
    Exts,
}

#[derive(Debug, PartialEq, Clone, Copy)]
enum Format {
    Txt,
    Csv,
    Json,
}

/// A command and its options, as given.
#[derive(Debug, PartialEq)]
struct Request {
    command: Command,
    path: PathBuf,
    format: Format,
    sort: SortColumn,
    by_files: bool,
    reverse: bool,
    limit: Option<usize>,
    apparent: bool,
}

/// Reads the command line (`args` without the program's name): `Ok(None)`
/// to start the app, `Err` with the reason when it's wrong.
fn parse(args: &[OsString]) -> Result<Result<Request, Run>, String> {
    let Some(first) = args.first() else {
        return Ok(Err(Run::App(None)));
    };
    let command = match first.to_str() {
        Some("-h" | "--help") => {
            print!("{HELP}");
            return Ok(Err(Run::Exit(0)));
        }
        Some("-V" | "--version") => {
            println!("spacemap {}", env!("CARGO_PKG_VERSION"));
            return Ok(Err(Run::Exit(0)));
        }
        Some("list") => Command::List,
        Some("flat") => Command::Flat,
        Some("exts") => Command::Exts,
        Some(s) if s.starts_with('-') => return Err(format!("unknown option {s}")),
        // Anything else is a folder to open in the app.
        _ if args.len() == 1 => return Ok(Err(Run::App(Some(PathBuf::from(first))))),
        _ => return Err(format!("unknown command {}", first.to_string_lossy())),
    };
    let mut req = Request {
        command,
        path: PathBuf::new(),
        format: Format::Txt,
        sort: SortColumn::Size,
        by_files: false,
        reverse: false,
        limit: None,
        apparent: false,
    };
    let mut path = None;
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let text = arg.to_str().unwrap_or("");
        // "--opt value" or "--opt=value".
        let (name, inline) = match text.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v.to_string())),
            _ => (text, None),
        };
        let mut value = |what: &str| -> Result<String, String> {
            match inline.clone() {
                Some(v) => Ok(v),
                None => rest
                    .next()
                    .and_then(|v| v.to_str())
                    .map(str::to_string)
                    .ok_or_else(|| format!("{name} needs {what}")),
            }
        };
        match name {
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(Err(Run::Exit(0)));
            }
            "--format" => {
                req.format = match value("txt, csv or json")?.as_str() {
                    "txt" => Format::Txt,
                    "csv" => Format::Csv,
                    "json" => Format::Json,
                    other => return Err(format!("unknown format {other}")),
                }
            }
            "--sort" => {
                req.sort = match value("a column")?.as_str() {
                    "size" => SortColumn::Size,
                    "files" if command == Command::List => SortColumn::Files,
                    "name" => SortColumn::Name,
                    "modified" => SortColumn::Modified,
                    "changed" => SortColumn::Changed,
                    other => {
                        return Err(format!("can't sort {} by {other}", command_name(command)));
                    }
                }
            }
            "--by" => {
                req.by_files = match value("bytes or files")?.as_str() {
                    "bytes" => false,
                    "files" => true,
                    other => return Err(format!("--by takes bytes or files, not {other}")),
                }
            }
            "--limit" => {
                req.limit = Some(
                    value("a number")?
                        .parse()
                        .map_err(|_| format!("{name} needs a number"))?,
                )
            }
            "--reverse" => req.reverse = true,
            "--apparent-size" => req.apparent = true,
            s if s.starts_with('-') && s.len() > 1 => return Err(format!("unknown option {s}")),
            _ if path.is_none() => path = Some(PathBuf::from(arg)),
            _ => return Err("only one folder can be given".to_string()),
        }
    }
    // Options that only make sense for one command.
    if req.limit.is_some() && command != Command::Flat {
        return Err("--limit is for flat".to_string());
    }
    if req.by_files && command != Command::Exts {
        return Err("--by is for exts".to_string());
    }
    req.path = path.ok_or_else(|| format!("{} needs a folder", command_name(command)))?;
    Ok(Ok(req))
}

fn command_name(c: Command) -> &'static str {
    match c {
        Command::List => "list",
        Command::Flat => "flat",
        Command::Exts => "exts",
    }
}

/// Runs what the command line asks for, or says to start the app.
pub(crate) fn run(args: &[OsString]) -> Run {
    let req = match parse(args) {
        Ok(Ok(req)) => req,
        Ok(Err(run)) => return run,
        Err(why) => {
            eprintln!("spacemap: {why}\nTry 'spacemap --help'.");
            return Run::Exit(2);
        }
    };
    // Output in English, whatever the app's language.
    set_language("en");
    let path = true_case(&std::fs::canonicalize(&req.path).unwrap_or(req.path.clone()));
    match std::fs::metadata(&path) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            eprintln!("spacemap: {} is not a folder", show_path(&path));
            return Run::Exit(1);
        }
        Err(e) => {
            eprintln!("spacemap: {}", friendly_io_error(&path, &e));
            return Run::Exit(1);
        }
    }
    let (tree, problems) = scan_for_cli(&path, req.apparent);
    for p in &problems {
        eprintln!("spacemap: {p}");
    }
    let cats = CategoryModel::load().0;
    let out = std::io::stdout().lock();
    let mut out = std::io::BufWriter::new(out);
    let written = write_report(&mut out, &req, &tree, &cats).and_then(|()| out.flush());
    match written {
        Ok(()) => Run::Exit(0),
        // The reader stopped early (`| head`): not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Run::Exit(0),
        Err(e) => {
            eprintln!("spacemap: {e}");
            Run::Exit(1)
        }
    }
}

/// Scans `path` as the app does, without the live chart; also returns the
/// problems met (folders that couldn't be read…).
fn scan_for_cli(path: &Path, apparent: bool) -> (Node, Vec<String>) {
    let (tx, rx) = channel();
    let problems = std::thread::spawn(move || {
        rx.into_iter()
            .filter_map(|m| match m {
                ScanMsg::LogError(e) => Some(e),
                _ => None,
            })
            .collect::<Vec<String>>()
    });
    let mounts = mount_points();
    let ctx = ScanCtx {
        mounts: &mounts,
        progress: &tx,
        cancel: &Default::default(),
        apparent_size: apparent,
        hard_links: Default::default(),
        saw_hangul: &Default::default(),
        in_file_order: is_rotational(path),
        live: None,
    };
    let tree = scan_dir(path, &ctx);
    drop(ctx);
    drop(tx);
    (tree, problems.join().unwrap_or_default())
}

/// Writes the report `req` asks for about `tree`.
fn write_report(
    out: &mut impl Write,
    req: &Request,
    tree: &Node,
    cats: &CategoryModel,
) -> std::io::Result<()> {
    match req.command {
        Command::List => {
            let mut rows: Vec<&Node> = tree.children.iter().collect();
            sort_rows(&mut rows, req.sort, req.reverse);
            write_entries(out, req.format, tree, &rows, false)
        }
        Command::Flat => {
            let mut files = Vec::new();
            collect_files(tree, &mut files);
            sort_rows(&mut files, req.sort, req.reverse);
            files.truncate(req.limit.unwrap_or(usize::MAX));
            write_entries(out, req.format, tree, &files, true)
        }
        Command::Exts => write_extensions(out, req, tree, cats),
    }
}

fn collect_files<'a>(n: &'a Node, out: &mut Vec<&'a Node>) {
    for c in n.children.iter() {
        if c.is_dir {
            deep(|| collect_files(c, out));
        } else {
            out.push(c);
        }
    }
}

/// Sorts by `column` (sizes, counts and dates largest/newest first, names
/// A–Z), equal ones by name, then `reverse`d if asked.
fn sort_rows(rows: &mut [&Node], column: SortColumn, reverse: bool) {
    use std::cmp::Reverse;
    let by_name = |a: &&Node, b: &&Node| natural_cmp(&a.name, &b.name);
    rows.par_sort_by(|a, b| {
        let first = match column {
            SortColumn::Size => Reverse(a.size).cmp(&Reverse(b.size)),
            SortColumn::Files => Reverse(a.file_count).cmp(&Reverse(b.file_count)),
            SortColumn::Modified => Reverse(a.mtime).cmp(&Reverse(b.mtime)),
            SortColumn::Changed => Reverse(a.ctime).cmp(&Reverse(b.ctime)),
            _ => std::cmp::Ordering::Equal,
        };
        first
            .then_with(|| by_name(a, b))
            .then_with(|| a.disk_name().cmp(b.disk_name()))
    });
    if reverse {
        rows.reverse();
    }
}

/// A time for CSV and JSON ("" when unknown).
fn iso_time(secs: i64) -> String {
    use chrono::TimeZone;
    match chrono::Local.timestamp_opt(secs, 0) {
        chrono::LocalResult::Single(t) if secs != NO_TIME => {
            t.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
        }
        _ => String::new(),
    }
}

/// `field` as one CSV field: quoted when it holds a comma, quote or line
/// break, with quotes doubled.
fn csv_field(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_string()
    }
}

/// The entries of `list` (`rows` directly in `tree`) or `flat` (files at
/// any depth, shown by their path from `tree`).
fn write_entries(
    out: &mut impl Write,
    format: Format,
    tree: &Node,
    rows: &[&Node],
    flat: bool,
) -> std::io::Result<()> {
    let top = tree.path();
    // A row's name: its name, or in the flat list its path below `tree`.
    let name = |n: &Node| {
        if flat {
            let p = n.path();
            show_path(p.strip_prefix(&top).unwrap_or(&p))
        } else {
            n.name.to_string()
        }
    };
    let kind = |n: &Node| if n.is_dir { "folder" } else { "file" };
    match format {
        Format::Txt => {
            let folders = rows.iter().filter(|n| n.is_dir).count() as u64;
            writeln!(
                out,
                "{}: {} in {} files",
                show_path(&top),
                human_size(tree.size),
                format_count(tree.file_count)
            )?;
            if flat {
                writeln!(out, "{} files shown", format_count(rows.len() as u64))?;
            } else {
                let files = rows.len() as u64 - folders;
                writeln!(
                    out,
                    "{} folders, {} files directly in it",
                    format_count(folders),
                    format_count(files)
                )?;
            }
            writeln!(out)?;
            let size_w = rows
                .iter()
                .map(|n| human_size(n.size).len())
                .max()
                .unwrap_or(0)
                .max(4);
            let files_w = rows
                .iter()
                .map(|n| format_count(n.file_count).len())
                .max()
                .unwrap_or(0)
                .max(5);
            let perms_w = rows
                .iter()
                .map(|n| format_perms(n.mode).chars().count())
                .max()
                .unwrap_or(0)
                .max(5);
            if flat {
                writeln!(
                    out,
                    "{:>size_w$}  {:<16}  {:<16}  {:<perms_w$}  Path",
                    "Size", "Modified", "Changed", "Perms"
                )?;
            } else {
                writeln!(
                    out,
                    "{:<6}  {:>size_w$}  {:>files_w$}  {:<16}  {:<16}  {:<perms_w$}  Name",
                    "Type", "Size", "Files", "Modified", "Changed", "Perms"
                )?;
            }
            for n in rows {
                let (size, modified, changed, perms) = (
                    human_size(n.size),
                    format_epoch(n.mtime),
                    format_epoch(n.ctime),
                    format_perms(n.mode),
                );
                if flat {
                    writeln!(
                        out,
                        "{size:>size_w$}  {modified:<16}  {changed:<16}  {perms:<perms_w$}  {}",
                        name(n)
                    )?;
                } else {
                    writeln!(
                        out,
                        "{:<6}  {size:>size_w$}  {:>files_w$}  {modified:<16}  {changed:<16}  {perms:<perms_w$}  {}",
                        kind(n),
                        format_count(n.file_count),
                        name(n)
                    )?;
                }
            }
        }
        Format::Csv => {
            let head = if flat { "path" } else { "type,name" };
            writeln!(out, "{head},size,files,modified,changed,permissions")?;
            for n in rows {
                let first = if flat {
                    csv_field(&name(n))
                } else {
                    format!("{},{}", kind(n), csv_field(&name(n)))
                };
                writeln!(
                    out,
                    "{first},{},{},{},{},{}",
                    n.size,
                    n.file_count,
                    iso_time(n.mtime),
                    iso_time(n.ctime),
                    format_perms(n.mode)
                )?;
            }
        }
        Format::Json => {
            write!(
                out,
                "{{\"path\":{},\"size\":{},\"files\":{},\"entries\":[",
                serde_json::Value::from(show_path(&top)),
                tree.size,
                tree.file_count
            )?;
            for (i, n) in rows.iter().enumerate() {
                let mut entry = serde_json::Map::new();
                if !flat {
                    entry.insert("type".into(), kind(n).into());
                }
                entry.insert((if flat { "path" } else { "name" }).into(), name(n).into());
                entry.insert("size".into(), n.size.into());
                entry.insert("files".into(), n.file_count.into());
                entry.insert("modified".into(), iso_time(n.mtime).into());
                entry.insert("changed".into(), iso_time(n.ctime).into());
                entry.insert("permissions".into(), format_perms(n.mode).into());
                if i > 0 {
                    write!(out, ",")?;
                }
                write!(out, "\n{}", serde_json::Value::Object(entry))?;
            }
            writeln!(out, "\n]}}")?;
        }
    }
    Ok(())
}

/// The `exts` report: every extension under `tree` with its category, file
/// count, size and share.
fn write_extensions(
    out: &mut impl Write,
    req: &Request,
    tree: &Node,
    cats: &CategoryModel,
) -> std::io::Result<()> {
    let mut exts: Vec<(String, String, u64, u64)> = category_breakdown(tree, cats)
        .into_iter()
        .flat_map(|row| {
            let category = cats.label(row.cat);
            row.exts
                .into_iter()
                .map(move |(ext, size, files)| (ext, category.clone(), size, files))
        })
        .collect();
    let weight = |e: &(String, String, u64, u64)| if req.by_files { e.3 } else { e.2 };
    exts.sort_by(|a, b| weight(b).cmp(&weight(a)).then_with(|| a.0.cmp(&b.0)));
    if req.reverse {
        exts.reverse();
    }
    let total: u64 = exts.iter().map(weight).fold(0, u64::saturating_add).max(1);
    let share = |e: &(String, String, u64, u64)| weight(e) as f64 * 100.0 / total as f64;
    let shown = |ext: &str| {
        if ext.is_empty() {
            "(no extension)".to_string()
        } else {
            format!(".{ext}")
        }
    };
    match req.format {
        Format::Txt => {
            writeln!(
                out,
                "{}: {} in {} files, by {}",
                show_path(&tree.path()),
                human_size(tree.size),
                format_count(tree.file_count),
                if req.by_files { "files" } else { "bytes" }
            )?;
            writeln!(out)?;
            let ext_w = exts
                .iter()
                .map(|e| shown(&e.0).chars().count())
                .max()
                .unwrap_or(0)
                .max(9);
            let cat_w = exts
                .iter()
                .map(|e| e.1.chars().count())
                .max()
                .unwrap_or(0)
                .max(8);
            writeln!(
                out,
                "{:<ext_w$}  {:<cat_w$}  {:>12}  {:>10}  {:>6}",
                "Extension", "Category", "Files", "Size", "Share"
            )?;
            for e in &exts {
                writeln!(
                    out,
                    "{:<ext_w$}  {:<cat_w$}  {:>12}  {:>10}  {:>5.1}%",
                    shown(&e.0),
                    e.1,
                    format_count(e.3),
                    human_size(e.2),
                    share(e)
                )?;
            }
        }
        Format::Csv => {
            writeln!(out, "extension,category,files,size,share")?;
            for e in &exts {
                writeln!(
                    out,
                    "{},{},{},{},{:.2}",
                    csv_field(&e.0),
                    csv_field(&e.1),
                    e.3,
                    e.2,
                    share(e)
                )?;
            }
        }
        Format::Json => {
            write!(
                out,
                "{{\"path\":{},\"size\":{},\"files\":{},\"by\":\"{}\",\"extensions\":[",
                serde_json::Value::from(show_path(&tree.path())),
                tree.size,
                tree.file_count,
                if req.by_files { "files" } else { "bytes" }
            )?;
            for (i, e) in exts.iter().enumerate() {
                if i > 0 {
                    write!(out, ",")?;
                }
                let entry = serde_json::json!({
                    "extension": e.0,
                    "category": e.1,
                    "files": e.3,
                    "size": e.2,
                    "share": (share(e) * 100.0).round() / 100.0,
                });
                write!(out, "\n{entry}")?;
            }
            writeln!(out, "\n]}}")?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// A tree with odd names: a comma, a quote, a line break, a backslash,
    /// a leading space, bytes that aren't UTF-8; equal sizes; a subfolder.
    fn odd_tree(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("spacemap-cli-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub, \"quoted\"")).unwrap();
        let names: [&[u8]; 7] = [
            b"plain.txt",
            b"comma,name.csv",
            b"quote\"name.txt",
            b"line\nbreak.log",
            b"back\\slash.TXT",
            b" lead.md",
            b"bad\xffname.bin",
        ];
        for (i, n) in names.iter().enumerate() {
            std::fs::write(
                dir.join(std::ffi::OsStr::from_bytes(n)),
                vec![b'x'; 5000 * (i % 3 + 1)],
            )
            .unwrap();
        }
        std::fs::write(dir.join("sub, \"quoted\"/deep.txt"), vec![b'y'; 20_000]).unwrap();
        dir
    }

    fn report(req: &Request) -> String {
        let (tree, _) = scan_for_cli(&req.path, req.apparent);
        let mut out = Vec::new();
        write_report(&mut out, req, &tree, &CategoryModel::defaults()).unwrap();
        String::from_utf8(out).unwrap()
    }

    fn request(command: Command, path: &Path, format: Format) -> Request {
        Request {
            command,
            path: path.to_path_buf(),
            format,
            sort: SortColumn::Size,
            by_files: false,
            reverse: false,
            limit: None,
            apparent: false,
        }
    }

    /// Splits CSV text into rows of fields (quoted fields may hold commas,
    /// doubled quotes and line breaks).
    fn read_csv(text: &str) -> Vec<Vec<String>> {
        let (mut rows, mut row, mut field) = (Vec::new(), Vec::new(), String::new());
        let mut chars = text.chars().peekable();
        let mut quoted = false;
        while let Some(c) = chars.next() {
            match (c, quoted) {
                ('"', true) if chars.peek() == Some(&'"') => {
                    field.push('"');
                    chars.next();
                }
                ('"', _) => quoted = !quoted,
                (',', false) => row.push(std::mem::take(&mut field)),
                ('\n', false) => {
                    row.push(std::mem::take(&mut field));
                    rows.push(std::mem::take(&mut row));
                }
                (c, _) => field.push(c),
            }
        }
        rows
    }

    #[test]
    fn command_lines_are_read_or_refused() {
        assert_eq!(parse(&args(&[])), Ok(Err(Run::App(None))));
        assert_eq!(
            parse(&args(&["/tmp"])),
            Ok(Err(Run::App(Some("/tmp".into()))))
        );
        assert_eq!(parse(&args(&["--version"])), Ok(Err(Run::Exit(0))));
        assert_eq!(parse(&args(&["-V"])), Ok(Err(Run::Exit(0))));
        let ok = |a: &[&str]| parse(&args(a)).unwrap().unwrap();
        let r = ok(&[
            "flat",
            "/x",
            "--format=csv",
            "--limit",
            "5",
            "--sort",
            "name",
            "--reverse",
        ]);
        assert_eq!(
            (r.format, r.limit, r.sort, r.reverse),
            (Format::Csv, Some(5), SortColumn::Name, true)
        );
        let r = ok(&[
            "exts",
            "--by",
            "files",
            "/x",
            "--format",
            "json",
            "--apparent-size",
        ]);
        assert_eq!(
            (r.by_files, r.format, r.apparent, r.path),
            (true, Format::Json, true, "/x".into())
        );
        for bad in [
            &["--nope"][..],
            &["list"],
            &["list", "/a", "/b"],
            &["list", "/a", "--limit", "3"],
            &["flat", "/a", "--by", "files"],
            &["flat", "/a", "--sort", "files"],
            &["list", "/a", "--format", "xml"],
            &["list", "/a", "--limit"],
            &["flat", "/a", "--limit", "-1"],
            &["a", "b"],
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad:?}");
        }
    }

    /// CSV and JSON give back every name exactly as the app shows it, odd
    /// ones included; text lists them all; sizes are bytes; order is by
    /// size, equal sizes by name, and --reverse turns it around.
    #[test]
    fn list_and_flat_keep_every_name() {
        let dir = odd_tree("names");
        let (tree, _) = scan_for_cli(&dir, false);
        let mut want: Vec<String> = tree.children.iter().map(|c| c.name.to_string()).collect();
        want.sort();
        // CSV
        let csv = read_csv(&report(&request(Command::List, &dir, Format::Csv)));
        assert_eq!(
            csv[0],
            [
                "type",
                "name",
                "size",
                "files",
                "modified",
                "changed",
                "permissions"
            ]
        );
        let mut names: Vec<String> = csv[1..].iter().map(|r| r[1].clone()).collect();
        let sizes: Vec<u64> = csv[1..].iter().map(|r| r[2].parse().unwrap()).collect();
        assert!(sizes.is_sorted_by(|a, b| a >= b), "{sizes:?}");
        names.sort();
        assert_eq!(names, want);
        // JSON
        let json: serde_json::Value =
            serde_json::from_str(&report(&request(Command::List, &dir, Format::Json))).unwrap();
        let mut names: Vec<String> = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        assert_eq!(names, want);
        assert_eq!(json["files"], tree.file_count);
        // Text
        let txt = report(&request(Command::List, &dir, Format::Txt));
        assert!(want.iter().all(|n| txt.contains(n.as_str())));
        // Reversed, and the flat list with a limit.
        let mut rev = request(Command::List, &dir, Format::Csv);
        rev.reverse = true;
        let back: Vec<Vec<String>> = read_csv(&report(&rev));
        let fwd = read_csv(&report(&request(Command::List, &dir, Format::Csv)));
        assert_eq!(
            back[1..].iter().rev().cloned().collect::<Vec<_>>(),
            fwd[1..].to_vec()
        );
        let mut flat = request(Command::Flat, &dir, Format::Csv);
        flat.limit = Some(3);
        let rows = read_csv(&report(&flat));
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows[1][0], "sub, \"quoted\"/deep.txt",
            "paths from the folder, the biggest first"
        );
        let all = read_csv(&report(&request(Command::Flat, &dir, Format::Csv)));
        assert_eq!(all.len() - 1, 8);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Extensions by bytes and by files, with categories and shares adding
    /// up to 100%, in every format.
    #[test]
    fn extensions_by_bytes_or_files() {
        let dir = odd_tree("exts");
        for i in 0..20 {
            std::fs::write(dir.join(format!("tiny{i}.log")), b"z").unwrap();
        }
        std::fs::write(dir.join("README"), b"no extension").unwrap();
        let json = |by_files: bool| -> serde_json::Value {
            let mut r = request(Command::Exts, &dir, Format::Json);
            r.by_files = by_files;
            // File lengths: on disk each tiny file takes a whole block.
            r.apparent = true;
            serde_json::from_str(&report(&r)).unwrap()
        };
        let (bytes, files) = (json(false), json(true));
        let first = |v: &serde_json::Value| {
            v["extensions"][0]["extension"]
                .as_str()
                .unwrap()
                .to_string()
        };
        assert_eq!(first(&bytes), "txt");
        assert_eq!(first(&files), "log");
        for v in [&bytes, &files] {
            let sum: f64 = v["extensions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["share"].as_f64().unwrap())
                .sum();
            assert!((sum - 100.0).abs() < 0.1, "{sum}");
            let n: u64 = v["extensions"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| e["files"].as_u64().unwrap())
                .sum();
            assert_eq!(n, 29);
        }
        let csv = read_csv(&report(&request(Command::Exts, &dir, Format::Csv)));
        assert_eq!(csv[0], ["extension", "category", "files", "size", "share"]);
        assert!(
            csv.iter().any(|r| r[0].is_empty()),
            "files without an extension"
        );
        let txt = report(&request(Command::Exts, &dir, Format::Txt));
        assert!(txt.contains("(no extension)") && txt.contains(".txt"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
