#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionConfigValue {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpSessionConfigOption {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub current_value: String,
    pub values: Vec<AcpSessionConfigValue>,
    pub native_yolo: bool,
}

impl AcpSessionConfigOption {
    pub fn is_model(&self) -> bool {
        self.category.as_deref() == Some("model") || self.id == "model"
    }

    pub fn current_value_name(&self) -> &str {
        self.values
            .iter()
            .find(|value| value.id == self.current_value)
            .map(|value| value.name.as_str())
            .unwrap_or(&self.current_value)
    }
}
