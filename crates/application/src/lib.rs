//! Application authorization and publishing use cases.

pub mod upload;

#[derive(Debug, Clone)]
pub struct Principal {
    pub id: String,
    pub username: String,
    pub server_admin: bool,
    pub publish_permission: bool,
}

impl Principal {
    pub fn may_create_app(&self) -> bool {
        self.server_admin || self.publish_permission
    }
    pub fn may_manage(&self, owner_id: &str) -> bool {
        self.server_admin || self.id == owner_id
    }
    pub fn may_publish(&self, owner_id: &str) -> bool {
        self.may_manage(owner_id) && self.may_create_app()
    }
}

pub fn may_read_catalog(
    principal: Option<&Principal>,
    owner_id: &str,
    public_published: bool,
    anonymous_enabled: bool,
) -> bool {
    principal
        .map(|actor| actor.may_manage(owner_id))
        .unwrap_or(false)
        || (public_published && (principal.is_some() || anonymous_enabled))
}

#[cfg(test)]
mod tests {
    use super::Principal;

    #[test]
    fn revocation_preserves_management_but_blocks_publish() {
        let owner = Principal {
            id: "owner".into(),
            username: "owner".into(),
            server_admin: false,
            publish_permission: false,
        };
        assert!(owner.may_manage("owner"));
        assert!(!owner.may_publish("owner"));
        assert!(!owner.may_manage("other"));
    }
}
