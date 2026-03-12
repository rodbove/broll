use base64::{Engine, engine::general_purpose::STANDARD};
use std::io::Write;

/// Copy text to the system clipboard.
/// Tries OSC 52 escape sequence first, falls back to pbcopy (macOS) / xclip (Linux).
pub fn copy_to_clipboard(text: &str) -> bool {
    // Try OSC 52 (works in iTerm2, Kitty, Alacritty, WezTerm, ghostty)
    let encoded = STANDARD.encode(text);
    let osc = format!("\x1b]52;c;{}\x07", encoded);
    if std::io::stderr().write_all(osc.as_bytes()).is_ok() {
        let _ = std::io::stderr().flush();
        return true;
    }

    // Fallback to platform clipboard command
    let cmd = if cfg!(target_os = "macos") {
        "pbcopy"
    } else {
        "xclip"
    };

    let args: &[&str] = if cmd == "xclip" {
        &["-selection", "clipboard"]
    } else {
        &[]
    };

    if let Ok(mut child) = std::process::Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        let _ = child.wait();
        return true;
    }

    false
}
