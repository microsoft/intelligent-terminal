#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionCommand {
    pub name: String,
    pub description: String,
    pub input_hint: Option<String>,
}
