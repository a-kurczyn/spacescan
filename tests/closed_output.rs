//! The command line with its standard output closed: an error, never a
//! report lost in silence.

use std::os::unix::process::CommandExt;
use std::process::{Command, Output, Stdio};

/// Runs the program with `args`, standard output closed when `closed`
/// (else discarded), in a fake home and with no display.
fn run(args: &[&str], closed: bool) -> Output {
    let home = std::env::temp_dir().join(format!("spacescan-closed-home-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_spacescan"));
    cmd.args(args)
        .env("HOME", &home)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    if closed {
        // SAFETY: close is async-signal-safe, as pre_exec requires.
        unsafe {
            cmd.pre_exec(|| {
                libc::close(libc::STDOUT_FILENO);
                Ok(())
            });
        }
    }
    let out = cmd.output().unwrap();
    assert_eq!(
        std::fs::read_dir(&home).unwrap().count(),
        0,
        "home stays empty"
    );
    out
}

#[test]
fn closed_output_is_an_error() {
    let dir = std::env::temp_dir().join(format!("spacescan-closed-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("sub/a.txt"), b"hello").unwrap();
    let path = dir.to_str().unwrap();
    for args in [
        vec!["list", path],
        vec!["flat", path],
        vec!["exts", path, "--format", "json"],
        vec!["--version"],
        vec!["--help"],
    ] {
        let closed = run(&args, true);
        assert_eq!(closed.status.code(), Some(1), "{args:?}");
        assert_eq!(
            String::from_utf8_lossy(&closed.stderr),
            "spacescan: standard output is closed\n",
            "{args:?}"
        );
        // Sent to /dev/null on purpose: a success.
        let discarded = run(&args, false);
        assert_eq!(discarded.status.code(), Some(0), "{args:?}");
        assert!(discarded.stderr.is_empty(), "{args:?}");
    }
    // A mistake on the command line still says so first.
    assert_eq!(run(&["list"], true).status.code(), Some(2));
    std::fs::remove_dir_all(&dir).unwrap();
}
