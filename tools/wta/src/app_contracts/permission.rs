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
}
