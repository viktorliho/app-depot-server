use crate::tree::validate_path;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("{}", self.title())]
pub enum RecipeError {
    Required,
    Invalid,
    UnsupportedVersion,
    Unavailable,
    Cycle,
    Conflict,
    Platform,
    Association,
    Source,
    Collision,
    Launch,
    Limit,
}

impl RecipeError {
    pub fn title(self) -> &'static str {
        match self {
            Self::Required => "Recipe required",
            Self::Invalid => "Invalid RecipeV1",
            Self::UnsupportedVersion => "Unsupported recipe version",
            Self::Unavailable => "Dependency unavailable",
            Self::Cycle => "Dependency cycle",
            Self::Conflict => "Conflicting dependency releases",
            Self::Platform => "Platform mismatch",
            Self::Association => "Artifact association unavailable or ambiguous",
            Self::Source => "Invalid or unavailable assembly source",
            Self::Collision => "Destination collision",
            Self::Launch => "Launch target or working directory is invalid",
            Self::Limit => "Recipe resolution limit exceeded",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecipeV1 {
    pub version: u32,
    #[serde(deserialize_with = "unique_map")]
    pub dependencies: BTreeMap<String, Dependency>,
    #[serde(deserialize_with = "unique_map")]
    pub targets: BTreeMap<String, Target>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Dependency {
    pub app_id: String,
    pub selector: Selector,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Selector {
    Latest,
    Release { release_id: String },
    Channel { name: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub operations: Vec<Copy>,
    pub launch: Launch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Copy {
    pub dependency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub association_label: Option<String>,
    #[serde(default)]
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub executable: bool,
    #[serde(default)]
    pub collision: Collision,
}

#[derive(Debug, Clone, PartialEq, Eq, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Collision {
    #[default]
    Reject,
    ReplaceFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Launch {
    pub path: String,
    #[serde(default)]
    pub working_directory: String,
    #[serde(default)]
    pub arguments: Vec<String>,
}

pub fn platform(value: &str) -> bool {
    matches!(
        value,
        "linux/x86_64"
            | "linux/x86"
            | "windows/x86"
            | "linux/aarch64"
            | "linux/ppc64le"
            | "windows/x86_64"
            | "windows/aarch64"
            | "macos/x86_64"
            | "macos/aarch64"
    )
}

fn uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

pub fn path(value: &str) -> bool {
    value.len() <= 1024
        && value.split('/').count() <= 32
        && validate_path(value).is_ok()
        && !value.contains(':')
        && !value.chars().any(char::is_control)
}

impl RecipeV1 {
    pub fn validate(&self) -> Result<(), RecipeError> {
        use RecipeError::Invalid;
        if self.version != 1 {
            return Err(RecipeError::UnsupportedVersion);
        }
        if self.dependencies.is_empty()
            || self.dependencies.len() > 32
            || self.targets.is_empty()
            || self.targets.len() > 6
        {
            return Err(Invalid);
        }
        for (alias, dep) in &self.dependencies {
            if alias.is_empty()
                || alias.len() > 64
                || !alias
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                || !uuid(&dep.app_id)
            {
                return Err(Invalid);
            }
            match &dep.selector {
                Selector::Release { release_id } if !uuid(release_id) => return Err(Invalid),
                Selector::Channel { name }
                    if name.is_empty()
                        || name.contains('/')
                        || name.len() > 255
                        || name.chars().any(char::is_control) =>
                {
                    return Err(Invalid);
                }
                _ => {}
            }
            if let Some(platforms) = &dep.platforms {
                let distinct: BTreeSet<_> = platforms.iter().collect();
                if distinct.len() != platforms.len()
                    || platforms.is_empty()
                    || !platforms.iter().all(|value| platform(value))
                {
                    return Err(Invalid);
                }
            }
        }
        let mut used = BTreeSet::new();
        for (name, target) in &self.targets {
            if !platform(name) || target.operations.is_empty() || target.operations.len() > 128 {
                return Err(Invalid);
            }
            for op in &target.operations {
                used.insert(&op.dependency);
                if !self.dependencies.contains_key(&op.dependency)
                    || op.association_id.as_deref().is_some_and(|id| !uuid(id))
                    || (op.association_id.is_some() && op.association_label.is_some())
                    || op.association_label.as_ref().is_some_and(|label| {
                        label.trim().is_empty()
                            || label.chars().count() > 160
                            || label.chars().any(char::is_control)
                    })
                    || !optional_path(&op.source)
                    || !optional_path(&op.destination)
                {
                    return Err(Invalid);
                }
            }
            let launch = &target.launch;
            if !path(&launch.path)
                || !optional_path(&launch.working_directory)
                || launch.arguments.len() > 64
                || launch.arguments.iter().map(String::len).sum::<usize>() > 16384
                || launch
                    .arguments
                    .iter()
                    .any(|arg| arg.len() > 4096 || arg.chars().any(char::is_control))
            {
                return Err(Invalid);
            }
        }
        if used.len() != self.dependencies.len() {
            return Err(Invalid);
        }
        Ok(())
    }
}

fn optional_path(value: &str) -> bool {
    value.is_empty() || path(value)
}

fn unique_map<'de, D, V>(deserializer: D) -> Result<BTreeMap<String, V>, D::Error>
where
    D: serde::Deserializer<'de>,
    V: Deserialize<'de>,
{
    struct Unique<V>(std::marker::PhantomData<V>);

    impl<'de, V: Deserialize<'de>> serde::de::Visitor<'de> for Unique<V> {
        type Value = BTreeMap<String, V>;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a map with unique keys")
        }

        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut entries: A,
        ) -> Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = entries.next_entry()? {
                if result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate map key"));
                }
            }
            Ok(result)
        }
    }

    deserializer.deserialize_map(Unique(std::marker::PhantomData))
}
