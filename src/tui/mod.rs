mod clipboard;
pub mod highlight;
pub mod search;
pub mod view;

pub fn display_path(path: &str) -> String {
    match dirs::home_dir() {
        Some(home) => {
            let home = home.to_string_lossy();
            match path.strip_prefix(home.as_ref()) {
                Some("") => "~".to_string(),
                Some(rest) if rest.starts_with('/') => format!("~{rest}"),
                _ => path.to_string(),
            }
        }
        None => path.to_string(),
    }
}
