use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const RUNTIME_CONTEXT_MARKER: &str = "<!-- WTA_RUNTIME_CONTEXT -->";

const USER_PROMPT_FILE_NAME: &str = "terminal-agent.md";
const DEFAULT_PROMPT_FILE_NAME: &str = "terminal-agent.default.md";
const EMBEDDED_DEFAULT_PROMPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/terminal-agent.md"
));

const AUTOFIX_USER_PROMPT_FILE_NAME: &str = "auto-fix.md";
const AUTOFIX_DEFAULT_PROMPT_FILE_NAME: &str = "auto-fix.default.md";
const EMBEDDED_AUTOFIX_PROMPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/prompts/auto-fix.md"
));

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PlannerPromptTemplate {
    pub content: String,
    pub source_label: String,
    pub display_name: String,
}

pub(crate) fn load_autofix_prompt_template() -> PlannerPromptTemplate {
    load_autofix_prompt_template_from_root(
        runtime_prompt_root().as_deref(),
        EMBEDDED_AUTOFIX_PROMPT,
    )
}

pub(crate) fn load_planner_prompt_template() -> PlannerPromptTemplate {
    load_planner_prompt_template_from_root(
        runtime_prompt_root().as_deref(),
        EMBEDDED_DEFAULT_PROMPT,
    )
}

pub(crate) fn merge_runtime_sections(template: &str, runtime_sections: &[String]) -> String {
    let runtime_block = runtime_sections
        .iter()
        .map(|section| section.trim())
        .filter(|section| !section.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    if runtime_block.is_empty() {
        return template.trim_end().to_string();
    }

    if template.contains(RUNTIME_CONTEXT_MARKER) {
        return template.replacen(RUNTIME_CONTEXT_MARKER, &runtime_block, 1);
    }

    format!("{}\n\n{}", template.trim_end(), runtime_block)
}

fn runtime_prompt_root() -> Option<PathBuf> {
    crate::runtime_paths::runtime_prompt_root()
}

fn load_autofix_prompt_template_from_root(
    prompt_root: Option<&Path>,
    embedded_default_prompt: &str,
) -> PlannerPromptTemplate {
    if let Some(prompt_root) = prompt_root {
        let _ = seed_autofix_prompt_files(prompt_root, embedded_default_prompt);

        let user_path = prompt_root.join(AUTOFIX_USER_PROMPT_FILE_NAME);
        if let Ok(content) = fs::read_to_string(&user_path) {
            return PlannerPromptTemplate {
                display_name: "Auto-Fix Agent".to_string(),
                content,
                source_label: format!("user:{}", user_path.display()),
            };
        }

        let default_path = prompt_root.join(AUTOFIX_DEFAULT_PROMPT_FILE_NAME);
        if let Ok(content) = fs::read_to_string(&default_path) {
            return PlannerPromptTemplate {
                display_name: "Auto-Fix Agent".to_string(),
                content,
                source_label: format!("default:{}", default_path.display()),
            };
        }
    }

    PlannerPromptTemplate {
        display_name: "Auto-Fix Agent".to_string(),
        content: embedded_default_prompt.to_string(),
        source_label: "embedded:auto-fix.md".to_string(),
    }
}

fn load_planner_prompt_template_from_root(
    prompt_root: Option<&Path>,
    embedded_default_prompt: &str,
) -> PlannerPromptTemplate {
    if let Some(prompt_root) = prompt_root {
        let _ = seed_prompt_files(prompt_root, embedded_default_prompt);

        let user_path = prompt_root.join(USER_PROMPT_FILE_NAME);
        if let Ok(content) = fs::read_to_string(&user_path) {
            return PlannerPromptTemplate {
                display_name: extract_prompt_display_name(&content),
                content,
                source_label: format!("user:{}", user_path.display()),
            };
        }

        let default_path = prompt_root.join(DEFAULT_PROMPT_FILE_NAME);
        if let Ok(content) = fs::read_to_string(&default_path) {
            return PlannerPromptTemplate {
                display_name: extract_prompt_display_name(&content),
                content,
                source_label: format!("default:{}", default_path.display()),
            };
        }
    }

    PlannerPromptTemplate {
        display_name: extract_prompt_display_name(embedded_default_prompt),
        content: embedded_default_prompt.to_string(),
        source_label: "embedded".to_string(),
    }
}

fn extract_prompt_display_name(content: &str) -> String {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(title) = trimmed.strip_prefix("#") {
            let title = title.trim_start_matches('#').trim();
            if !title.is_empty() {
                return title.to_string();
            }
        }
        break;
    }

    "Prompt".to_string()
}

fn seed_autofix_prompt_files(
    prompt_root: &Path,
    embedded_default_prompt: &str,
) -> std::io::Result<()> {
    fs::create_dir_all(prompt_root)?;

    let default_path = prompt_root.join(AUTOFIX_DEFAULT_PROMPT_FILE_NAME);
    let previous_default = fs::read_to_string(&default_path).ok();
    let user_path = prompt_root.join(AUTOFIX_USER_PROMPT_FILE_NAME);
    let existing_user = fs::read_to_string(&user_path).ok();

    write_if_changed(&default_path, embedded_default_prompt)?;

    if !user_path.exists() {
        fs::write(&user_path, embedded_default_prompt)?;
    } else if previous_default.as_deref() == existing_user.as_deref() {
        fs::write(&user_path, embedded_default_prompt)?;
    }

    Ok(())
}

fn seed_prompt_files(prompt_root: &Path, embedded_default_prompt: &str) -> std::io::Result<()> {
    fs::create_dir_all(prompt_root)?;

    let default_path = prompt_root.join(DEFAULT_PROMPT_FILE_NAME);
    let previous_default = fs::read_to_string(&default_path).ok();
    let user_path = prompt_root.join(USER_PROMPT_FILE_NAME);
    let existing_user = fs::read_to_string(&user_path).ok();

    write_if_changed(&default_path, embedded_default_prompt)?;

    if !user_path.exists() {
        fs::write(&user_path, embedded_default_prompt)?;
    } else if previous_default.as_deref() == existing_user.as_deref() {
        fs::write(&user_path, embedded_default_prompt)?;
    }

    Ok(())
}

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    match fs::read_to_string(path) {
        Ok(existing) if existing == content => Ok(()),
        _ => fs::write(path, content),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        load_planner_prompt_template_from_root, merge_runtime_sections, DEFAULT_PROMPT_FILE_NAME,
        RUNTIME_CONTEXT_MARKER, USER_PROMPT_FILE_NAME,
    };
    use std::fs;
    use std::path::PathBuf;

    fn temp_prompt_root(test_name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "wta-prompt-tests-{}-{}",
            test_name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        root
    }

    #[test]
    fn merge_runtime_sections_replaces_marker() {
        let merged = merge_runtime_sections(
            &format!("before\n{}\nafter", RUNTIME_CONTEXT_MARKER),
            &[String::from("runtime block")],
        );

        assert_eq!(merged, "before\nruntime block\nafter");
    }

    #[test]
    fn merge_runtime_sections_appends_when_marker_missing() {
        let merged =
            merge_runtime_sections("before", &[String::from("first"), String::from("second")]);

        assert_eq!(merged, "before\n\nfirst\n\nsecond");
    }

    #[test]
    fn loader_seeds_prompt_files_and_prefers_user_prompt() {
        let prompt_root = temp_prompt_root("prefers-user");
        let embedded = "embedded prompt";
        fs::create_dir_all(&prompt_root).unwrap();
        fs::write(prompt_root.join(USER_PROMPT_FILE_NAME), "user prompt").unwrap();

        let template = load_planner_prompt_template_from_root(Some(&prompt_root), embedded);

        assert_eq!(template.content, "user prompt");
        assert!(template.source_label.starts_with("user:"));
        assert_eq!(
            fs::read_to_string(prompt_root.join(DEFAULT_PROMPT_FILE_NAME)).unwrap(),
            embedded
        );

        let _ = fs::remove_dir_all(prompt_root);
    }

    #[test]
    fn loader_falls_back_to_embedded_without_prompt_root() {
        let template = load_planner_prompt_template_from_root(None, "embedded prompt");

        assert_eq!(template.content, "embedded prompt");
        assert_eq!(template.source_label, "embedded");
    }

    #[test]
    fn loader_updates_user_prompt_when_it_matches_previous_default() {
        let prompt_root = temp_prompt_root("migrate-unedited-user");
        let previous_default = "old default prompt";
        let embedded = "new default prompt";

        fs::create_dir_all(&prompt_root).unwrap();
        fs::write(prompt_root.join(DEFAULT_PROMPT_FILE_NAME), previous_default).unwrap();
        fs::write(prompt_root.join(USER_PROMPT_FILE_NAME), previous_default).unwrap();

        let template = load_planner_prompt_template_from_root(Some(&prompt_root), embedded);

        assert_eq!(template.content, embedded);
        assert_eq!(
            fs::read_to_string(prompt_root.join(DEFAULT_PROMPT_FILE_NAME)).unwrap(),
            embedded
        );
        assert_eq!(
            fs::read_to_string(prompt_root.join(USER_PROMPT_FILE_NAME)).unwrap(),
            embedded
        );

        let _ = fs::remove_dir_all(prompt_root);
    }

    #[test]
    fn loader_preserves_customized_user_prompt_when_default_changes() {
        let prompt_root = temp_prompt_root("preserve-custom-user");
        let previous_default = "old default prompt";
        let embedded = "new default prompt";

        fs::create_dir_all(&prompt_root).unwrap();
        fs::write(prompt_root.join(DEFAULT_PROMPT_FILE_NAME), previous_default).unwrap();
        fs::write(
            prompt_root.join(USER_PROMPT_FILE_NAME),
            "custom user prompt",
        )
        .unwrap();

        let template = load_planner_prompt_template_from_root(Some(&prompt_root), embedded);

        assert_eq!(template.content, "custom user prompt");
        assert_eq!(
            fs::read_to_string(prompt_root.join(DEFAULT_PROMPT_FILE_NAME)).unwrap(),
            embedded
        );
        assert_eq!(
            fs::read_to_string(prompt_root.join(USER_PROMPT_FILE_NAME)).unwrap(),
            "custom user prompt"
        );

        let _ = fs::remove_dir_all(prompt_root);
    }
}
