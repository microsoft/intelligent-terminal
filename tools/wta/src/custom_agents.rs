//! Custom agent discovery + parsing (`.agent.md` files).
//!
//! A *custom agent* is a `.agent.md` Markdown file with YAML frontmatter that
//! overrides the built-in `terminal-agent` system prompt for a single agent
//! pane / tab. The file format deliberately mirrors GitHub Copilot's
//! `.agent.md` so definitions are portable across the ecosystem:
//!
//! ```text
//! ---
//! name: devops-helper
//! description: 'Diagnoses failing CI/build commands and proposes fixes.'
//! model: claude-haiku-4.5
//! ---
//! You are a DevOps specialist focused on build/CI failures...
//! ```
//!
//! Discovery scopes, highest priority first (later scope wins on id collision):
//!   1. Project: `<cwd..git-root>/.intelligent-terminal/agents/*.md` (walked up)
//!   2. User:    `~/.intelligent-terminal/agents/*.md`
//!   3. Built-in `terminal-agent` (always present, lowest priority)
//!
//! The IT-neutral dirs above are backend-agnostic (Intelligent Terminal's own
//! home dotdir, like `~/.claude` / `~/.copilot`). Phase B adds *provider-native*
//! sources selected by the active backend CLI (Copilot `.github/agents`, Claude
//! `.claude/agents`, …) so `/agent` also surfaces the agents you already defined
//! for whatever CLI the pane is running.
//!
//! The list is re-scanned every time the `/agent` picker opens, so adding,
//! editing, or deleting a `.agent.md` file takes effect with no restart — the
//! file on disk is the single source of truth (Copilot-CLI style). There is no
//! registry.
//!
//! This module only *discovers and parses* agents. Applying a selected agent's
//! system prompt (`body`) / `model` to an ACP session is handled elsewhere
//! (`protocol::acp::prompt` + the helper session flow).

use std::fs;
use std::path::{Path, PathBuf};

/// Stable id of the built-in default agent (the embedded `terminal-agent`
/// prompt). Selecting it reverts a tab to the default behavior.
pub const BUILTIN_AGENT_ID: &str = "terminal-agent";

/// Where a discovered agent came from. Higher variants win on id collision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentScope {
    /// The embedded default `terminal-agent`.
    BuiltIn,
    /// `~/.github/agents/`.
    User,
    /// `<cwd>/.github/agents/`.
    Project,
}

/// A custom agent definition parsed from a `.agent.md` file (or the built-in).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomAgent {
    /// Stable identifier used by `/agent` selection and the helper flag.
    /// Derived from frontmatter `name` (trimmed) or, when absent, the file
    /// stem with any trailing `.agent` removed. Compared case-insensitively.
    pub id: String,
    /// Human-friendly name shown in the picker. Frontmatter `name` verbatim,
    /// else the id.
    pub display_name: String,
    /// Frontmatter `description` — the routing/discovery hint shown in the UI.
    pub description: String,
    /// Frontmatter `model`, applied only when the running CLI supports a model
    /// flag. `None` = inherit the session's current model.
    pub model: Option<String>,
    /// Frontmatter `agent` (a CLI switch such as `claude`/`copilot`). Parsed
    /// for forward-compat but IGNORED in the MVP — master stays single-CLI.
    pub agent_cli: Option<String>,
    /// Frontmatter `tools` restriction. Parsed but not yet enforced.
    pub tools: Vec<String>,
    /// Markdown body after the frontmatter — used as the ACP system prompt.
    /// Empty for the built-in (the default loader supplies its prompt).
    pub body: String,
    /// Discovery scope this agent came from.
    pub scope: AgentScope,
    /// Absolute path of the source file (`None` for the built-in).
    pub source_path: Option<PathBuf>,
}

impl CustomAgent {
    /// True for the built-in default agent.
    pub fn is_builtin(&self) -> bool {
        self.scope == AgentScope::BuiltIn
    }
}

/// The built-in default agent entry. Its `body` is intentionally empty: when
/// this agent is active, the default embedded `terminal-agent` prompt is used
/// via the existing prompt loader rather than a body carried here.
pub fn builtin_agent() -> CustomAgent {
    CustomAgent {
        id: BUILTIN_AGENT_ID.to_string(),
        display_name: "Terminal Agent".to_string(),
        description: "Default Intelligent Terminal assistant.".to_string(),
        model: None,
        agent_cli: None,
        tools: Vec::new(),
        body: String::new(),
        scope: AgentScope::BuiltIn,
        source_path: None,
    }
}

/// A discovery source: a set of home-relative user dirs and repo-relative
/// project dirs (scanned per level while walking up when `walk_up`). Phase A
/// ships only the IT-neutral source; Phase B appends provider-native sources
/// (Copilot `.github/agents`, Claude `.claude/agents`, …) selected by the
/// active backend.
struct AgentSource {
    /// Dirs relative to the user's home directory, e.g.
    /// `.intelligent-terminal/agents`.
    user_rel: &'static [&'static str],
    /// Dirs relative to each project level, e.g. `.intelligent-terminal/agents`.
    project_rel: &'static [&'static str],
    /// When true, `project_rel` is scanned at every directory from the cwd up
    /// to (and including) the git root; otherwise only at the cwd.
    walk_up: bool,
}

/// The always-on, backend-agnostic IT source. Branded to Intelligent Terminal
/// (its own home dotdir, like `~/.claude` / `~/.copilot`) — deliberately NOT
/// `.github`, which is Copilot's provider convention (added in Phase B).
fn neutral_sources() -> Vec<AgentSource> {
    vec![AgentSource {
        user_rel: &[".intelligent-terminal/agents"],
        project_rel: &[".intelligent-terminal/agents"],
        walk_up: true,
    }]
}

/// Discover all custom agents visible from `project_dir` (walked up to the git
/// root for a project agents folder) and `user_home` (the IT-neutral user
/// dir), plus the always-present built-in.
///
/// Priority (later wins on id collision): **Project > User > BuiltIn**, and the
/// override keeps the earlier display slot. Within the project scope, a level
/// closer to the cwd wins over an ancestor.
///
/// Both roots are optional so this is unit-testable with temp directories and
/// so a missing `$HOME` / cwd degrades gracefully to fewer agents.
pub fn discover_agents(project_dir: Option<&Path>, user_home: Option<&Path>) -> Vec<CustomAgent> {
    let mut agents = vec![builtin_agent()];
    let sources = neutral_sources();

    // USER scope first so PROJECT upserts over it. (When Phase B adds provider
    // sources, they are appended after neutral within each scope, so
    // provider-native wins over IT-neutral at the same scope.)
    if let Some(home) = user_home {
        for source in &sources {
            for rel in source.user_rel {
                collect_from_dir(&join_rel(home, rel), AgentScope::User, &mut agents);
            }
        }
    }

    // PROJECT scope: scan each level from the git root down to the cwd, so the
    // level closest to the cwd is processed last and wins on collision.
    if let Some(cwd) = project_dir {
        for source in &sources {
            let levels = if source.walk_up {
                project_levels(cwd)
            } else {
                vec![cwd.to_path_buf()]
            };
            for level in &levels {
                for rel in source.project_rel {
                    collect_from_dir(&join_rel(level, rel), AgentScope::Project, &mut agents);
                }
            }
        }
    }

    agents
}

/// Join a `/`-separated relative path onto `base`, one segment at a time (so it
/// works regardless of the platform path separator).
fn join_rel(base: &Path, rel: &str) -> PathBuf {
    let mut p = base.to_path_buf();
    for seg in rel.split('/').filter(|s| !s.is_empty()) {
        p.push(seg);
    }
    p
}

/// The project directory levels to scan, ordered **git-root-first, cwd-last**,
/// so callers that upsert (last-wins) let the cwd-closest definition win.
///
/// The repo root is the nearest ancestor (including `cwd`) that contains a
/// `.git` entry. When no git root is found we scan only the cwd (matching
/// Codex: "if it cannot find a project root, it only checks the current
/// directory") rather than walking to the filesystem root.
fn project_levels(cwd: &Path) -> Vec<PathBuf> {
    let git_root = cwd.ancestors().find(|a| a.join(".git").exists());
    match git_root {
        Some(root) => {
            let mut levels: Vec<PathBuf> = Vec::new();
            for anc in cwd.ancestors() {
                levels.push(anc.to_path_buf());
                if anc == root {
                    break;
                }
            }
            levels.reverse(); // root-first, cwd-last
            levels
        }
        None => vec![cwd.to_path_buf()],
    }
}

/// Convenience wrapper over [`discover_agents`] using the real current
/// directory and user home directory.
pub fn discover_agents_default() -> Vec<CustomAgent> {
    let cwd = std::env::current_dir().ok();
    let home = user_home_dir();
    discover_agents(cwd.as_deref(), home.as_deref())
}

/// Find a discovered agent by id (case-insensitive).
pub fn find_agent_by_id<'a>(agents: &'a [CustomAgent], id: &str) -> Option<&'a CustomAgent> {
    agents.iter().find(|a| a.id.eq_ignore_ascii_case(id))
}

/// Resolve the user's home directory without pulling in the `dirs`/`home`
/// crates (avoids a new dependency + third-party-notice regeneration). On
/// Windows `USERPROFILE` is authoritative; `HOME` is the cross-platform / test
/// fallback.
fn user_home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

/// Scan `dir` for `*.md` agent files and upsert each into `agents`.
///
/// Files are processed in sorted filename order for deterministic results.
/// Unreadable or unparsable files are skipped silently — a broken agent file
/// must never take down the picker.
fn collect_from_dir(dir: &Path, scope: AgentScope, agents: &mut Vec<CustomAgent>) {
    let mut entries: Vec<PathBuf> = match fs::read_dir(dir) {
        Ok(read) => read
            .flatten()
            .map(|e| e.path())
            .filter(|p| is_agent_file(p))
            .collect(),
        Err(_) => return,
    };
    entries.sort();

    for path in entries {
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Some(agent) = parse_agent(&text, &path, scope) {
            upsert(agents, agent);
        }
    }
}

/// True for files that VS Code / Copilot treat as agent definitions: any `.md`
/// file in the agents directory (including the `*.agent.md` convention).
fn is_agent_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e.eq_ignore_ascii_case("md"))
}

/// Insert `agent`, or replace an existing same-id entry in place (so a
/// higher-priority scope wins while keeping the earlier display slot).
fn upsert(agents: &mut Vec<CustomAgent>, agent: CustomAgent) {
    if let Some(existing) = agents.iter_mut().find(|a| a.id.eq_ignore_ascii_case(&agent.id)) {
        *existing = agent;
    } else {
        agents.push(agent);
    }
}

/// Parse one `.agent.md` file into a [`CustomAgent`]. Returns `None` only when
/// the file yields no usable id (should not happen — the file stem is always a
/// fallback), so parsing is otherwise infallible and tolerant of missing
/// fields.
fn parse_agent(text: &str, path: &Path, scope: AgentScope) -> Option<CustomAgent> {
    let (frontmatter, body) = split_frontmatter(text);

    let name = frontmatter
        .as_ref()
        .and_then(|fm| fm.scalar("name"))
        .map(str::to_string);

    let id = name
        .clone()
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| file_stem_id(path))?;

    let display_name = name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .unwrap_or(&id)
        .to_string();

    let description = frontmatter
        .as_ref()
        .and_then(|fm| fm.scalar("description"))
        .unwrap_or("")
        .trim()
        .to_string();

    let model = frontmatter
        .as_ref()
        .and_then(|fm| fm.scalar("model"))
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map(str::to_string);

    let agent_cli = frontmatter
        .as_ref()
        .and_then(|fm| fm.scalar("agent"))
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .map(str::to_string);

    let tools = frontmatter
        .as_ref()
        .map(|fm| fm.list("tools"))
        .unwrap_or_default();

    Some(CustomAgent {
        id,
        display_name,
        description,
        model,
        agent_cli,
        tools,
        body: body.trim().to_string(),
        scope,
        source_path: Some(path.to_path_buf()),
    })
}

/// Derive an id from a file path: the stem with any trailing `.agent` removed
/// (so `devops-helper.agent.md` → `devops-helper`).
fn file_stem_id(path: &Path) -> Option<String> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    let stem = stem.strip_suffix(".agent").unwrap_or(stem);
    let stem = stem.trim();
    (!stem.is_empty()).then(|| stem.to_string())
}

// ─── Minimal YAML-frontmatter parser ─────────────────────────────────────────
//
// We deliberately avoid a YAML crate (serde_yaml is unmaintained and any new
// dependency forces a third-party-notice regeneration — see
// `rust-wta.instructions.md`). Agent frontmatter only uses a tiny subset:
// `key: scalar`, single/double-quoted scalars, inline arrays `[a, b]`, and
// block lists (`-` items). This parser covers exactly that and treats anything
// it doesn't understand as absent rather than erroring.

/// Parsed frontmatter: an ordered set of `key -> value` where a value is either
/// a scalar string or a list of strings.
struct Frontmatter {
    entries: Vec<(String, FmValue)>,
}

enum FmValue {
    Scalar(String),
    List(Vec<String>),
}

impl Frontmatter {
    fn scalar(&self, key: &str) -> Option<&str> {
        self.entries.iter().find_map(|(k, v)| match v {
            FmValue::Scalar(s) if k == key => Some(s.as_str()),
            _ => None,
        })
    }

    fn list(&self, key: &str) -> Vec<String> {
        self.entries
            .iter()
            .find_map(|(k, v)| match v {
                FmValue::List(items) if k == key => Some(items.clone()),
                FmValue::Scalar(s) if k == key => Some(parse_inline_array(s)),
                _ => None,
            })
            .unwrap_or_default()
    }
}

/// Split a document into `(frontmatter, body)`. When the text does not begin
/// with a `---` fence, the whole document is the body and there is no
/// frontmatter.
fn split_frontmatter(text: &str) -> (Option<Frontmatter>, String) {
    // Tolerate a UTF-8 BOM and leading blank lines before the opening fence.
    let trimmed = text.trim_start_matches('\u{feff}');
    let mut lines = trimmed.lines();

    // The first non-empty line must be exactly `---` to start frontmatter.
    let mut consumed_leading = 0usize;
    let first = loop {
        match lines.next() {
            Some(l) if l.trim().is_empty() => {
                consumed_leading += l.len() + 1;
                continue;
            }
            other => break other,
        }
    };
    if first.map(|l| l.trim()) != Some("---") {
        return (None, text.to_string());
    }

    let mut fm_lines: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        fm_lines.push(line);
    }
    if !closed {
        // Unterminated frontmatter — treat the whole file as body.
        return (None, text.to_string());
    }

    let body: String = lines.collect::<Vec<_>>().join("\n");
    let _ = consumed_leading;
    (Some(parse_frontmatter_lines(&fm_lines)), body)
}

fn parse_frontmatter_lines(lines: &[&str]) -> Frontmatter {
    let mut entries: Vec<(String, FmValue)> = Vec::new();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        i += 1;

        let line = strip_comment(raw);
        if line.trim().is_empty() {
            continue;
        }
        // Only treat top-level (non-indented) `key:` lines as new keys; block
        // list items (`  - x`) are consumed by the look-ahead below.
        let Some(colon) = line.find(':') else {
            continue;
        };
        let key = line[..colon].trim();
        if key.is_empty() || key.starts_with('-') {
            continue;
        }
        let value = line[colon + 1..].trim();

        if value.is_empty() {
            // Possible block list on following indented `-` lines.
            let mut items = Vec::new();
            while i < lines.len() {
                let peek = strip_comment(lines[i]);
                let peek_trim = peek.trim();
                if peek_trim.is_empty() {
                    i += 1;
                    continue;
                }
                if let Some(item) = peek_trim.strip_prefix('-') {
                    items.push(unquote(item.trim()));
                    i += 1;
                } else {
                    break;
                }
            }
            if items.is_empty() {
                entries.push((key.to_string(), FmValue::Scalar(String::new())));
            } else {
                entries.push((key.to_string(), FmValue::List(items)));
            }
        } else if value.starts_with('[') {
            entries.push((key.to_string(), FmValue::List(parse_inline_array(value))));
        } else {
            entries.push((key.to_string(), FmValue::Scalar(unquote(value))));
        }
    }

    Frontmatter { entries }
}

/// Strip a trailing ` # comment` from a frontmatter line, but not a `#` inside
/// a quoted scalar. Cheap heuristic: only strip when the `#` is preceded by
/// whitespace and is outside quotes.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut prev_ws = true;
    for (idx, &b) in bytes.iter().enumerate() {
        match b {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'#' if !in_single && !in_double && prev_ws => return &line[..idx],
            _ => {}
        }
        prev_ws = b == b' ' || b == b'\t';
    }
    line
}

/// Remove matching surrounding single or double quotes from a scalar.
fn unquote(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let first = bytes[0];
        let last = bytes[s.len() - 1];
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            return s[1..s.len() - 1].to_string();
        }
    }
    s.to_string()
}

/// Parse an inline YAML array like `['read', "edit", search]` into items.
fn parse_inline_array(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|item| unquote(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A throwaway temp directory that cleans itself up on drop. Avoids adding
    /// the `tempfile` crate (dependency + notice regeneration).
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let path = std::env::temp_dir().join(format!(
                "wta-ca-test-{}-{}-{}",
                std::process::id(),
                n,
                nanos
            ));
            fs::create_dir_all(&path).unwrap();
            TempDir { path }
        }

        fn write_agent(&self, rel: &str, contents: &str) {
            self.write_agent_in(".", rel, contents);
        }

        /// Write an agent file under `<self.path>/<level>/.intelligent-terminal/agents/`.
        /// `level` is a `/`-separated subpath (use "." for the root).
        fn write_agent_in(&self, level: &str, rel: &str, contents: &str) {
            let mut dir = self.path.clone();
            if level != "." {
                for seg in level.split('/') {
                    dir.push(seg);
                }
            }
            dir.push(".intelligent-terminal");
            dir.push("agents");
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(rel), contents).unwrap();
        }

        /// Mark a subdirectory (or the root, ".") as a git repo root so
        /// `project_levels` walk-up stops there.
        fn make_git_root(&self, level: &str) {
            let mut dir = self.path.clone();
            if level != "." {
                for seg in level.split('/') {
                    dir.push(seg);
                }
            }
            fs::create_dir_all(dir.join(".git")).unwrap();
        }

        /// Absolute path to a `/`-separated subdirectory of this temp dir.
        fn sub(&self, level: &str) -> PathBuf {
            let mut dir = self.path.clone();
            for seg in level.split('/') {
                dir.push(seg);
            }
            dir
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn builtin_is_always_present_and_first() {
        let agents = discover_agents(None, None);
        assert_eq!(agents.len(), 1);
        assert!(agents[0].is_builtin());
        assert_eq!(agents[0].id, BUILTIN_AGENT_ID);
    }

    #[test]
    fn parses_frontmatter_and_body() {
        let project = TempDir::new();
        project.write_agent(
            "devops-helper.agent.md",
            "---\nname: devops-helper\ndescription: 'Fixes failing builds.'\nmodel: claude-haiku-4.5\n---\nYou are a DevOps specialist.\nHelp fix CI failures.\n",
        );

        let agents = discover_agents(Some(&project.path), None);
        let agent = find_agent_by_id(&agents, "devops-helper").expect("found");
        assert_eq!(agent.id, "devops-helper");
        assert_eq!(agent.display_name, "devops-helper");
        assert_eq!(agent.description, "Fixes failing builds.");
        assert_eq!(agent.model.as_deref(), Some("claude-haiku-4.5"));
        assert_eq!(agent.scope, AgentScope::Project);
        assert_eq!(
            agent.body,
            "You are a DevOps specialist.\nHelp fix CI failures."
        );
    }

    #[test]
    fn id_falls_back_to_file_stem_without_name() {
        let project = TempDir::new();
        // No `name` in frontmatter → id derives from stem minus `.agent`.
        project.write_agent(
            "reviewer.agent.md",
            "---\ndescription: 'Reviews code.'\n---\nBe a strict reviewer.\n",
        );

        let agents = discover_agents(Some(&project.path), None);
        let agent = find_agent_by_id(&agents, "reviewer").expect("found");
        assert_eq!(agent.id, "reviewer");
        assert_eq!(agent.display_name, "reviewer");
        assert_eq!(agent.description, "Reviews code.");
    }

    #[test]
    fn project_scope_overrides_user_scope_by_id() {
        let user = TempDir::new();
        // Reuse write_agent layout under the "home" temp dir.
        let user_dir = user.path.join(".intelligent-terminal").join("agents");
        fs::create_dir_all(&user_dir).unwrap();
        fs::write(
            user_dir.join("shared.agent.md"),
            "---\nname: shared\ndescription: 'User version.'\n---\nUser body.\n",
        )
        .unwrap();

        let project = TempDir::new();
        project.write_agent(
            "shared.agent.md",
            "---\nname: shared\ndescription: 'Project version.'\n---\nProject body.\n",
        );

        let agents = discover_agents(Some(&project.path), Some(&user.path));
        let shared = find_agent_by_id(&agents, "shared").expect("found");
        // Project wins on collision.
        assert_eq!(shared.description, "Project version.");
        assert_eq!(shared.scope, AgentScope::Project);
        assert_eq!(shared.body, "Project body.");
        // Only one `shared` entry (deduped), plus the built-in.
        assert_eq!(agents.iter().filter(|a| a.id == "shared").count(), 1);
    }

    #[test]
    fn custom_agent_can_override_builtin_and_keeps_first_slot() {
        let project = TempDir::new();
        project.write_agent(
            "terminal-agent.agent.md",
            "---\nname: terminal-agent\ndescription: 'Custom default.'\n---\nCustom default body.\n",
        );

        let agents = discover_agents(Some(&project.path), None);
        assert_eq!(agents[0].id, BUILTIN_AGENT_ID);
        // Overridden in place: scope + description come from the file now.
        assert_eq!(agents[0].scope, AgentScope::Project);
        assert_eq!(agents[0].description, "Custom default.");
    }

    #[test]
    fn parses_inline_and_block_tool_lists() {
        let project = TempDir::new();
        project.write_agent(
            "inline.agent.md",
            "---\nname: inline\ntools: ['read', \"edit\", search]\n---\nbody\n",
        );
        project.write_agent(
            "block.agent.md",
            "---\nname: block\ntools:\n  - read\n  - edit\n---\nbody\n",
        );

        let agents = discover_agents(Some(&project.path), None);
        let inline = find_agent_by_id(&agents, "inline").unwrap();
        assert_eq!(inline.tools, vec!["read", "edit", "search"]);
        let block = find_agent_by_id(&agents, "block").unwrap();
        assert_eq!(block.tools, vec!["read", "edit"]);
    }

    #[test]
    fn agent_frontmatter_field_is_parsed_but_marked_for_the_caller() {
        let project = TempDir::new();
        project.write_agent(
            "switcher.agent.md",
            "---\nname: switcher\nagent: claude\ndescription: 'x'\n---\nbody\n",
        );
        let agents = discover_agents(Some(&project.path), None);
        let a = find_agent_by_id(&agents, "switcher").unwrap();
        assert_eq!(a.agent_cli.as_deref(), Some("claude"));
    }

    #[test]
    fn file_without_frontmatter_uses_stem_and_whole_body() {
        let project = TempDir::new();
        project.write_agent("plain.md", "Just a system prompt with no frontmatter.\n");
        let agents = discover_agents(Some(&project.path), None);
        let a = find_agent_by_id(&agents, "plain").expect("found");
        assert_eq!(a.description, "");
        assert_eq!(a.body, "Just a system prompt with no frontmatter.");
    }

    #[test]
    fn unterminated_frontmatter_is_treated_as_body() {
        let project = TempDir::new();
        project.write_agent("broken.md", "---\nname: broken\nno closing fence\n");
        let agents = discover_agents(Some(&project.path), None);
        let a = find_agent_by_id(&agents, "broken").expect("found");
        // The unterminated block is not parsed as frontmatter.
        assert_eq!(a.id, "broken"); // from file stem
        assert!(a.body.contains("no closing fence"));
    }

    #[test]
    fn strips_trailing_comment_but_not_inside_quotes() {
        assert_eq!(strip_comment("name: foo # a comment").trim(), "name: foo");
        assert_eq!(
            strip_comment("description: 'has # inside'").trim(),
            "description: 'has # inside'"
        );
    }

    #[test]
    fn discovery_is_deterministic_and_sorted() {
        let project = TempDir::new();
        project.write_agent("zeta.agent.md", "---\nname: zeta\n---\nz\n");
        project.write_agent("alpha.agent.md", "---\nname: alpha\n---\na\n");

        let ids: Vec<String> = discover_agents(Some(&project.path), None)
            .into_iter()
            .map(|a| a.id)
            .collect();
        // built-in first, then sorted-by-filename order (alpha before zeta).
        assert_eq!(ids, vec!["terminal-agent", "alpha", "zeta"]);
    }

    #[test]
    fn walks_up_to_git_root_finds_ancestor_agent() {
        let repo = TempDir::new();
        repo.make_git_root(".");
        // Agent defined at the repo root's neutral dir…
        repo.write_agent_in(".", "root-agent.agent.md", "---\nname: root-agent\n---\nr\n");
        // …discovered even when the cwd is a nested subdirectory.
        let cwd = repo.sub("a/b/c");
        fs::create_dir_all(&cwd).unwrap();

        let agents = discover_agents(Some(&cwd), None);
        assert!(
            find_agent_by_id(&agents, "root-agent").is_some(),
            "walk-up must find an agent defined at the git root from a nested cwd"
        );
    }

    #[test]
    fn cwd_level_wins_over_ancestor_on_collision() {
        let repo = TempDir::new();
        repo.make_git_root(".");
        repo.write_agent_in(
            ".",
            "dup.agent.md",
            "---\nname: dup\ndescription: 'Root version.'\n---\nroot\n",
        );
        repo.write_agent_in(
            "a/b",
            "dup.agent.md",
            "---\nname: dup\ndescription: 'Nested version.'\n---\nnested\n",
        );
        let cwd = repo.sub("a/b");

        let agents = discover_agents(Some(&cwd), None);
        let dup = find_agent_by_id(&agents, "dup").expect("found");
        assert_eq!(
            dup.description, "Nested version.",
            "the level closest to the cwd must win over an ancestor"
        );
        assert_eq!(agents.iter().filter(|a| a.id == "dup").count(), 1);
    }

    #[test]
    fn no_git_root_scans_only_cwd_not_ancestors() {
        // No `.git` anywhere under the temp root → ancestors are NOT scanned.
        let root = TempDir::new();
        root.write_agent_in(".", "ancestor.agent.md", "---\nname: ancestor\n---\na\n");
        let cwd = root.sub("x/y");
        fs::create_dir_all(&cwd).unwrap();

        let agents = discover_agents(Some(&cwd), None);
        assert!(
            find_agent_by_id(&agents, "ancestor").is_none(),
            "without a git root, discovery must scan only the cwd, not ancestors"
        );
    }
}
