use std::fs;
use std::path::{Path, PathBuf};

pub(crate) const RUNTIME_CONTEXT_MARKER: &str = "<!-- WTA_RUNTIME_CONTEXT -->";
pub(crate) const DEFAULT_SPECIALIST_NAME: &str = "terminal-agent";

/// Max recursion depth when scanning an agent-definition directory. Claude's
/// `.claude/agents/` is documented as recursive (subdirs are organizational;
/// identity comes from the file, not the path), so we descend a bounded depth.
const SPECIALIST_SCAN_DEPTH: usize = 8;

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpecialistEntry {
    /// Display name shown in `/as` (for example `CLAUDE` or `devops`).
    pub display_name: String,
    /// Full path to the discovered specialist file.
    pub path: PathBuf,
    /// Where this specialist came from.
    pub source: SpecialistSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SpecialistSource {
    /// From WTA's runtime `prompts/` directory.
    Wta,
    /// From Copilot CLI agent files.
    Copilot,
    /// From Claude Code agent files.
    Claude,
    /// From Gemini CLI agent files.
    Gemini,
    /// From Codex CLI prompt files.
    Codex,
}

impl SpecialistSource {
    fn sort_rank(self) -> u8 {
        match self {
            Self::Wta => 0,
            Self::Copilot => 1,
            Self::Claude => 2,
            Self::Gemini => 3,
            Self::Codex => 4,
        }
    }

    /// Map an agent CLI id (`AgentProfile::id`, e.g. `"copilot"`) to the
    /// specialist source whose `.<cli>/agents` directories belong to it.
    /// Returns `None` for unknown / custom agents that have no `.x/agents`
    /// convention.
    pub(crate) fn from_agent_id(id: &str) -> Option<Self> {
        match id {
            "copilot" => Some(Self::Copilot),
            "claude" => Some(Self::Claude),
            "gemini" => Some(Self::Gemini),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecialistRoot {
    source: SpecialistSource,
    /// Directory scanned (recursively, up to `max_depth`) for `*.md` files.
    path: PathBuf,
    max_depth: usize,
}

impl SpecialistRoot {
    fn agents_dir(source: SpecialistSource, path: PathBuf, max_depth: usize) -> Self {
        Self {
            source,
            path,
            max_depth,
        }
    }
}

pub(crate) fn load_autofix_prompt_template() -> PlannerPromptTemplate {
    load_autofix_prompt_template_from_root(
        runtime_prompt_root().as_deref(),
        EMBEDDED_AUTOFIX_PROMPT,
    )
}

pub(crate) fn load_planner_prompt_template() -> PlannerPromptTemplate {
    load_planner_prompt_template_named(None)
}

pub(crate) fn load_planner_prompt_template_named(name: Option<&str>) -> PlannerPromptTemplate {
    if let Some(path) = specialist_path_from_selection(name) {
        if let Some(template) = load_specialist_by_path(&path) {
            return template;
        }
    }

    load_planner_prompt_template_from_root(
        runtime_prompt_root().as_deref(),
        EMBEDDED_DEFAULT_PROMPT,
        name,
    )
}

pub(crate) fn list_specialists() -> Vec<String> {
    list_specialists_from_root(runtime_prompt_root().as_deref())
}

pub(crate) fn discover_specialists(cwd: &Path) -> Vec<SpecialistEntry> {
    let repo_root = find_git_repo_root(cwd);
    let home_dir = home_dir();
    let roots = specialist_roots(
        cwd,
        repo_root.as_deref(),
        home_dir.as_deref(),
        runtime_prompt_root().as_deref(),
    );
    discover_specialists_from_roots(&roots, runtime_prompt_root().as_deref())
}

/// Keep only WTA specialists plus those belonging to the active agent CLI.
/// The agent pane runs a single CLI, so its `/as` list is scoped to that
/// agent's `.<cli>/agents` files. An unknown / custom agent (`active == None`)
/// has no `.x/agents` convention, so only WTA specialists remain.
///
/// Pure (no env / IO) so it is unit-testable in isolation.
pub(crate) fn scope_specialists_to_agent(
    specialists: Vec<SpecialistEntry>,
    active: Option<SpecialistSource>,
) -> Vec<SpecialistEntry> {
    specialists
        .into_iter()
        .filter(|entry| entry.source == SpecialistSource::Wta || Some(entry.source) == active)
        .collect()
}

pub(crate) fn load_specialist_by_path(path: &Path) -> Option<PlannerPromptTemplate> {
    let content = fs::read_to_string(path).ok()?;
    Some(PlannerPromptTemplate {
        display_name: extract_prompt_display_name(&content),
        content,
        source_label: format!("specialist:{}", path.display()),
    })
}

pub(crate) fn specialist_display_name_for_selection(selection: &str) -> String {
    if let Some(path) = specialist_path_from_selection(Some(selection)) {
        return specialist_display_name_from_path(&path);
    }

    normalize_specialist_name(selection)
        .unwrap_or(selection.trim())
        .to_string()
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
    name: Option<&str>,
) -> PlannerPromptTemplate {
    if let Some(prompt_root) = prompt_root {
        let _ = seed_prompt_files(prompt_root, embedded_default_prompt);

        if let Some(named_user_path) = named_specialist_path(prompt_root, name) {
            if let Ok(content) = fs::read_to_string(&named_user_path) {
                return PlannerPromptTemplate {
                    display_name: extract_prompt_display_name(&content),
                    content,
                    source_label: format!("user:{}", named_user_path.display()),
                };
            }
        }

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

fn list_specialists_from_root(prompt_root: Option<&Path>) -> Vec<String> {
    let roots = specialist_roots(Path::new("."), None, None, prompt_root);
    let mut specialists = discover_specialists_from_roots(&roots, prompt_root)
        .into_iter()
        .filter(|entry| entry.source == SpecialistSource::Wta)
        .map(|entry| entry.display_name)
        .collect::<Vec<_>>();

    specialists.sort_by(|left, right| compare_specialist_names(left, right));
    specialists.dedup();
    specialists
}

/// Single source of truth for specialist discovery locations.
///
/// Every supported agent CLI follows the same convention: agent definition
/// files live in `.<cli>/agents/**/*.md` at the repo level and
/// `~/.<cli>/agents/**/*.md` at the user level. The `**/*.md` glob also
/// matches Copilot's `*.agent.md` files (they still end in `.md`). WTA's own
/// runtime `prompts/` directory is treated the same way.
fn specialist_roots(
    cwd: &Path,
    repo_root: Option<&Path>,
    home_dir: Option<&Path>,
    prompt_root: Option<&Path>,
) -> Vec<SpecialistRoot> {
    /// `(source, dot-directory)` for each CLI's `agents` folder.
    const CLI_AGENT_DIRS: &[(SpecialistSource, &str)] = &[
        (SpecialistSource::Copilot, ".copilot"),
        (SpecialistSource::Claude, ".claude"),
        (SpecialistSource::Gemini, ".gemini"),
        (SpecialistSource::Codex, ".codex"),
    ];

    let mut roots = Vec::new();

    // Repo-level: `<repo>/.<cli>/agents/**/*.md`.
    if let Some(repo_root) = repo_root {
        for (source, dir) in CLI_AGENT_DIRS {
            roots.push(SpecialistRoot::agents_dir(
                *source,
                repo_root.join(dir).join("agents"),
                SPECIALIST_SCAN_DEPTH,
            ));
        }
    }

    // User-level: `~/.<cli>/agents/**/*.md`.
    if let Some(home_dir) = home_dir {
        for (source, dir) in CLI_AGENT_DIRS {
            roots.push(SpecialistRoot::agents_dir(
                *source,
                home_dir.join(dir).join("agents"),
                SPECIALIST_SCAN_DEPTH,
            ));
        }
    }

    // WTA's own runtime prompts directory.
    if let Some(prompt_root) = prompt_root {
        roots.push(SpecialistRoot::agents_dir(
            SpecialistSource::Wta,
            prompt_root.to_path_buf(),
            0,
        ));
    }

    let _ = cwd;
    roots
}

fn discover_specialists_from_roots(
    roots: &[SpecialistRoot],
    prompt_root: Option<&Path>,
) -> Vec<SpecialistEntry> {
    let mut specialists = Vec::new();

    for root in roots {
        scan_specialist_root(&mut specialists, root);
    }

    ensure_default_wta_specialist(&mut specialists, prompt_root);

    specialists.sort_by(|left, right| {
        left.source
            .sort_rank()
            .cmp(&right.source.sort_rank())
            .then_with(|| {
                wta_default_rank(&left.display_name, left.source)
                    .cmp(&wta_default_rank(&right.display_name, right.source))
            })
            .then_with(|| {
                left.display_name
                    .to_ascii_lowercase()
                    .cmp(&right.display_name.to_ascii_lowercase())
            })
            .then_with(|| left.display_name.cmp(&right.display_name))
    });
    specialists
}

fn scan_specialist_root(specialists: &mut Vec<SpecialistEntry>, root: &SpecialistRoot) {
    if root.source == SpecialistSource::Wta {
        let _ = seed_prompt_files(&root.path, EMBEDDED_DEFAULT_PROMPT);
    }

    scan_specialist_dir(specialists, root.source, &root.path, ".md", root.max_depth);
}

fn scan_specialist_dir(
    specialists: &mut Vec<SpecialistEntry>,
    source: SpecialistSource,
    dir: &Path,
    suffix: &str,
    remaining_depth: usize,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if remaining_depth > 0 {
                scan_specialist_dir(specialists, source, &path, suffix, remaining_depth - 1);
            }
            continue;
        }

        push_specialist_file_if_allowed(specialists, source, &path, Some(suffix));
    }
}

fn push_specialist_file_if_allowed(
    specialists: &mut Vec<SpecialistEntry>,
    source: SpecialistSource,
    path: &Path,
    suffix: Option<&str>,
) {
    if path.is_dir() || !path.is_file() {
        return;
    }

    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return;
    };
    if let Some(suffix) = suffix {
        if !file_name.ends_with(suffix) {
            return;
        }
    }
    if should_skip_specialist_file(source, file_name) {
        return;
    }

    let display_name = specialist_display_name_from_path(path);
    if specialists.iter().any(|entry| {
        entry.source == source && entry.display_name.eq_ignore_ascii_case(&display_name)
    }) {
        return;
    }

    specialists.push(SpecialistEntry {
        display_name,
        path: path.to_path_buf(),
        source,
    });
}

fn should_skip_specialist_file(source: SpecialistSource, file_name: &str) -> bool {
    source == SpecialistSource::Wta
        && (!file_name.ends_with(".md")
            || file_name.ends_with(".default.md")
            || file_name.starts_with("auto-fix"))
}

fn ensure_default_wta_specialist(
    specialists: &mut Vec<SpecialistEntry>,
    prompt_root: Option<&Path>,
) {
    if specialists.iter().any(|entry| {
        entry.source == SpecialistSource::Wta
            && entry
                .display_name
                .eq_ignore_ascii_case(DEFAULT_SPECIALIST_NAME)
    }) {
        return;
    }

    let default_path = prompt_root
        .map(|root| root.join(USER_PROMPT_FILE_NAME))
        .unwrap_or_else(|| PathBuf::from(USER_PROMPT_FILE_NAME));
    specialists.push(SpecialistEntry {
        display_name: DEFAULT_SPECIALIST_NAME.to_string(),
        path: default_path,
        source: SpecialistSource::Wta,
    });
}

fn specialist_display_name_from_path(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(strip_specialist_extension)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| path.display().to_string())
}

fn compare_specialist_names(left: &str, right: &str) -> std::cmp::Ordering {
    wta_default_rank(left, SpecialistSource::Wta)
        .cmp(&wta_default_rank(right, SpecialistSource::Wta))
        .then_with(|| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()))
        .then_with(|| left.cmp(right))
}

fn wta_default_rank(name: &str, source: SpecialistSource) -> u8 {
    if source == SpecialistSource::Wta && name.eq_ignore_ascii_case(DEFAULT_SPECIALIST_NAME) {
        0
    } else {
        1
    }
}

fn named_specialist_path(prompt_root: &Path, name: Option<&str>) -> Option<PathBuf> {
    let name = normalize_specialist_name(name?)?;
    if name == DEFAULT_SPECIALIST_NAME {
        return None;
    }
    Some(prompt_root.join(format!("{name}.md")))
}

fn normalize_specialist_name(name: &str) -> Option<&str> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains(['\\', '/']) {
        return None;
    }
    Some(strip_specialist_extension(trimmed))
}

fn specialist_path_from_selection(name: Option<&str>) -> Option<PathBuf> {
    let trimmed = name?.trim();
    if trimmed.is_empty() || !trimmed.contains(['\\', '/']) {
        return None;
    }
    Some(PathBuf::from(trimmed))
}

fn strip_specialist_extension(name: &str) -> &str {
    name.strip_suffix(".agent.md")
        .or_else(|| name.strip_suffix(".toml"))
        .or_else(|| name.strip_suffix(".md"))
        .unwrap_or(name)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            let drive = std::env::var_os("HOMEDRIVE")?;
            let path = std::env::var_os("HOMEPATH")?;
            if drive.is_empty() || path.is_empty() {
                None
            } else {
                let mut home = PathBuf::from(drive);
                home.push(PathBuf::from(path));
                Some(home)
            }
        })
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
}

fn find_git_repo_root(cwd: &Path) -> Option<PathBuf> {
    let mut current = if cwd.is_file() { cwd.parent()? } else { cwd };

    for _ in 0..=10 {
        if current.join(".git").exists() {
            return Some(current.to_path_buf());
        }
        current = current.parent()?;
    }

    None
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

    // (Re)seed the user file only when it is absent or still matches the
    // previous embedded default (i.e. the user hasn't customized it). Use
    // `write_if_changed` so an unchanged file is never rewritten — this avoids
    // needless disk churn on every prompt load and, because the write is
    // atomic, keeps concurrent readers from observing a truncated file.
    if existing_user.is_none() || previous_default.as_deref() == existing_user.as_deref() {
        write_if_changed(&user_path, embedded_default_prompt)?;
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

    // See the note in `seed_autofix_prompt_files`: only (re)seed an absent or
    // still-default user file, and route through `write_if_changed` so an
    // unchanged file is never rewritten and concurrent readers never see a
    // truncated file.
    if existing_user.is_none() || previous_default.as_deref() == existing_user.as_deref() {
        write_if_changed(&user_path, embedded_default_prompt)?;
    }

    Ok(())
}

/// Counter for unique temp-file names in [`write_atomic`]. Process-wide so
/// concurrent writers (e.g. many tests loading prompts at once) never collide
/// on the same staging path.
static NEXT_TMP_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn write_if_changed(path: &Path, content: &str) -> std::io::Result<()> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == content {
            return Ok(());
        }
    }
    write_atomic(path, content)
}

/// Write `content` to `path` atomically: stage into a uniquely-named temp file
/// in the same directory, then `rename` it over the destination. On both
/// Windows and Unix `rename` replaces the destination in a single operation, so
/// a concurrent reader always observes either the old or the new complete file
/// — never a half-truncated one. The shared runtime prompt root is read and
/// seeded from many threads (notably the test suite), where a plain in-place
/// `fs::write` truncates first and races readers down to an empty string.
fn write_atomic(path: &Path, content: &str) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path.file_name().and_then(|n| n.to_str()).unwrap_or("prompt");
    let unique = NEXT_TMP_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = dir.join(format!(".{}.{}.{}.tmp", stem, std::process::id(), unique));
    fs::write(&tmp, content)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        discover_specialists_from_roots, find_git_repo_root, list_specialists_from_root,
        load_planner_prompt_template_from_root, load_planner_prompt_template_named,
        load_specialist_by_path, merge_runtime_sections, scope_specialists_to_agent,
        specialist_display_name_for_selection, specialist_display_name_from_path,
        specialist_path_from_selection, specialist_roots, strip_specialist_extension,
        SpecialistEntry, SpecialistSource, DEFAULT_SPECIALIST_NAME, DEFAULT_PROMPT_FILE_NAME,
        RUNTIME_CONTEXT_MARKER, SPECIALIST_SCAN_DEPTH, USER_PROMPT_FILE_NAME,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    fn entry(source: SpecialistSource, name: &str) -> SpecialistEntry {
        SpecialistEntry {
            display_name: name.to_string(),
            path: PathBuf::from(format!("{name}.md")),
            source,
        }
    }

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

        let template = load_planner_prompt_template_from_root(Some(&prompt_root), embedded, None);

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
        let template = load_planner_prompt_template_from_root(None, "embedded prompt", None);

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

        let template = load_planner_prompt_template_from_root(Some(&prompt_root), embedded, None);

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

        let template = load_planner_prompt_template_from_root(Some(&prompt_root), embedded, None);

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

    #[test]
    fn named_loader_loads_discovered_specialist_by_absolute_path() {
        // A discovered specialist is selected by full path (contains a path
        // separator). It must be read directly via load_specialist_by_path,
        // NOT routed through the WTA-prompt-root name resolver (which rejects
        // slashes) — otherwise it would silently fall back to the default.
        let dir = temp_prompt_root("named-by-path");
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("reviewer.md");
        fs::write(&file, "# Reviewer\nYou are a strict reviewer.").unwrap();

        let selection = file.to_string_lossy().into_owned();
        let template = load_planner_prompt_template_named(Some(&selection));

        assert_eq!(template.content, "# Reviewer\nYou are a strict reviewer.");
        assert_eq!(template.display_name, "Reviewer");
        assert!(
            template.source_label.starts_with("specialist:"),
            "expected a path-loaded specialist, got source_label={}",
            template.source_label
        );

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn named_loader_prefers_matching_specialist_file() {
        let prompt_root = temp_prompt_root("named-specialist");
        fs::create_dir_all(&prompt_root).unwrap();
        fs::write(prompt_root.join("devops.md"), "# DevOps\ncustom").unwrap();

        let template =
            load_planner_prompt_template_from_root(Some(&prompt_root), "embedded prompt", Some("devops"));

        assert_eq!(template.display_name, "DevOps");
        assert_eq!(template.content, "# DevOps\ncustom");
        assert!(template.source_label.ends_with("devops.md"));

        let _ = fs::remove_dir_all(prompt_root);
    }

    #[test]
    fn named_loader_falls_back_to_default_when_specialist_missing() {
        let prompt_root = temp_prompt_root("named-fallback");
        fs::create_dir_all(&prompt_root).unwrap();
        fs::write(prompt_root.join(USER_PROMPT_FILE_NAME), "default user prompt").unwrap();

        let template = load_planner_prompt_template_from_root(
            Some(&prompt_root),
            "embedded prompt",
            Some("missing"),
        );

        assert_eq!(template.content, "default user prompt");
        assert!(template.source_label.starts_with("user:"));

        let _ = fs::remove_dir_all(prompt_root);
    }

    #[test]
    fn list_specialists_excludes_defaults_and_autofix_files() {
        let prompt_root = temp_prompt_root("list-specialists");
        fs::create_dir_all(&prompt_root).unwrap();
        fs::write(prompt_root.join("devops.md"), "devops").unwrap();
        fs::write(prompt_root.join("security.md"), "security").unwrap();
        fs::write(prompt_root.join("security.default.md"), "ignored").unwrap();
        fs::write(prompt_root.join("auto-fix-custom.md"), "ignored").unwrap();

        let specialists = list_specialists_from_root(Some(&prompt_root));

        assert_eq!(
            specialists,
            vec![
                DEFAULT_SPECIALIST_NAME.to_string(),
                "devops".to_string(),
                "security".to_string()
            ]
        );

        let _ = fs::remove_dir_all(prompt_root);
    }

    #[test]
    fn load_specialist_by_path_reads_markdown_and_uses_specialist_source_label() {
        let prompt_root = temp_prompt_root("load-specialist-path");
        fs::create_dir_all(&prompt_root).unwrap();
        let specialist_path = prompt_root.join("CLAUDE.md");
        fs::write(&specialist_path, "# Claude\nUse Claude rules").unwrap();

        let template = load_specialist_by_path(&specialist_path).unwrap();

        assert_eq!(template.display_name, "Claude");
        assert_eq!(template.content, "# Claude\nUse Claude rules");
        assert_eq!(
            template.source_label,
            format!("specialist:{}", specialist_path.display())
        );

        let _ = fs::remove_dir_all(prompt_root);
    }

    #[test]
    fn specialist_roots_centralize_uniform_agents_dirs() {
        let repo_root = temp_prompt_root("roots-repo");
        let home_root = temp_prompt_root("roots-home");
        let prompt_root = repo_root.join("wta-prompts");
        let cwd = repo_root.join("src");

        let roots = specialist_roots(&cwd, Some(&repo_root), Some(&home_root), Some(&prompt_root));
        let summary = roots
            .iter()
            .map(|root| (root.source, root.path.clone(), root.max_depth))
            .collect::<Vec<_>>();

        assert_eq!(
            summary,
            vec![
                // Repo-level: `<repo>/.<cli>/agents` for every CLI.
                (
                    SpecialistSource::Copilot,
                    repo_root.join(".copilot").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (
                    SpecialistSource::Claude,
                    repo_root.join(".claude").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (
                    SpecialistSource::Gemini,
                    repo_root.join(".gemini").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (
                    SpecialistSource::Codex,
                    repo_root.join(".codex").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                // User-level: `~/.<cli>/agents` for every CLI.
                (
                    SpecialistSource::Copilot,
                    home_root.join(".copilot").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (
                    SpecialistSource::Claude,
                    home_root.join(".claude").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (
                    SpecialistSource::Gemini,
                    home_root.join(".gemini").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (
                    SpecialistSource::Codex,
                    home_root.join(".codex").join("agents"),
                    SPECIALIST_SCAN_DEPTH,
                ),
                (SpecialistSource::Wta, prompt_root, 0),
            ]
        );
    }

    #[test]
    fn discover_specialists_scans_correct_locations_and_preserves_precedence() {
        let repo_root = temp_prompt_root("cwd-discovery");
        let home_root = temp_prompt_root("home-discovery");
        let prompt_root = repo_root.join("wta-prompts");
        let nested_cwd = repo_root.join("src").join("nested");

        fs::create_dir_all(repo_root.join(".git")).unwrap();
        fs::create_dir_all(repo_root.join(".claude").join("agents")).unwrap();
        fs::create_dir_all(repo_root.join(".codex").join("agents")).unwrap();
        fs::create_dir_all(home_root.join(".copilot").join("agents")).unwrap();
        fs::create_dir_all(home_root.join(".claude").join("agents")).unwrap();
        fs::create_dir_all(home_root.join(".gemini").join("agents")).unwrap();
        fs::create_dir_all(home_root.join(".codex").join("agents")).unwrap();
        fs::create_dir_all(&nested_cwd).unwrap();
        fs::create_dir_all(&prompt_root).unwrap();

        fs::write(prompt_root.join("devops.md"), "devops").unwrap();
        // Copilot's `.agent.md` files still end in `.md`, so the uniform
        // `**/*.md` scan picks them up.
        fs::write(
            home_root.join(".copilot").join("agents").join("user.agent.md"),
            "# User\nhome guidance",
        )
        .unwrap();
        fs::write(
            repo_root.join(".claude").join("agents").join("reviewer.md"),
            "# Reviewer\nrepo guidance",
        )
        .unwrap();
        fs::write(
            home_root.join(".claude").join("agents").join("reviewer.md"),
            "# Reviewer User\nhome guidance",
        )
        .unwrap();
        fs::write(
            home_root.join(".claude").join("agents").join("user-claude.md"),
            "# Claude User\nhome guidance",
        )
        .unwrap();
        fs::write(
            home_root.join(".gemini").join("agents").join("gemini-user.md"),
            "# Gemini User\nhome guidance",
        )
        .unwrap();
        fs::write(
            repo_root.join(".codex").join("agents").join("codex-repo.md"),
            "# Codex Repo\nrepo guidance",
        )
        .unwrap();
        fs::write(
            home_root.join(".codex").join("agents").join("codex-user.md"),
            "# Codex User\nhome guidance",
        )
        .unwrap();

        let roots = specialist_roots(&nested_cwd, Some(&repo_root), Some(&home_root), Some(&prompt_root));
        let specialists = discover_specialists_from_roots(&roots, Some(&prompt_root));

        let summary = specialists
            .iter()
            .map(|entry| (entry.source, entry.display_name.clone()))
            .collect::<Vec<_>>();

        assert_eq!(
            summary,
            vec![
                (SpecialistSource::Wta, DEFAULT_SPECIALIST_NAME.to_string()),
                (SpecialistSource::Wta, "devops".to_string()),
                (SpecialistSource::Copilot, "user".to_string()),
                (SpecialistSource::Claude, "reviewer".to_string()),
                (SpecialistSource::Claude, "user-claude".to_string()),
                (SpecialistSource::Gemini, "gemini-user".to_string()),
                (SpecialistSource::Codex, "codex-repo".to_string()),
                (SpecialistSource::Codex, "codex-user".to_string()),
            ]
        );

        let reviewer = specialists
            .iter()
            .find(|entry| entry.source == SpecialistSource::Claude && entry.display_name == "reviewer")
            .expect("expected repo claude specialist");
        assert_eq!(
            reviewer.path,
            repo_root.join(".claude").join("agents").join("reviewer.md")
        );
        // Copilot's `.agent.md` file is discovered via the uniform scan; its
        // display name strips the full `.agent.md` extension.
        let copilot = specialists
            .iter()
            .find(|entry| entry.source == SpecialistSource::Copilot)
            .expect("expected copilot specialist");
        assert_eq!(
            copilot.path,
            home_root.join(".copilot").join("agents").join("user.agent.md")
        );

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(home_root);
    }

    #[test]
    fn scope_specialists_keeps_wta_and_active_agent_only() {
        let all = vec![
            entry(SpecialistSource::Wta, "terminal-agent"),
            entry(SpecialistSource::Copilot, "cop"),
            entry(SpecialistSource::Claude, "cla"),
            entry(SpecialistSource::Gemini, "gem"),
            entry(SpecialistSource::Codex, "cdx"),
        ];

        let scoped = scope_specialists_to_agent(all.clone(), Some(SpecialistSource::Claude));
        let names: Vec<_> = scoped.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(names, vec!["terminal-agent", "cla"]);

        // Unknown / custom agent → WTA only.
        let wta_only = scope_specialists_to_agent(all, None);
        let names: Vec<_> = wta_only.iter().map(|e| e.display_name.as_str()).collect();
        assert_eq!(names, vec!["terminal-agent"]);
    }

    #[test]
    fn from_agent_id_maps_known_clis_only() {
        assert_eq!(
            SpecialistSource::from_agent_id("copilot"),
            Some(SpecialistSource::Copilot)
        );
        assert_eq!(
            SpecialistSource::from_agent_id("claude"),
            Some(SpecialistSource::Claude)
        );
        assert_eq!(
            SpecialistSource::from_agent_id("gemini"),
            Some(SpecialistSource::Gemini)
        );
        assert_eq!(
            SpecialistSource::from_agent_id("codex"),
            Some(SpecialistSource::Codex)
        );
        assert_eq!(SpecialistSource::from_agent_id("custom:foo"), None);
        assert_eq!(SpecialistSource::from_agent_id(""), None);
    }

    #[test]
    fn strip_specialist_extension_handles_known_suffixes() {
        assert_eq!(strip_specialist_extension("devops.md"), "devops");
        assert_eq!(strip_specialist_extension("user.agent.md"), "user");
        assert_eq!(strip_specialist_extension("plain"), "plain");
    }

    #[test]
    fn display_name_from_path_uses_file_stem() {
        assert_eq!(
            specialist_display_name_from_path(Path::new("/a/b/AGENTS.md")),
            "AGENTS"
        );
        assert_eq!(
            specialist_display_name_from_path(Path::new("/a/b/review.agent.md")),
            "review"
        );
    }

    #[test]
    fn selection_distinguishes_path_from_bare_name() {
        // A bare name resolves to itself (extension stripped); a path-like
        // selection resolves to the file stem of the path.
        assert_eq!(specialist_display_name_for_selection("devops"), "devops");
        assert_eq!(specialist_display_name_for_selection("devops.md"), "devops");
        assert!(specialist_path_from_selection(Some("devops")).is_none());
        assert_eq!(
            specialist_path_from_selection(Some("/repo/.claude/agents/x.md")),
            Some(PathBuf::from("/repo/.claude/agents/x.md"))
        );
        assert_eq!(
            specialist_display_name_for_selection("/repo/.claude/agents/x.md"),
            "x"
        );
    }

    #[test]
    fn find_git_repo_root_walks_up_to_dot_git() {
        let repo_root = temp_prompt_root("git-root");
        let nested = repo_root.join("a").join("b").join("c");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(repo_root.join(".git")).unwrap();

        assert_eq!(find_git_repo_root(&nested).as_deref(), Some(repo_root.as_path()));

        // No `.git` anywhere up the chain → None.
        let orphan = temp_prompt_root("git-orphan");
        let orphan_nested = orphan.join("x");
        fs::create_dir_all(&orphan_nested).unwrap();
        assert!(find_git_repo_root(&orphan_nested).is_none());

        let _ = fs::remove_dir_all(repo_root);
        let _ = fs::remove_dir_all(orphan);
    }
}
