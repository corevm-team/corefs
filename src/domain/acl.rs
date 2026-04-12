use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Principal {
    User(String),
    Group(String),
    Role(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AclEntry {
    pub principal: Principal,
    pub can_read: bool,
    pub can_write: bool,
    pub can_execute: bool,
}

impl AclEntry {
    pub fn full_access(principal: Principal) -> Self {
        Self {
            principal,
            can_read: true,
            can_write: true,
            can_execute: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_access_grants_all_permissions() {
        let entry = AclEntry::full_access(Principal::User("alice".to_string()));

        assert!(entry.can_read);
        assert!(entry.can_write);
        assert!(entry.can_execute);
        assert_eq!(entry.principal, Principal::User("alice".to_string()));
    }
}
