use schemars::JsonSchema;
use serde::Deserialize;

#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Shell {
    /// Report prompts and working directories and use xterm-256color for SSH.
    #[schemars(with = "bool", default = "enabled")]
    integration: Option<bool>,
}

const fn enabled() -> bool {
    true
}

impl Shell {
    #[must_use]
    pub fn integration(&self) -> bool {
        self.integration.unwrap_or_else(enabled)
    }

    pub(super) fn overlay(&mut self, file: &Self) {
        self.integration = file.integration.or(self.integration);
    }
}
