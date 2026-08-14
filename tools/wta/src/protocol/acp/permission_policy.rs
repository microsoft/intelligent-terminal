use std::path::{Component, Path, PathBuf};

use acp::schema::v1::{
    PermissionOption, PermissionOptionId, PermissionOptionKind, ToolCallLocation, ToolKind,
};
use agent_client_protocol as acp;
use std::sync::{Arc, RwLock};

use crate::agent_source::AgentSource;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum ConfirmationPolicy {
    Auto,
    Deny,
    #[default]
    Prompt,
}

impl ConfirmationPolicy {
    pub(crate) fn parse(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Self::Auto,
            "deny" => Self::Deny,
            "prompt" => Self::Prompt,
            _ => Self::Prompt,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct OperationPolicies {
    pub(crate) read: ConfirmationPolicy,
    pub(crate) create: ConfirmationPolicy,
    pub(crate) input: ConfirmationPolicy,
}

impl OperationPolicies {
    pub(crate) fn new(read: &str, create: &str, input: &str) -> Self {
        Self {
            read: ConfirmationPolicy::parse(read),
            create: ConfirmationPolicy::parse(create),
            input: ConfirmationPolicy::parse(input),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SharedOperationPolicies(Arc<RwLock<OperationPolicies>>);

impl SharedOperationPolicies {
    pub(crate) fn new(policies: OperationPolicies) -> Self {
        Self(Arc::new(RwLock::new(policies)))
    }

    pub(crate) fn snapshot(&self) -> OperationPolicies {
        *self.0.read().unwrap_or_else(|error| error.into_inner())
    }

    pub(crate) fn replace(&self, policies: OperationPolicies) {
        *self.0.write().unwrap_or_else(|error| error.into_inner()) = policies;
    }
}

impl Default for SharedOperationPolicies {
    fn default() -> Self {
        Self::new(OperationPolicies::default())
    }
}

impl From<OperationPolicies> for SharedOperationPolicies {
    fn from(policies: OperationPolicies) -> Self {
        Self::new(policies)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperationClass {
    Read,
    Create,
    Input,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PromptReason {
    Policy,
    UnknownOperation,
    MissingLocation,
    LocationOutsideRoots,
    UnsafePathResolution,
    MissingAllowOnce,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PermissionDecision {
    AutoApprove {
        option_id: PermissionOptionId,
        operation: OperationClass,
    },
    AutoReject {
        option_id: Option<PermissionOptionId>,
        operation: OperationClass,
    },
    Prompt {
        reason: PromptReason,
    },
}

pub(crate) fn evaluate(
    kind: Option<&ToolKind>,
    locations: &[ToolCallLocation],
    options: &[PermissionOption],
    roots: &[PathBuf],
    source: &AgentSource,
    policies: OperationPolicies,
) -> PermissionDecision {
    let Some(operation) = classify(kind) else {
        return PermissionDecision::Prompt {
            reason: PromptReason::UnknownOperation,
        };
    };
    let policy = match operation {
        OperationClass::Read => policies.read,
        OperationClass::Create => policies.create,
        OperationClass::Input => policies.input,
    };

    match policy {
        ConfirmationPolicy::Prompt => PermissionDecision::Prompt {
            reason: PromptReason::Policy,
        },
        ConfirmationPolicy::Deny => PermissionDecision::AutoReject {
            option_id: option_id(options, PermissionOptionKind::RejectOnce),
            operation,
        },
        ConfirmationPolicy::Auto => {
            if operation != OperationClass::Input {
                if locations.is_empty() {
                    return PermissionDecision::Prompt {
                        reason: PromptReason::MissingLocation,
                    };
                }
                if matches!(source, AgentSource::Wsl { .. }) {
                    return PermissionDecision::Prompt {
                        reason: PromptReason::UnsafePathResolution,
                    };
                }
                for location in locations {
                    match is_contained_by_any_root(&location.path, roots) {
                        Ok(true) => {}
                        Ok(false) => {
                            return PermissionDecision::Prompt {
                                reason: PromptReason::LocationOutsideRoots,
                            };
                        }
                        Err(()) => {
                            return PermissionDecision::Prompt {
                                reason: PromptReason::UnsafePathResolution,
                            };
                        }
                    }
                }
            }

            match option_id(options, PermissionOptionKind::AllowOnce) {
                Some(option_id) => PermissionDecision::AutoApprove {
                    option_id,
                    operation,
                },
                None => PermissionDecision::Prompt {
                    reason: PromptReason::MissingAllowOnce,
                },
            }
        }
    }
}

fn classify(kind: Option<&ToolKind>) -> Option<OperationClass> {
    match kind? {
        ToolKind::Read | ToolKind::Search => Some(OperationClass::Read),
        ToolKind::Edit | ToolKind::Move | ToolKind::Delete => Some(OperationClass::Create),
        ToolKind::Execute => Some(OperationClass::Input),
        _ => None,
    }
}

fn option_id(
    options: &[PermissionOption],
    expected: PermissionOptionKind,
) -> Option<PermissionOptionId> {
    options
        .iter()
        .find(|option| option.kind == expected)
        .map(|option| option.option_id.clone())
}

fn is_contained_by_any_root(path: &Path, roots: &[PathBuf]) -> Result<bool, ()> {
    let path = resolve_allow_missing(path)?;
    for root in roots {
        let root = resolve_allow_missing(root)?;
        if components_start_with(&path, &root) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn resolve_allow_missing(path: &Path) -> Result<PathBuf, ()> {
    if !path.is_absolute() {
        return Err(());
    }
    let mut existing = path;
    let mut suffix = Vec::new();
    while !existing.exists() {
        let Some(name) = existing.file_name() else {
            return Err(());
        };
        suffix.push(name.to_os_string());
        existing = existing.parent().ok_or(())?;
    }
    let mut resolved = std::fs::canonicalize(existing).map_err(|_| ())?;
    for component in suffix.iter().rev() {
        resolved.push(component);
    }
    Ok(resolved)
}

fn components_start_with(path: &Path, root: &Path) -> bool {
    let mut path_components = path.components();
    for root_component in root.components() {
        let Some(path_component) = path_components.next() else {
            return false;
        };
        if !component_eq(path_component, root_component) {
            return false;
        }
    }
    true
}

fn component_eq(left: Component<'_>, right: Component<'_>) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let path = std::env::temp_dir()
                .join(format!("wta-permission-policy-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).expect("create test directory");
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn option(kind: PermissionOptionKind) -> PermissionOption {
        PermissionOption::new(
            PermissionOptionId::new(format!("{kind:?}")),
            format!("{kind:?}"),
            kind,
        )
    }

    #[test]
    fn malformed_policy_fails_closed_to_prompt() {
        assert_eq!(
            ConfirmationPolicy::parse("unexpected"),
            ConfirmationPolicy::Prompt
        );
    }

    #[test]
    fn execute_auto_requires_allow_once_but_not_a_path() {
        let decision = evaluate(
            Some(&ToolKind::Execute),
            &[],
            &[option(PermissionOptionKind::AllowOnce)],
            &[],
            &AgentSource::Host,
            OperationPolicies::new("prompt", "prompt", "auto"),
        );
        assert!(matches!(
            decision,
            PermissionDecision::AutoApprove {
                operation: OperationClass::Input,
                ..
            }
        ));
    }

    #[test]
    fn deny_selects_reject_once() {
        let decision = evaluate(
            Some(&ToolKind::Delete),
            &[],
            &[option(PermissionOptionKind::RejectOnce)],
            &[],
            &AgentSource::Host,
            OperationPolicies::new("prompt", "deny", "prompt"),
        );
        assert!(matches!(
            decision,
            PermissionDecision::AutoReject {
                option_id: Some(_),
                operation: OperationClass::Create,
            }
        ));
    }

    #[test]
    fn read_outside_roots_prompts() {
        let temp = TestDirectory::new();
        let root = temp.0.join("allowed");
        let outside = temp.0.join("outside");
        std::fs::create_dir_all(&root).expect("allowed dir");
        std::fs::create_dir_all(&outside).expect("outside dir");
        let decision = evaluate(
            Some(&ToolKind::Read),
            &[ToolCallLocation::new(outside.join("file.txt"))],
            &[option(PermissionOptionKind::AllowOnce)],
            &[root],
            &AgentSource::Host,
            OperationPolicies::new("auto", "prompt", "prompt"),
        );
        assert_eq!(
            decision,
            PermissionDecision::Prompt {
                reason: PromptReason::LocationOutsideRoots,
            }
        );
    }

    #[test]
    fn read_nonexistent_child_inside_root_is_approved() {
        let temp = TestDirectory::new();
        let root = temp.0.join("allowed");
        std::fs::create_dir_all(&root).expect("allowed dir");
        let decision = evaluate(
            Some(&ToolKind::Read),
            &[ToolCallLocation::new(root.join("new").join("file.txt"))],
            &[option(PermissionOptionKind::AllowOnce)],
            std::slice::from_ref(&root),
            &AgentSource::Host,
            OperationPolicies::new("auto", "prompt", "prompt"),
        );
        assert!(matches!(
            decision,
            PermissionDecision::AutoApprove {
                operation: OperationClass::Read,
                ..
            }
        ));
    }
}
