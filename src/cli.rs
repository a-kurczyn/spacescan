//! Command-line use: `spacescan --help` lists the commands. A command scans
//! a folder and prints what the app's tables show, as text, CSV or JSON.

use super::*;
use std::ffi::OsString;
use std::io::Write;

const HELP: &str = "\
spacescan — see what takes the space on your disks

Usage:
  spacescan                 start the app
  spacescan PATH            start the app and scan PATH
  spacescan COMMAND PATH [OPTIONS]

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

/// What a command line asks for, once read.
#[derive(Debug, PartialEq)]
enum Parsed {
    /// Start the app, scanning this folder first if given.
    App(Option<PathBuf>),
    Help,
    Version,
    Report(Request),
}

/// Reads the command line (`args` without the program's name), or says
/// what's wrong with it.
fn parse(args: &[OsString]) -> Result<Parsed, String> {
    let Some(first) = args.first() else {
        return Ok(Parsed::App(None));
    };
    let command = match first.to_str() {
        Some("-h" | "--help") => return Ok(Parsed::Help),
        Some("-V" | "--version") => return Ok(Parsed::Version),
        Some("list") => Command::List,
        Some("flat") => Command::Flat,
        Some("exts") => Command::Exts,
        Some(s) if s.starts_with('-') => return Err(format!("unknown option {s}")),
        // Anything else is a folder to open in the app.
        _ if args.len() == 1 => return Ok(Parsed::App(Some(PathBuf::from(first)))),
        _ => return Err(format!("unknown command {}", first.to_string_lossy())),
    };
    let name = command_name(command);
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
    let mut only_paths = false;
    let mut rest = args[1..].iter();
    while let Some(arg) = rest.next() {
        let text = arg.to_str().unwrap_or("");
        if only_paths || !text.starts_with('-') || text == "-" {
            if path.replace(PathBuf::from(arg)).is_some() {
                return Err("only one folder can be given".to_string());
            }
            continue;
        }
        // "--opt value" or "--opt=value".
        let (option, inline) = match text.split_once('=') {
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
                    .ok_or_else(|| format!("{option} needs {what}")),
            }
        };
        // Options for some commands only.
        let refuse = |allowed: &[Command]| {
            (!allowed.contains(&command)).then(|| format!("{name} takes no {option}"))
        };
        match option {
            "--" => only_paths = true,
            "-h" | "--help" => return Ok(Parsed::Help),
            "-V" | "--version" => return Ok(Parsed::Version),
            "--reverse" | "--apparent-size" if inline.is_some() => {
                return Err(format!("{option} takes no value"));
            }
            "--reverse" => req.reverse = true,
            "--apparent-size" => req.apparent = true,
            "--format" => {
                req.format = match value("txt, csv or json")?.as_str() {
                    "txt" => Format::Txt,
                    "csv" => Format::Csv,
                    "json" => Format::Json,
                    other => return Err(format!("unknown format {other}")),
                }
            }
            "--sort" => {
                if let Some(e) = refuse(&[Command::List, Command::Flat]) {
                    return Err(e + " (exts is ordered with --by)");
                }
                req.sort = match value("a column")?.as_str() {
                    "size" => SortColumn::Size,
                    "files" if command == Command::List => SortColumn::Files,
                    "name" => SortColumn::Name,
                    "modified" => SortColumn::Modified,
                    "changed" => SortColumn::Changed,
                    other => return Err(format!("can't sort {name} by {other}")),
                }
            }
            "--by" => {
                if let Some(e) = refuse(&[Command::Exts]) {
                    return Err(e);
                }
                req.by_files = match value("bytes or files")?.as_str() {
                    "bytes" => false,
                    "files" => true,
                    other => return Err(format!("--by takes bytes or files, not {other}")),
                }
            }
            "--limit" => {
                if let Some(e) = refuse(&[Command::Flat]) {
                    return Err(e);
                }
                req.limit = Some(
                    value("a number")?
                        .parse()
                        .map_err(|_| format!("{option} needs a whole number"))?,
                )
            }
            _ => return Err(format!("unknown option {option}")),
        }
    }
    req.path = path.ok_or_else(|| format!("{name} needs a folder"))?;
    Ok(Parsed::Report(req))
}

fn command_name(c: Command) -> &'static str {
    match c {
        Command::List => "list",
        Command::Flat => "flat",
        Command::Exts => "exts",
    }
}

/// Writes `text` to the standard output. A reader that stops early
/// (`| head`) isn't an error.
fn print_out(text: &str) -> Run {
    let mut out = std::io::stdout().lock();
    match out.write_all(text.as_bytes()).and_then(|()| out.flush()) {
        Ok(()) => Run::Exit(0),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Run::Exit(0),
        Err(e) => {
            eprintln!("spacescan: {e}");
            Run::Exit(1)
        }
    }
}

/// Whether standard output was closed when the program started. Rust's
/// runtime points a closed one at /dev/null before `main`, after which a
/// report would vanish without an error, so this is checked earlier.
static STDOUT_CLOSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Runs at load time, before Rust's runtime starts.
// SAFETY: the loader calls it once, before `main`, with C arguments it
// ignores; it only calls fcntl and stores an atomic, nothing that needs
// Rust's runtime.
#[used]
#[unsafe(link_section = ".init_array")]
static CHECK_STDOUT: extern "C" fn() = {
    extern "C" fn check() {
        // SAFETY: F_GETFD only reads a descriptor's flags; it fails only
        // when the descriptor isn't open.
        let closed = unsafe { libc::fcntl(libc::STDOUT_FILENO, libc::F_GETFD) } == -1;
        STDOUT_CLOSED.store(closed, std::sync::atomic::Ordering::Relaxed);
    }
    check
};

/// Runs what the command line asks for, or says to start the app.
pub(crate) fn run(args: &[OsString]) -> Run {
    let parsed = parse(args);
    // Anything printing to a closed output fails, as in other Unix tools.
    if matches!(
        parsed,
        Ok(Parsed::Help | Parsed::Version | Parsed::Report(_))
    ) && STDOUT_CLOSED.load(std::sync::atomic::Ordering::Relaxed)
    {
        eprintln!("spacescan: standard output is closed");
        return Run::Exit(1);
    }
    let req = match parsed {
        Ok(Parsed::App(path)) => return Run::App(path),
        Ok(Parsed::Help) => return print_out(HELP),
        Ok(Parsed::Version) => {
            return print_out(&format!("spacescan {}\n", env!("CARGO_PKG_VERSION")));
        }
        Ok(Parsed::Report(req)) => req,
        Err(why) => {
            eprintln!("spacescan: {why}\nTry 'spacescan --help'.");
            return Run::Exit(2);
        }
    };
    // Output in English, whatever the app's language.
    set_language("en");
    let path = true_case(&std::fs::canonicalize(&req.path).unwrap_or(req.path.clone()));
    match std::fs::metadata(&path) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => {
            eprintln!("spacescan: {} is not a folder", show_path(&path));
            return Run::Exit(1);
        }
        Err(e) => {
            eprintln!("spacescan: {}", friendly_io_error(&path, &e));
            return Run::Exit(1);
        }
    }
    let (tree, problems, unreadable) = scan_for_cli(&path, req.apparent);
    for p in &problems {
        eprintln!("spacescan: {p}");
    }
    // The folder itself couldn't be read: nothing to report.
    if unreadable.contains(&path) {
        return Run::Exit(1);
    }
    // Categories are only needed for extensions, and are read without ever
    // writing the user's files.
    let cats = if req.command == Command::Exts {
        let (cats, problem) = CategoryModel::load_read_only();
        if let Some(p) = problem {
            eprintln!("spacescan: {p}");
        }
        cats
    } else {
        CategoryModel::defaults()
    };
    let out = std::io::stdout().lock();
    let mut out = std::io::BufWriter::new(out);
    let written = write_report(&mut out, &req, &tree, &cats).and_then(|()| out.flush());
    match written {
        Ok(()) => Run::Exit(0),
        // The reader stopped early (`| head`): not an error.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Run::Exit(0),
        Err(e) => {
            eprintln!("spacescan: {e}");
            Run::Exit(1)
        }
    }
}

/// Scans `path` as the app does, without the live chart; also returns the
/// problems met and the folders that couldn't be read.
fn scan_for_cli(path: &Path, apparent: bool) -> (Node, Vec<String>, Vec<PathBuf>) {
    let (tx, rx) = channel();
    let collect = std::thread::spawn(move || {
        let (mut problems, mut unreadable) = (Vec::new(), Vec::new());
        for m in rx {
            match m {
                ScanMsg::LogError(e) => problems.push(e),
                ScanMsg::Unreadable(p) => unreadable.push(p),
                _ => {}
            }
        }
        (problems, unreadable)
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
    let (problems, unreadable) = collect.join().unwrap_or_default();
    (tree, problems, unreadable)
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
            let rows = rows_in_order(tree, false, req.sort, req.reverse);
            write_entries(out, req.format, tree, &rows, false)
        }
        Command::Flat => {
            let mut files = rows_in_order(tree, true, req.sort, req.reverse);
            files.truncate(req.limit.unwrap_or(usize::MAX));
            write_entries(out, req.format, tree, &files, true)
        }
        Command::Exts => write_extensions(out, req, tree, cats),
    }
}

/// The rows of `list` (the folder's own entries) or `flat` (every file
/// under it) in `sort` order, `reverse`d if asked. Equal ones come as in
/// the app's table; flat by name goes by the path below the folder.
pub(crate) fn rows_in_order(
    tree: &Node,
    flat: bool,
    sort: SortColumn,
    reverse: bool,
) -> Vec<&Node> {
    if !flat {
        let mut rows: Vec<&Node> = tree.children.iter().collect();
        sort_rows(&mut rows, sort, reverse);
        return rows;
    }
    let mut files = files_in_app_order(tree);
    if sort == SortColumn::Name {
        // By the path below the folder: it and its sort key are built once
        // per file, not at every comparison. (Paths differ, so no two are
        // equal.)
        let top = tree.path();
        let mut keyed: Vec<(Vec<u8>, String, &Node)> = files
            .par_iter()
            .map(|n| {
                let p = n.path();
                let shown = show_path(p.strip_prefix(&top).unwrap_or(&p));
                (natural_key(&shown), shown, *n)
            })
            .collect();
        keyed.par_sort_unstable_by(|a, b| {
            let by = a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1));
            if reverse { by.reverse() } else { by }
        });
        files = keyed.into_iter().map(|(_, _, n)| n).collect();
    } else {
        sort_rows(&mut files, sort, reverse);
    }
    files
}

/// `n` and the word for it, singular for one.
fn counted(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", format_count(n), if n == 1 { one } else { many })
}

/// Every file under `top`, in the order the app's flat list takes equal
/// ones: folder by folder, a folder coming when its first file is met, and
/// each folder's files in the folder's order.
fn files_in_app_order(top: &Node) -> Vec<&Node> {
    fn walk<'a>(dir: &'a Node, folders: &mut Vec<Vec<&'a Node>>) {
        let mut mine = None;
        for c in dir.children.iter() {
            if c.is_dir {
                deep(|| walk(c, folders));
            } else {
                let at = *mine.get_or_insert_with(|| {
                    folders.push(Vec::new());
                    folders.len() - 1
                });
                folders[at].push(c);
            }
        }
    }
    let mut folders = Vec::new();
    walk(top, &mut folders);
    folders.into_iter().flatten().collect()
}

/// Sorts by `column` as the app's table does: sizes, counts and dates
/// largest/newest first, names A–Z, the other way if `reverse`; equal ones
/// keep the order they come in.
fn sort_rows(rows: &mut [&Node], column: SortColumn, reverse: bool) {
    let by: fn(&Node, &Node) -> std::cmp::Ordering = match column {
        SortColumn::Size => |a, b| b.size.cmp(&a.size),
        SortColumn::Files => |a, b| b.file_count.cmp(&a.file_count),
        SortColumn::Modified => |a, b| b.mtime.cmp(&a.mtime),
        SortColumn::Changed => |a, b| b.ctime.cmp(&a.ctime),
        _ => {
            // By name: each name's sort key is built once, not at every
            // comparison. (Names in one folder differ, so none are equal.)
            rows.par_sort_by_cached_key(|n| (natural_key(&n.name), n.name.clone()));
            if reverse {
                rows.reverse();
            }
            return;
        }
    };
    rows.par_sort_by(|a, b| {
        let order = by(a, b);
        if reverse { order.reverse() } else { order }
    });
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
    // A folder where another filesystem is mounted (not scanned) is shown
    // with a label; CSV and JSON give its own name and say so.
    let mounted = |n: &Node| n.is_dir && *n.name != *show_os(n.disk_name());
    let own_name = |n: &Node| {
        if flat {
            name(n)
        } else {
            show_os(n.disk_name())
        }
    };
    match format {
        Format::Txt => {
            let folders = rows.iter().filter(|n| n.is_dir).count() as u64;
            writeln!(
                out,
                "{}: {} in {}",
                show_path(&top),
                human_size(tree.size),
                counted(tree.file_count, "file", "files")
            )?;
            if flat {
                writeln!(out, "{} shown", counted(rows.len() as u64, "file", "files"))?;
            } else {
                let files = rows.len() as u64 - folders;
                writeln!(
                    out,
                    "{}, {} directly in it",
                    counted(folders, "folder", "folders"),
                    counted(files, "file", "files")
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
            let (head, tail) = if flat {
                ("path", "")
            } else {
                ("type,name", ",other_filesystem")
            };
            writeln!(out, "{head},size,files,modified,changed,permissions{tail}")?;
            for n in rows {
                let first = if flat {
                    csv_field(&name(n))
                } else {
                    format!("{},{}", kind(n), csv_field(&own_name(n)))
                };
                let last = if flat {
                    String::new()
                } else {
                    format!(",{}", mounted(n))
                };
                writeln!(
                    out,
                    "{first},{},{},{},{},{}{last}",
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
                entry.insert(
                    (if flat { "path" } else { "name" }).into(),
                    own_name(n).into(),
                );
                if !flat {
                    entry.insert("other_filesystem".into(), mounted(n).into());
                }
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
    // Sorted again below, by what --by asks.
    let mut exts: Vec<(String, String, u64, u64)> = category_breakdown(tree, cats, Measure::Bytes)
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
                "{}: {} in {}, by {}",
                show_path(&tree.path()),
                human_size(tree.size),
                counted(tree.file_count, "file", "files"),
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
        let dir = std::env::temp_dir().join(format!("spacescan-cli-{tag}-{}", std::process::id()));
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
        let (tree, ..) = scan_for_cli(&req.path, req.apparent);
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

    /// Command lines read as meant, and wrong ones refused: options before
    /// or after the folder, "--opt=value", "--" before a folder named like
    /// an option, help and version anywhere; values on on/off options,
    /// options of another command, missing or bad values, two folders.
    #[test]
    fn command_lines_are_read_or_refused() {
        assert_eq!(parse(&args(&[])), Ok(Parsed::App(None)));
        assert_eq!(
            parse(&args(&["/tmp"])),
            Ok(Parsed::App(Some("/tmp".into())))
        );
        assert_eq!(
            parse(&args(&["list"])),
            Err("list needs a folder".to_string())
        );
        for help in [&["--help"][..], &["-h"], &["list", "/x", "-h"]] {
            assert_eq!(parse(&args(help)), Ok(Parsed::Help));
        }
        for version in [&["--version"][..], &["-V"], &["exts", ".", "-V"]] {
            assert_eq!(parse(&args(version)), Ok(Parsed::Version));
        }
        let ok = |a: &[&str]| match parse(&args(a)) {
            Ok(Parsed::Report(r)) => r,
            other => panic!("{a:?}: {other:?}"),
        };
        let r = ok(&[
            "flat",
            "--format=csv",
            "/x",
            "--limit",
            "5",
            "--sort",
            "name",
            "--reverse",
        ]);
        assert_eq!(
            (r.format, r.limit, r.sort, r.reverse, r.path),
            (Format::Csv, Some(5), SortColumn::Name, true, "/x".into())
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
            (r.by_files, r.format, r.apparent),
            (true, Format::Json, true)
        );
        assert_eq!(ok(&["list", "--", "-dash"]).path, PathBuf::from("-dash"));
        assert_eq!(
            ok(&["list", "--", "--reverse"]).path,
            PathBuf::from("--reverse")
        );
        assert_eq!(ok(&["list", "list"]).path, PathBuf::from("list"));
        for bad in [
            &["--nope"][..],
            &["list", "/a", "/b"],
            &["list", "/a", "--", "/b"],
            &["list", "/a", "--limit", "3"],
            &["list", "/a", "--by", "bytes"],
            &["flat", "/a", "--by", "files"],
            &["flat", "/a", "--sort", "files"],
            &["exts", "/a", "--sort", "name"],
            &["list", "/a", "--format", "xml"],
            &["list", "/a", "--limit"],
            &["flat", "/a", "--limit", "-1"],
            &["flat", "/a", "--limit", "2.5"],
            &["list", "/a", "--apparent-size=false"],
            &["list", "/a", "--reverse=yes"],
            &["list", "/a", "-x"],
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
        let (tree, ..) = scan_for_cli(&dir, false);
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
                "permissions",
                "other_filesystem"
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
        // Smallest first; equal sizes keep their order, as in the app.
        let mut smallest_first = fwd[1..].to_vec();
        smallest_first.sort_by_key(|r| r[2].parse::<u64>().unwrap());
        assert_eq!(back[1..], smallest_first);
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

    /// A folder's entries sorted by name, either way: the natural,
    /// case-insensitive order the app's table uses.
    #[test]
    fn list_by_name_is_the_natural_order() {
        let names = [
            "file10", "File2", "file1", "b", "Ärger", "a007", "a7", ".dot", "Zeta", "alpha",
        ];
        let kids: Vec<Node> = names
            .iter()
            .map(|n| test_node(&format!("/t/{n}"), 1, false, vec![]))
            .collect();
        let tree = test_node("/t", 10, true, kids);
        let mut want: Vec<&str> = names.to_vec();
        want.sort_by(|a, b| natural_cmp(a, b));
        let got = |reverse| -> Vec<String> {
            rows_in_order(&tree, false, SortColumn::Name, reverse)
                .iter()
                .map(|n| n.name.to_string())
                .collect()
        };
        assert_eq!(got(false), want);
        want.reverse();
        assert_eq!(got(true), want);
    }

    /// The flat list sorted by name goes by each file's whole path, so files
    /// of the same name in different folders are told apart and a folder's
    /// files stay together.
    #[test]
    fn flat_by_name_goes_by_path() {
        let dir = std::env::temp_dir().join(format!("spacescan-cli-paths-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for d in ["b", "a/z", "c"] {
            std::fs::create_dir_all(dir.join(d)).unwrap();
            std::fs::write(dir.join(d).join("same.txt"), b"1").unwrap();
        }
        std::fs::write(dir.join("a/aa.txt"), b"1").unwrap();
        std::fs::write(dir.join("top.txt"), b"1").unwrap();
        let mut req = request(Command::Flat, &dir, Format::Csv);
        req.sort = SortColumn::Name;
        let paths: Vec<String> = read_csv(&report(&req))[1..]
            .iter()
            .map(|r| r[0].clone())
            .collect();
        assert_eq!(
            paths,
            [
                "a/aa.txt",
                "a/z/same.txt",
                "b/same.txt",
                "c/same.txt",
                "top.txt"
            ]
        );
        req.reverse = true;
        let back: Vec<String> = read_csv(&report(&req))[1..]
            .iter()
            .map(|r| r[0].clone())
            .collect();
        assert_eq!(back, paths.iter().rev().cloned().collect::<Vec<_>>());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// A folder where another filesystem is mounted: its own name and a
    /// mark in CSV and JSON, the app's label in text; a carriage return in
    /// a name comes escaped, and CSV quotes any field holding one.
    #[test]
    fn mount_points_and_carriage_returns() {
        let dir = std::env::temp_dir().join(format!("spacescan-cli-mount-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("mnt")).unwrap();
        std::fs::write(dir.join("cr\rname"), b"1").unwrap();
        let (tx, rx) = channel();
        std::thread::spawn(move || for _ in rx {});
        let mounts: HashSet<PathBuf> = [dir.join("mnt")].into();
        let ctx = ScanCtx {
            mounts: &mounts,
            progress: &tx,
            cancel: &Default::default(),
            apparent_size: false,
            hard_links: Default::default(),
            saw_hangul: &Default::default(),
            in_file_order: false,
            live: None,
        };
        let tree = scan_dir(&dir, &ctx);
        let write = |format: Format| {
            let mut out = Vec::new();
            write_report(
                &mut out,
                &request(Command::List, &dir, format),
                &tree,
                &CategoryModel::defaults(),
            )
            .unwrap();
            String::from_utf8(out).unwrap()
        };
        let csv = read_csv(&write(Format::Csv));
        assert_eq!(csv[0].last().unwrap(), "other_filesystem");
        let mnt = csv.iter().find(|r| r[1] == "mnt").unwrap();
        assert_eq!(mnt.last().unwrap(), "true");
        // Names come escaped, as the app shows them: no raw carriage return.
        let cr = csv.iter().find(|r| r[1].starts_with("cr")).unwrap();
        assert_eq!(
            (cr[1].as_str(), cr.last().unwrap().as_str()),
            ("cr\\rname", "false")
        );
        // Any text holding one is still quoted.
        assert_eq!(csv_field("a\rb"), "\"a\rb\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        let json: serde_json::Value = serde_json::from_str(&write(Format::Json)).unwrap();
        let entry = json["entries"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "mnt")
            .unwrap();
        assert_eq!(entry["other_filesystem"], true);
        assert!(
            write(Format::Txt).contains(
                &*tree
                    .children
                    .iter()
                    .find(|c| c.disk_name() == "mnt")
                    .unwrap()
                    .name
            )
        );
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
