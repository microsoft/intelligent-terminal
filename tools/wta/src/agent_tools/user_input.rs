use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

pub const MAX_QUESTION_CHARS: usize = 2_000;
pub const MAX_CHOICES: usize = 8;
pub const MAX_CHOICE_CHARS: usize = 200;
pub const MAX_ANSWER_CHARS: usize = 4_000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UserInputRequest {
    pub question: String,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub allow_freeform: bool,
}

impl UserInputRequest {
    pub fn validate(self) -> Result<Self> {
        if self.question.trim().is_empty() {
            bail!("question must not be empty");
        }
        if self.question.chars().count() > MAX_QUESTION_CHARS {
            bail!("question exceeds the character limit");
        }
        if self
            .question
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
        {
            bail!("question contains unsupported control characters");
        }
        if self.choices.len() > MAX_CHOICES {
            bail!("too many choices");
        }
        if self
            .choices
            .iter()
            .any(|choice| {
                choice.trim().is_empty()
                    || choice.chars().count() > MAX_CHOICE_CHARS
                    || choice.chars().any(char::is_control)
            })
        {
            bail!("choices must be non-empty and within the character limit");
        }
        if self.choices.is_empty() && !self.allow_freeform {
            bail!("at least one choice or freeform input is required");
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum UserInputResponse {
    Answered {
        answer: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        selected_index: Option<usize>,
    },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_choices_or_freeform() {
        assert!(UserInputRequest {
            question: "Choose".into(),
            choices: vec!["A".into(), "B".into()],
            allow_freeform: false,
        }
        .validate()
        .is_ok());
        assert!(UserInputRequest {
            question: "Explain".into(),
            choices: vec![],
            allow_freeform: true,
        }
        .validate()
        .is_ok());
        assert!(UserInputRequest {
            question: "Blocked".into(),
            choices: vec![],
            allow_freeform: false,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn rejects_oversized_and_empty_values() {
        assert!(UserInputRequest {
            question: "q".repeat(MAX_QUESTION_CHARS + 1),
            choices: vec!["A".into()],
            allow_freeform: false,
        }
        .validate()
        .is_err());
        assert!(UserInputRequest {
            question: "Choose".into(),
            choices: vec!["bad\nchoice".into()],
            allow_freeform: false,
        }
        .validate()
        .is_err());
    }
}
