/// Show a file in Explorer, or open a folder or web link.
pub fn open(target: &str) -> Result<(), String> {
    let arg = if std::path::Path::new(target).is_file() { format!("/select,{target}") } else { target.to_owned() };
    std::process::Command::new("explorer").arg(arg).spawn().map(|_| ()).map_err(|e| e.to_string())
}
