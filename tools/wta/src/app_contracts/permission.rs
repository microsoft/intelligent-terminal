pub const SESSION_DIRECTORY_GRANT_KIND: &str = "intellterm_session_directory_grant";
pub const GLOBAL_DIRECTORY_GRANT_KIND: &str = "intellterm_global_directory_grant";

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PermOption {
    pub id: String,
    pub name: String,
    pub kind: String,
}

impl PermOption {
    /// True if this is an "allow" option. Case-insensitive because `kind`
    /// is the ACP `PermissionOptionKind` rendered via `format!("{:?}", ...)`.
    pub fn is_allow(&self) -> bool {
        self.kind
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("allow"))
    }

    /// True if this is a "reject" option.
    pub fn is_reject(&self) -> bool {
        self.kind
            .get(..6)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("reject"))
    }

    pub fn is_session_directory_grant(&self) -> bool {
        self.kind == SESSION_DIRECTORY_GRANT_KIND
    }

    pub fn is_global_directory_grant(&self) -> bool {
        self.kind == GLOBAL_DIRECTORY_GRANT_KIND
    }
}
