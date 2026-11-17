//! Native error dialogs, for failures the user has to see even when
//! Porthole was not started from a terminal.

use std::path::{Path, PathBuf};

use rfd::{MessageButtons, MessageDialog, MessageDialogResult, MessageLevel};

use crate::logs;

/// Set this environment variable to skip dialogs (CI, headless machines).
/// The error still goes to stderr and the log.
pub const NO_DIALOGS_ENV: &str = "PORTHOLE_NO_DIALOGS";

/// Longest log line shown in a dialog; longer ones are cut.
const MAX_LINE_CHARS: usize = 200;

/// A fatal error to show the user.
#[derive(Debug, Clone)]
pub struct ErrorReport {
    pub title: String,
    pub message: String,
    /// Last lines of `sidecar.log`.
    pub log_tail: Vec<String>,
    /// Offered as "Open the logs folder?".
    pub logs_dir: Option<PathBuf>,
}

/// The dialog's body text.
pub fn body(report: &ErrorReport) -> String {
    let mut text = capitalize(&report.message);
    let lines: Vec<&String> = report
        .log_tail
        .iter()
        .filter(|l| !l.starts_with("--- "))
        .collect();
    if !lines.is_empty() {
        text.push_str("\n\nLast lines of sidecar.log:\n");
        for line in lines {
            if line.chars().count() > MAX_LINE_CHARS {
                text.extend(line.chars().take(MAX_LINE_CHARS));
                text.push('…');
            } else {
                text.push_str(line);
            }
            text.push('\n');
        }
    }
    if report.logs_dir.is_some() {
        text.push_str("\nOpen the logs folder?");
    }
    text
}

/// Error messages are lower-case by Rust convention; a dialog reads
/// better as a sentence.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// Show `report` and wait for the user to dismiss it. Must run on the main
/// thread.
pub fn show_error(report: &ErrorReport) {
    logs::write("ERROR", &report.message);
    // From a terminal, show the backend's last words there too (with
    // `--console` they were already streamed).
    if !logs::console() && !report.log_tail.is_empty() {
        eprintln!("porthole: last lines of sidecar.log:");
        for line in &report.log_tail {
            eprintln!("  {line}");
        }
    }
    if std::env::var_os(NO_DIALOGS_ENV).is_some() {
        return;
    }
    let buttons = if report.logs_dir.is_some() {
        MessageButtons::YesNo
    } else {
        MessageButtons::Ok
    };
    let result = MessageDialog::new()
        .set_level(MessageLevel::Error)
        .set_title(&report.title)
        .set_description(body(report))
        .set_buttons(buttons)
        .show();
    if result == MessageDialogResult::Yes
        && let Some(dir) = &report.logs_dir
    {
        open_folder(dir);
    }
}

fn open_folder(dir: &Path) {
    if let Err(err) = open::that_detached(dir) {
        logs::warning!("could not open {}: {err}", dir.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_includes_tail_and_prompt() {
        let report = ErrorReport {
            title: "MyApp".into(),
            message: "the backend exited (exit code 1).".into(),
            log_tail: vec![
                "--- porthole: starting x ---".into(),
                "Traceback".into(),
                "x".repeat(300),
            ],
            logs_dir: Some(PathBuf::from("/logs")),
        };
        let text = body(&report);
        assert!(text.starts_with(
            "The backend exited (exit code 1).\n\nLast lines of sidecar.log:\nTraceback\n"
        ));
        assert!(!text.contains("--- porthole"));
        assert!(text.contains(&format!("{}…", "x".repeat(200))));
        assert!(text.ends_with("Open the logs folder?"));
    }

    #[test]
    fn body_without_tail() {
        let report = ErrorReport {
            title: "t".into(),
            message: "m".into(),
            log_tail: vec![],
            logs_dir: None,
        };
        assert_eq!(body(&report), "M");
    }
}
