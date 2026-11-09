/// `[SettingGroups]`: a named multi-choice whose options become `#if` flags.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingGroup {
    /// Group id used in the project snapshot.
    pub name: String,
    /// Display name.
    pub label: String,
    /// Options in declaration order.
    pub options: Vec<SettingOption>,
}

/// One option of a [`SettingGroup`].
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SettingOption {
    /// Flag name the `#if` tests.
    pub name: String,
    /// Display name.
    pub label: String,
}
