use std::time::SystemTime;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub id: u64,
    pub name: String,
    pub created_at: SystemTime,
    pub paths: Vec<String>,
}
