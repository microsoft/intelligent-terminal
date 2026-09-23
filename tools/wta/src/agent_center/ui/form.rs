// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::{editor::DraftEditor, Operation};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};

pub(super) struct Field {
    pub path: Option<String>,
    pub label: String,
    pub schema: Value,
    pub required: bool,
    pub draft: String,
    pub editor: DraftEditor,
    pub choices: Vec<(String, Value)>,
    pub choice: Option<usize>,
}

pub(super) struct Form {
    pub question: Value,
    pub work: Option<String>,
    pub fields: Vec<Field>,
    pub selected: usize,
    pub object: bool,
    pub scroll: u16,
    pub follow: bool,
}

impl Form {
    pub fn new(question: Value, work: Option<String>) -> Result<Self> {
        let schema = &question["responseSchema"];
        let object = schema["type"] == "object";
        let mut fields = Vec::new();
        check_keywords(schema)?;
        if object {
            ensure!(
                schema["additionalProperties"] == false && schema.get("enum").is_none(),
                "{}",
                t!("agent_center.console_schema_unsupported")
            );
            let properties = schema["properties"]
                .as_object()
                .with_context(|| t!("agent_center.console_schema_unsupported").into_owned())?;
            for (name, property) in properties {
                let required = schema["required"]
                    .as_array()
                    .is_some_and(|keys| keys.contains(&json!(name)));
                fields.push(Field::new(
                    Some(name.clone()),
                    property.clone(),
                    required,
                    &question,
                )?);
            }
        } else {
            fields.push(Field::new(None, schema.clone(), true, &question)?);
        }
        ensure!(
            !fields.is_empty(),
            "{}",
            t!("agent_center.console_schema_unsupported")
        );
        Ok(Self {
            question,
            work,
            fields,
            selected: 0,
            object,
            scroll: 0,
            follow: true,
        })
    }

    pub fn operation(&self) -> Result<Operation> {
        let mut value = if self.object { json!({}) } else { Value::Null };
        for field in &self.fields {
            if let Some(answer) = field.answer()? {
                if let Some(path) = &field.path {
                    value[path] = answer;
                } else {
                    value = answer;
                }
            }
        }
        let decision = self.question["kind"] == "DecisionRequest";
        let id = self.question["id"]
            .as_str()
            .with_context(|| t!("agent_center.invalid_response").into_owned())?;
        let version = self.question["version"]
            .as_u64()
            .filter(|v| *v > 0)
            .with_context(|| t!("agent_center.invalid_response").into_owned())?;
        Ok(Operation {
            method: if decision {
                "decision.answer"
            } else {
                "conversation.answer_input"
            }
            .into(),
            params: if decision {
                json!({"decisionId":id,"value":value})
            } else {
                json!({"requestId":id,"action":"Answer","value":value})
            },
            if_match: vec![
                json!({"kind":if decision {"DecisionRequest"} else {"IntakeRequest"},"id":id,"version":version}),
            ],
            mutation: true,
            confirmation: true,
            command_id: uuid::Uuid::new_v4().to_string(),
        })
    }

    pub fn layout(&self, target: &str, width: u16) -> (super::editor::DraftLayout, usize) {
        let (body, focus) = self.content(target);
        let field = &self.fields[self.selected];
        let mut editor = DraftEditor::default();
        editor.width = usize::from(width.max(1));
        editor.cursor = focus;
        if field.choices.is_empty() {
            editor.anchor = field
                .editor
                .anchor
                .map(|anchor| focus.saturating_sub(field.editor.cursor) + anchor);
        }
        (editor.layout(&body), focus)
    }

    pub fn viewport(&mut self, target: &str, width: u16, height: u16) -> u16 {
        let (layout, focus) = self.layout(target, width);
        let height = usize::from(height.max(1));
        let maximum = layout
            .lines
            .len()
            .saturating_sub(height)
            .min(u16::MAX as usize) as u16;
        if self.follow {
            let row = layout.cursor(focus).0;
            self.scroll = row.saturating_sub(height - 1).min(u16::MAX as usize) as u16;
        }
        self.scroll = self.scroll.min(maximum);
        self.scroll
    }

    fn content(&self, target: &str) -> (String, usize) {
        let mut body = format!(
            "{}\n\n{}\n\n{}",
            target,
            super::projection::text(&self.question, "question"),
            t!("agent_center.console_form_help")
        );
        let mut focus = 0;
        for (index, field) in self.fields.iter().enumerate() {
            let answer = field
                .choice
                .and_then(|choice| field.choices.get(choice))
                .map(|(label, _)| label.as_str())
                .unwrap_or(&field.draft);
            body.push_str("\n\n");
            let label = format!(
                "{} {}{}: ",
                if index == self.selected { ">" } else { " " },
                field.label,
                if field.required { " *" } else { "" },
            );
            if index == self.selected {
                focus = body.len()
                    + label.len()
                    + if field.choices.is_empty() {
                        field.editor.cursor.min(answer.len())
                    } else {
                        answer.len()
                    };
            }
            body.push_str(&label);
            body.push_str(answer);
            if index == self.selected {
                body.push_str("\n\n");
                body.push_str(super::projection::text(&field.schema, "description"));
                if !field.choices.is_empty() {
                    body.push_str("\n\n");
                    body.push_str(
                        &field
                            .choices
                            .iter()
                            .map(|(label, _)| label.as_str())
                            .collect::<Vec<_>>()
                            .join(" | "),
                    );
                }
            }
        }
        (body, focus)
    }
}

fn check_keywords(schema: &Value) -> Result<()> {
    let object = schema
        .as_object()
        .with_context(|| t!("agent_center.console_schema_unsupported").into_owned())?;
    ensure!(
        object.keys().all(|key| [
            "type",
            "$schema",
            "title",
            "description",
            "enum",
            "properties",
            "required",
            "additionalProperties",
            "minLength",
            "maxLength",
            "minimum",
            "maximum"
        ]
        .contains(&key.as_str())),
        "{}",
        t!("agent_center.console_schema_unsupported")
    );
    Ok(())
}

impl Field {
    fn new(path: Option<String>, schema: Value, required: bool, question: &Value) -> Result<Self> {
        check_keywords(&schema)?;
        ensure!(
            matches!(
                schema["type"].as_str(),
                Some("string" | "integer" | "boolean")
            ),
            "{}",
            t!("agent_center.console_schema_unsupported")
        );
        let label = schema["title"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| path.clone())
            .unwrap_or_else(|| t!("agent_center.console_answer").into_owned());
        let values = if let Some(values) = schema.get("enum") {
            values
                .as_array()
                .filter(|values| !values.is_empty())
                .with_context(|| t!("agent_center.console_schema_unsupported").into_owned())?
                .clone()
        } else if schema["type"] == "boolean" {
            vec![json!(true), json!(false)]
        } else {
            vec![]
        };
        let mut choices = Vec::new();
        for value in values {
            ensure!(
                match schema["type"].as_str() {
                    Some("string") => value.is_string(),
                    Some("integer") => value.is_i64(),
                    Some("boolean") => value.is_boolean(),
                    _ => false,
                },
                "{}",
                t!("agent_center.console_schema_unsupported")
            );
            let option = question["options"]
                .as_array()
                .and_then(|options| options.iter().find(|option| option["id"] == value));
            let label = if let Some(option) = option {
                format!(
                    "{} — {}",
                    super::projection::text(option, "label"),
                    super::projection::text(option, "impact")
                )
            } else if let Some(boolean) = value.as_bool() {
                if boolean {
                    t!("agent_center.console_yes")
                } else {
                    t!("agent_center.console_no")
                }
                .into_owned()
            } else {
                super::projection::scalar_fields(&value)
            };
            choices.push((label, value));
        }

        Ok(Self {
            path,
            label,
            schema,
            required,
            draft: String::new(),
            editor: DraftEditor::default(),
            choices,
            choice: None,
        })
    }

    fn answer(&self) -> Result<Option<Value>> {
        let value = if !self.choices.is_empty() {
            self.choice
                .and_then(|index| self.choices.get(index))
                .map(|(_, value)| value.clone())
        } else if self.draft.is_empty() && !self.required {
            None
        } else if self.schema["type"] == "integer" {
            Some(json!(self.draft.parse::<i64>().with_context(|| t!(
                "agent_center.console_answer_invalid",
                field = &self.label
            )
            .into_owned())?))
        } else {
            Some(json!(self.draft))
        };
        let invalid =
            || t!("agent_center.console_answer_invalid", field = &self.label).into_owned();
        let Some(value) = value else {
            ensure!(!self.required, "{}", invalid());
            return Ok(None);
        };
        if let Some(text) = value.as_str() {
            let length = text.chars().count() as u64;
            if self.schema["minLength"]
                .as_u64()
                .is_some_and(|min| length < min)
                || self.schema["maxLength"]
                    .as_u64()
                    .is_some_and(|max| length > max)
            {
                bail!("{}", invalid());
            }
        }
        if let Some(number) = value.as_i64() {
            ensure!(
                !self.schema["minimum"]
                    .as_i64()
                    .is_some_and(|min| number < min)
                    && !self.schema["maximum"]
                        .as_i64()
                        .is_some_and(|max| number > max),
                "{}",
                invalid()
            );
        }
        Ok(Some(value))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structured_intake_uses_fields_and_exact_guard_without_json_or_implicit_choices() {
        let _locale = crate::test_support::lock_locale();
        let mut form = Form::new(json!({"kind":"IntakeRequest","id":"intake","version":4,
            "question":"Choose report format","responseSchema":{"type":"object","additionalProperties":false,
            "required":["format","limit"],"properties":{
                "format":{"type":"string","enum":["brief","detailed"]},
                "limit":{"type":"integer","minimum":1,"maximum":10},
                "note":{"type":"string"}
            }}}), None).unwrap();
        assert!(
            form.operation().is_err(),
            "required choice must not default"
        );
        form.fields
            .iter_mut()
            .find(|field| field.path.as_deref() == Some("format"))
            .unwrap()
            .choice = Some(1);
        form.fields
            .iter_mut()
            .find(|field| field.path.as_deref() == Some("limit"))
            .unwrap()
            .draft = "5".into();
        let operation = form.operation().unwrap();
        assert_eq!(
            operation.params,
            json!({"requestId":"intake","action":"Answer","value":{"format":"detailed","limit":5}})
        );
        assert_eq!(
            operation.if_match,
            [json!({"kind":"IntakeRequest","id":"intake","version":4})]
        );
        assert!(operation.confirmation);
    }

    #[test]
    fn unsupported_schema_is_explicit_and_never_silently_flattened() {
        let _locale = crate::test_support::lock_locale();
        for schema in [
            json!({"type":"array","items":{"type":"string"}}),
            json!({"type":"object","properties":{"nested":{"type":"object","properties":{},"additionalProperties":false}},"additionalProperties":false}),
            json!({"type":"string","pattern":"secret"}),
            json!({"type":"string","enum":[{"complex":true}]}),
        ] {
            assert!(Form::new(json!({"responseSchema":schema}), None).is_err());
        }
    }

    #[test]
    fn field_bounds_and_human_option_impacts_are_preserved() {
        let _locale = crate::test_support::lock_locale();
        let form = Form::new(
            json!({"kind":"DecisionRequest","id":"decision","version":2,"question":"Output detail?",
            "options":[{"id":"brief","label":"Concise report","impact":"Omit raw logs"}],
            "responseSchema":{"type":"string","enum":["brief"]}}),
            Some("a".into()),
        )
        .unwrap();
        let (layout, _) = form.layout("Fix report · Reports", 80);
        let rendered = layout
            .lines
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(rendered.contains("Concise report — Omit raw logs"));
        assert!(form.operation().is_err());
        let mut numeric = Form::new(
            json!({"kind":"IntakeRequest","id":"intake","version":1,
            "responseSchema":{"type":"integer","minimum":1,"maximum":2}}),
            None,
        )
        .unwrap();
        numeric.fields[0].draft = "3".into();
        assert!(numeric.operation().is_err());
    }
}
