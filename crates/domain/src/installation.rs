use crate::recipe::{path, platform, Collision};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("{}", self.title())]
pub enum InstallationError {
    Invalid,
    Unsupported,
    Missing,
    Source,
    Selection,
    Collision,
    Platform,
    Launch,
    Limit,
}
impl InstallationError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Invalid => "installation_invalid",
            Self::Unsupported => "installation_unsupported",
            Self::Missing => "installation_missing",
            Self::Source => "installation_source",
            Self::Selection => "installation_selection",
            Self::Collision => "installation_collision",
            Self::Platform => "installation_platform",
            Self::Launch => "installation_launch",
            Self::Limit => "installation_limit",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Invalid => "Invalid installation manifest",
            Self::Unsupported => "Unsupported installation manifest version",
            Self::Missing => "Installation manifest is absent",
            Self::Source => "Release association or inventory path unavailable",
            Self::Selection => "Choose valid installation options",
            Self::Collision => "Installation destination collision",
            Self::Platform => "Installation platform unavailable",
            Self::Launch => "Launch declaration or installed target unavailable",
            Self::Limit => "Installation resolution limit exceeded",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub version: u32,
    #[serde(default)]
    pub recipe_payload: bool,
    #[serde(default)]
    pub payload: Vec<Copy>,
    #[serde(default)]
    pub options: Vec<OptionGroup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub localization: Option<Localization>,
    #[serde(default)]
    pub prerequisites: Vec<Prerequisite>,
    #[serde(default)]
    pub unresolved: Vec<String>,
    #[serde(default)]
    pub launch: Option<Launch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase", deny_unknown_fields)]
pub enum Localization {
    Unknown,
    Fixed { language: String },
    Bundled { languages: Vec<String> },
    Selectable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Copy {
    pub association_id: String,
    #[serde(default)]
    pub source: String,
    pub destination: String,
    #[serde(default)]
    pub collision: Collision,
    #[serde(default)]
    pub platforms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OptionGroup {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub language: bool,
    #[serde(default)]
    pub multiple: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub defaults: Vec<String>,
    pub choices: Vec<Choice>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Choice {
    pub id: String,
    pub name: String,
    pub payload: Vec<Copy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Condition {
    pub option_id: String,
    pub choice_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Prerequisite {
    pub id: String,
    pub name: String,
    pub association_id: String,
    #[serde(default)]
    pub path: String,
    pub platforms: Vec<String>,
    #[serde(default)]
    pub arguments: Vec<String>,
    #[serde(default)]
    pub optional: bool,
    pub evidence: String,
    #[serde(default)]
    pub when: Option<Condition>,
    #[serde(default = "success")]
    pub success_exit_codes: Vec<i32>,
}

fn success() -> Vec<i32> {
    vec![0]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Launch {
    pub association_id: String,
    pub entry_point_id: String,
    pub revision_id: String,
    pub destination: String,
    #[serde(default)]
    pub working_directory: Option<String>,
}

pub fn canonical_uuid(value: &str) -> bool {
    uuid::Uuid::parse_str(value).is_ok_and(|id| id.to_string() == value)
}

fn identifier(value: &str) -> bool {
    (1..=80).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

pub fn language_id(value: &str) -> bool {
    value.split('-').all(|part| {
        (1..=8).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_alphanumeric())
    }) && value
        .bytes()
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic())
}

pub fn bundled_languages(languages: &[String]) -> bool {
    (2..=32).contains(&languages.len())
        && languages
            .iter()
            .all(|tag| tag.len() <= 80 && language_id(tag))
        && languages
            .iter()
            .map(|tag| tag.to_ascii_lowercase())
            .collect::<BTreeSet<_>>()
            .len()
            == languages.len()
}

fn label(value: &str, limit: usize) -> bool {
    !value.trim().is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}

fn platforms(values: &[String]) -> bool {
    values.len() <= 6 && values.iter().all(|p| platform(p)) && distinct(values)
}

fn distinct<T: Ord>(values: &[T]) -> bool {
    values.iter().collect::<BTreeSet<_>>().len() == values.len()
}

pub fn arguments(values: &[String]) -> bool {
    values.len() <= 64
        && values.iter().map(String::len).sum::<usize>() <= 16384
        && values
            .iter()
            .all(|s| s.len() <= 4096 && !s.chars().any(char::is_control))
}

pub fn relative(value: &str) -> bool {
    value.is_empty() || path(value)
}

impl Manifest {
    pub fn copies(&self) -> impl Iterator<Item = &Copy> {
        self.payload.iter().chain(
            self.options
                .iter()
                .flat_map(|group| group.choices.iter().flat_map(|choice| &choice.payload)),
        )
    }

    pub fn validate(&self) -> Result<(), InstallationError> {
        use InstallationError::{Invalid, Unsupported};
        if self.version != 1 {
            return Err(Unsupported);
        }
        if self.options.len() > 16
            || self.prerequisites.len() > 32
            || self.copies().count() > 128
            || (self.recipe_payload
                && (!self.payload.is_empty()
                    || !self.options.is_empty()
                    || !self.prerequisites.is_empty()
                    || self.launch.is_some()))
            || (!self.recipe_payload && self.payload.is_empty())
            || self.unresolved.len() > 128
            || self.unresolved.iter().any(|s| !label(s, 1024))
        {
            return Err(Invalid);
        }
        if self.copies().any(|op| {
            !canonical_uuid(&op.association_id)
                || !relative(&op.source)
                || !relative(&op.destination)
                || !platforms(&op.platforms)
        }) {
            return Err(Invalid);
        }
        let mut ids = BTreeSet::new();
        let mut has_language = false;
        for group in &self.options {
            if !identifier(&group.id)
                || !ids.insert(&group.id)
                || !label(&group.name, 160)
                || group.choices.is_empty()
                || group.choices.len() > 32
                || (group.language
                    && (group.multiple || std::mem::replace(&mut has_language, true)))
                || group.choices.iter().any(|choice| {
                    !identifier(&choice.id)
                        || !label(&choice.name, 160)
                        || (group.language && !language_id(&choice.id))
                })
            {
                return Err(Invalid);
            }
            let choice_ids: BTreeSet<_> = group
                .choices
                .iter()
                .map(|choice| choice.id.to_ascii_lowercase())
                .collect();
            if choice_ids.len() != group.choices.len() || !selection(group, &group.defaults) {
                return Err(Invalid);
            }
        }
        let language_group = self.options.iter().find(|group| group.language);
        let valid_tag = |tag: &str| tag.len() <= 80 && language_id(tag);
        let valid_localization = match &self.localization {
            None => true,
            Some(Localization::Unknown) => language_group.is_none(),
            Some(Localization::Fixed { language }) => {
                language_group.is_none() && valid_tag(language)
            }
            Some(Localization::Bundled { languages }) => {
                language_group.is_none() && bundled_languages(languages)
            }
            Some(Localization::Selectable) => language_group.is_some_and(|group| {
                group.choices.len() >= 2
                    && group
                        .choices
                        .windows(2)
                        .any(|choices| choices[0].payload != choices[1].payload)
            }),
        };
        if !valid_localization {
            return Err(Invalid);
        }
        ids.clear();
        for item in &self.prerequisites {
            if !identifier(&item.id)
                || !ids.insert(&item.id)
                || !label(&item.name, 160)
                || !label(&item.evidence, 1024)
                || !canonical_uuid(&item.association_id)
                || !relative(&item.path)
                || item.platforms.is_empty()
                || !platforms(&item.platforms)
                || !arguments(&item.arguments)
                || item.success_exit_codes.is_empty()
                || item.success_exit_codes.len() > 16
                || !distinct(&item.success_exit_codes)
                || item.when.as_ref().is_some_and(|condition| {
                    !self.options.iter().any(|group| {
                        group.id == condition.option_id
                            && group
                                .choices
                                .iter()
                                .any(|choice| choice.id == condition.choice_id)
                    })
                })
            {
                return Err(Invalid);
            }
        }
        if self.launch.as_ref().is_some_and(|launch| {
            !canonical_uuid(&launch.association_id)
                || !identifier(&launch.entry_point_id)
                || !canonical_uuid(&launch.revision_id)
                || !path(&launch.destination)
                || launch
                    .working_directory
                    .as_deref()
                    .is_some_and(|value| !relative(value))
        }) {
            return Err(Invalid);
        }
        Ok(())
    }

    pub fn selections(
        &self,
        input: &BTreeMap<String, Vec<String>>,
    ) -> Result<BTreeMap<String, Vec<String>>, InstallationError> {
        if input
            .keys()
            .any(|id| !self.options.iter().any(|group| &group.id == id))
        {
            return Err(InstallationError::Selection);
        }
        self.options
            .iter()
            .map(|group| {
                let selected = input.get(&group.id).unwrap_or(&group.defaults);
                if !selection(group, selected) || (group.required && selected.is_empty()) {
                    return Err(InstallationError::Selection);
                }
                Ok((
                    group.id.clone(),
                    group
                        .choices
                        .iter()
                        .filter(|choice| selected.contains(&choice.id))
                        .map(|choice| choice.id.clone())
                        .collect(),
                ))
            })
            .collect()
    }
}

fn selection(group: &OptionGroup, selected: &[String]) -> bool {
    selected.len() <= group.choices.len()
        && (group.multiple || selected.len() <= 1)
        && distinct(selected)
        && selected
            .iter()
            .all(|id| group.choices.iter().any(|choice| &choice.id == id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localization_distinguishes_payload_choices_from_supported_languages() {
        let copy = Copy {
            association_id: uuid::Uuid::nil().to_string(),
            source: String::new(),
            destination: String::new(),
            collision: Collision::Reject,
            platforms: vec![],
        };
        let mut manifest = Manifest {
            version: 1,
            recipe_payload: false,
            payload: vec![copy.clone()],
            options: vec![],
            localization: None,
            prerequisites: vec![],
            unresolved: vec![],
            launch: None,
        };
        for localization in [
            Localization::Unknown,
            Localization::Fixed {
                language: "ru-RU".into(),
            },
            Localization::Bundled {
                languages: vec!["en-US".into(), "ru-RU".into()],
            },
        ] {
            manifest.localization = Some(localization);
            manifest.validate().unwrap();
            assert!(manifest.selections(&BTreeMap::new()).unwrap().is_empty());
        }
        manifest.localization = Some(Localization::Bundled {
            languages: vec!["en-US".into(), "EN-us".into()],
        });
        assert!(manifest.validate().is_err());
        manifest.localization = Some(Localization::Selectable);
        manifest.options = vec![OptionGroup {
            id: "language".into(),
            name: "Game language".into(),
            language: true,
            multiple: false,
            required: true,
            defaults: vec![],
            choices: vec![
                Choice {
                    id: "en-US".into(),
                    name: "English".into(),
                    payload: vec![],
                },
                Choice {
                    id: "ru-RU".into(),
                    name: "Russian".into(),
                    payload: vec![],
                },
            ],
        }];
        assert!(manifest.validate().is_err());
        manifest.options[0].choices[1].payload.push(copy);
        manifest.validate().unwrap();
        assert!(manifest.selections(&BTreeMap::new()).is_err());
        assert!(manifest
            .selections(&BTreeMap::from([("language".into(), vec!["ru-RU".into()])]))
            .is_ok());
        manifest.localization = Some(Localization::Fixed {
            language: "ru-RU".into(),
        });
        assert!(manifest.validate().is_err());
        manifest.localization = None;
        manifest.validate().unwrap();
    }
}
