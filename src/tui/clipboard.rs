use std::io::Write;

/// Copy `text` to the clipboard and return a user-facing status message.
/// `lines` is the number of lines copied, used for the success wording.
pub fn yank_status(text: &str, lines: usize) -> String {
    if copy_to_clipboard(text) {
        match lines {
            1 => "1 line yanked".to_string(),
            n => format!("{n} lines yanked"),
        }
    } else {
        "clipboard unavailable".to_string()
    }
}

/// Copy text to the system clipboard using platform commands.
/// Uses pbcopy on macOS, xclip on Linux.
pub fn copy_to_clipboard(text: &str) -> bool {
    let (cmd, args): (&str, &[&str]) = if cfg!(target_os = "macos") {
        ("pbcopy", &[])
    } else {
        ("xclip", &["-selection", "clipboard"])
    };

    let Ok(mut child) = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    else {
        return false;
    };

    // Report success only if the bytes were written and the helper exited cleanly,
    // so callers don't show "copied" when the clipboard write actually failed.
    let Some(mut stdin) = child.stdin.take() else {
        return false;
    };
    if stdin.write_all(text.as_bytes()).is_err() {
        return false;
    }
    drop(stdin); // close the pipe so the helper can finish
    matches!(child.wait(), Ok(status) if status.success())
}
