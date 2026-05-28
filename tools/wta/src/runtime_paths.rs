use std::path::PathBuf;

pub fn intelligent_terminal_root() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA")
        .or_else(|| std::env::var_os("APPDATA"))
        .map(PathBuf::from)
        .map(|path| path.join("IntelligentTerminal"))
}

pub fn runtime_prompt_root() -> Option<PathBuf> {
    intelligent_terminal_root().map(|root| root.join("prompts"))
}

pub fn runtime_log_path(file_name: &str) -> PathBuf {
    if let Some(root) = intelligent_terminal_root() {
        let log_dir = root.join("logs");
        let _ = std::fs::create_dir_all(&log_dir);
        return log_dir.join(file_name);
    }

    PathBuf::from(file_name)
}

pub fn master_pipe_file_path() -> Option<PathBuf> {
    intelligent_terminal_root().map(|root| root.join("master-pipe.txt"))
}
