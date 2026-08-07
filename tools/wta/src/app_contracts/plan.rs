#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanEntryStatus,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PlanEntryStatus {
    Pending,
    InProgress,
    Completed,
}
