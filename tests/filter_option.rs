//! The name filter on the command line, and app options that can't be used
//! (refused before any window opens).

use std::process::{Command, Output};

/// Runs the program with `args` in a fake home and with no display. The
/// home is left exactly as it was: empty.
fn run(args: &[&str], tag: &str) -> Output {
    let home = std::env::temp_dir().join(format!(
        "spacescan-filter-home-{tag}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_spacescan"))
        .args(args)
        .env("HOME", &home)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "home stays empty"
    );
    std::fs::remove_dir(&home).unwrap();
    out
}

/// The CSV rows (header left out) of a report, each split at its commas.
fn rows(out: &Output) -> Vec<Vec<String>> {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| l.split(',').map(str::to_string).collect())
        .collect()
}

#[test]
fn reports_count_only_files_that_match() {
    let dir = std::env::temp_dir().join(format!("spacescan-filter-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("sub/deep")).unwrap();
    std::fs::create_dir_all(dir.join("none")).unwrap();
    for (name, len) in [
        ("a.log", 4000),
        ("b.txt", 9000),
        ("sub/c.LOG", 5000),
        ("sub/d.tmp", 100),
        ("sub/deep/e.txt", 7000),
        ("none/f.mkv", 3000),
    ] {
        std::fs::write(dir.join(name), vec![b'x'; len]).unwrap();
    }
    let d = dir.to_str().unwrap();
    let csv = ["--format", "csv", "--apparent-size"];

    // Every file under the folder: only those that match, any case.
    // (path, size, files, …), paths relative to the folder.
    let flat = rows(&run(
        &[&["flat", d, "--filter", "*.log,*.tmp"][..], &csv].concat(),
        "flat",
    ));
    let mut paths: Vec<&str> = flat.iter().map(|r| r[0].as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["a.log", "sub/c.LOG", "sub/d.tmp"]);

    // Folders count only their matching files; one without any is left out.
    // (type, name, size, files, …)
    let list = rows(&run(
        &[&["list", d, "--filter=*.log"][..], &csv].concat(),
        "list",
    ));
    let mut sizes: Vec<(&str, &str, &str)> = list
        .iter()
        .map(|r| (r[1].as_str(), r[2].as_str(), r[3].as_str()))
        .collect();
    sizes.sort();
    assert_eq!(sizes, [("a.log", "4000", "1"), ("sub", "5000", "1")]);

    // Extensions of the matching files only.
    // (extension, category, files, size, share)
    let exts = rows(&run(
        &[&["exts", d, "--filter", "*.txt"][..], &csv].concat(),
        "exts",
    ));
    assert_eq!(exts.len(), 1, "{exts:?}");
    assert_eq!(
        (&exts[0][0][..], &exts[0][2][..], &exts[0][3][..]),
        ("txt", "2", "16000")
    );

    // Nothing matches: an empty report, not an error.
    let empty = rows(&run(
        &[&["flat", d, "--filter", "*.nothing"][..], &csv].concat(),
        "empty",
    ));
    assert!(empty.is_empty(), "{empty:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn unknown_categories_are_refused_before_the_app_starts() {
    let out = run(&["--category", "Video,NoSuchThing", "/"], "cat");
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown category NoSuchThing"), "{err}");
    assert!(err.contains("Video") && err.contains("(other)"), "{err}");
    let out = run(&["--category", "", "/"], "empty");
    assert_eq!(out.status.code(), Some(2));
}
