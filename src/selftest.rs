//! `--selftest`: the smallest thing that says whether the parts that hold logic
//! still work, without a window, a clipboard or the registry.
//!
//! `cargo test` runs the same table through `mod tests` below, so the two can never
//! drift apart, and an agent that only has the built exe can still get an answer:
//! the exit code is pass (0) or fail (1), and the report goes to the log as well as
//! to the console it was started from.

use std::ffi::OsString;

use crate::{copy_paths, install, log, paths, shell, ui, version};

pub struct Check {
    pub name: &'static str,
    pub detail: String,
    pub ok: bool,
}

fn check(name: &'static str, ok: bool, detail: impl Into<String>) -> Check {
    Check {
        name,
        detail: detail.into(),
        ok,
    }
}

/// Everything here must be free of side effects.
pub fn checks() -> Vec<Check> {
    let mut checks = Vec::new();

    checks.push(check(
        "version: 1.10 is newer than 1.9",
        version::is_newer("1.10", "1.9"),
        "1.10 vs 1.9",
    ));
    checks.push(check(
        "version: the same version is not newer",
        !version::is_newer("1.0.0", "1.0"),
        "1.0.0 vs 1.0",
    ));
    checks.push(check(
        "version: an older version is not newer",
        !version::is_newer("0.9", "1.0.0"),
        "0.9 vs 1.0.0",
    ));
    checks.push(check(
        "version: a leading v is ignored",
        version::is_newer("v1.2", "1.1"),
        "v1.2 vs 1.1",
    ));
    checks.push(check(
        "version: an unreadable tag is not newer",
        !version::is_newer("latest", "1.0.0"),
        "latest vs 1.0.0",
    ));
    checks.push(check(
        "version: this build's own version reads as a version",
        version::is_newer(env!("CARGO_PKG_VERSION"), "0.0.0"),
        crate::version(),
    ));
    checks.push(check(
        "version: the version shown to people is two digits",
        crate::version().matches('.').count() == 1,
        crate::version(),
    ));

    // A drive root is the one path the command line mangles: `"D:\"` arrives as
    // `D:"` because the backslash escapes the closing quote.
    let quoted_root = OsString::from(concat!("D", ":", "\""));
    checks.push(check(
        "paths: a drive root quoted on the command line keeps its backslash",
        paths::normalize(&quoted_root) == OsString::from("D:\\"),
        format!("{:?} -> {:?}", quoted_root, paths::normalize(&quoted_root)),
    ));

    let verbatim = OsString::from(r"\\?\C:\Models\file.gguf");
    checks.push(check(
        "paths: the verbatim prefix is removed",
        paths::normalize(&verbatim) == OsString::from(r"C:\Models\file.gguf"),
        format!("{:?}", paths::normalize(&verbatim)),
    ));

    let verbatim_unc = OsString::from(r"\\?\UNC\server\share\file.txt");
    checks.push(check(
        "paths: a verbatim UNC path becomes an ordinary UNC path",
        paths::normalize(&verbatim_unc) == OsString::from(r"\\server\share\file.txt"),
        format!("{:?}", paths::normalize(&verbatim_unc)),
    ));

    let relative = paths::absolute(OsString::from("notes.txt"));
    checks.push(check(
        "paths: a relative path is made absolute",
        std::path::Path::new(&relative).is_absolute(),
        format!("{:?}", relative),
    ));

    checks.push(check(
        "menu: the handler covers files, shortcuts, folders and drives",
        install::HANDLER_CLASSES.contains(&"*")
            && install::HANDLER_CLASSES.contains(&"lnkfile")
            && install::HANDLER_CLASSES.contains(&"Directory")
            && install::HANDLER_CLASSES.contains(&"Drive"),
        install::HANDLER_CLASSES.join(", "),
    ));
    checks.push(check(
        "menu: the handler class id is registered in one place",
        shell::CLSID_TEXT.starts_with('{') && shell::CLSID_TEXT.ends_with('}'),
        shell::CLSID_TEXT,
    ));
    // A test build of the library has no resources at all, so the check only means
    // something in the shipped exe; `cargo test` must not fail over it.
    let helper = install::helper_size();
    checks.push(check(
        "install: this exe carries the shell helper",
        helper > 0 || cfg!(test),
        format!("{helper} bytes"),
    ));
    checks.push(check(
        "menu: empty space inside a folder is still served by a verb",
        !install::BACKGROUND.0.is_empty() && !install::BACKGROUND.1.is_empty(),
        format!("{} -> {}", install::BACKGROUND.0, install::BACKGROUND.1),
    ));

    let batch = vec![
        OsString::from(r"C:\Models\file.gguf"),
        OsString::from(r"\\server\share\Ωmega ünïcode.txt"),
    ];
    checks.push(check(
        "batch: the spool round-trips a path unchanged",
        copy_paths::decode_lines(&copy_paths::encode_lines(&batch)) == batch,
        "two paths, one of them Unicode",
    ));
    let mut unsorted = vec![
        OsString::from(r"C:\b.txt"),
        OsString::from(r"C:\A.txt"),
        OsString::from(r"C:\A.txt"),
    ];
    copy_paths::sort_paths(&mut unsorted);
    checks.push(check(
        "batch: paths come out in name order, once each",
        unsorted == vec![OsString::from(r"C:\A.txt"), OsString::from(r"C:\b.txt")],
        format!("{:?}", unsorted),
    ));

    checks.push(check(
        "log: the log file has a destination",
        log::path().is_some(),
        log::path()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "no %LOCALAPPDATA%".into()),
    ));

    checks
}

/// Print the table and return whether everything passed.
pub fn run() -> bool {
    ui::console(&format!(
        "CopyFilePath {} - selftest",
        crate::version()
    ));

    let mut all_ok = true;
    for check in checks() {
        ui::console(&format!(
            "{}  {}  ({})",
            if check.ok { "PASS" } else { "FAIL" },
            check.name,
            check.detail
        ));
        log::write(&format!(
            "selftest {}: {} ({})",
            if check.ok { "pass" } else { "FAIL" },
            check.name,
            check.detail
        ));
        all_ok &= check.ok;
    }

    let summary = if all_ok {
        "all checks passed"
    } else {
        "FAILURES - see the lines above"
    };
    ui::console(summary);
    log::write(&format!("selftest: {summary}"));
    all_ok
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_self_check_passes() {
        let failed: Vec<String> = super::checks()
            .into_iter()
            .filter(|check| !check.ok)
            .map(|check| format!("{} ({})", check.name, check.detail))
            .collect();
        assert!(failed.is_empty(), "failed checks:\n{}", failed.join("\n"));
    }
}
