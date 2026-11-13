/// A file identity is typed separately from a CAS digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileArtifactId(String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeArtifactId(String);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct App {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub owner_id: String,
    pub visibility: String,
    pub state: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Release {
    pub id: String,
    pub app_id: String,
    pub state: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Association {
    pub id: String,
    pub release_id: String,
    pub artifact_id: String,
}

pub fn base_slug(name: &str) -> Option<String> {
    if name.trim().is_empty() {
        return None;
    }
    let mut out = String::new();
    let mut separator = false;
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            if separator && !out.is_empty() {
                out.push('-');
            }
            out.push(c.to_ascii_lowercase());
            separator = false;
        } else {
            separator = true;
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

impl FileArtifactId {
    pub fn parse(value: &str) -> Option<Self> {
        let digest = value.strip_prefix("adfile:sha256:")?;
        if digest.len() == 64
            && digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            Some(Self(value.to_owned()))
        } else {
            None
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TreeArtifactId {
    pub fn parse(value: &str) -> Option<Self> {
        let digest = value.strip_prefix("adtree:v1:sha256:")?;
        if digest.len() == 64
            && digest
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            Some(Self(value.to_owned()))
        } else {
            None
        }
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_identity_is_typed_and_canonical() {
        let valid = format!("adfile:sha256:{}", "a".repeat(64));
        assert_eq!(FileArtifactId::parse(&valid).unwrap().as_str(), valid);
        assert!(FileArtifactId::parse(&"a".repeat(64)).is_none());
        assert!(FileArtifactId::parse(&format!("adfile:sha256:{}", "A".repeat(64))).is_none());
    }

    #[test]
    fn slug_is_separate_from_identity() {
        assert_eq!(base_slug("My App 2"), Some("my-app-2".into()));
        assert_eq!(base_slug("  "), None);
    }
}
pub mod tree;
