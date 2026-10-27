//! Source-session fork records name the successor of an already-bound conversation.

use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopilotFork {
    pub source: String,
    pub target: String,
}

pub fn classify(record: &Value, session_id: &str) -> Option<CopilotFork> {
    if record.get("type")?.as_str()? != "session.info" {
        return None;
    }
    let data = record.get("data")?;
    if data.get("infoType")?.as_str()? != "fork" {
        return None;
    }
    let message = data
        .get("message")?
        .as_str()?
        .strip_prefix("Forked this session into ")?
        .strip_suffix('.')?;
    let source = uuid::Uuid::parse_str(session_id).ok()?;
    let reference = message
        .rsplit_once(" before event ")
        .filter(|(_, event)| uuid::Uuid::parse_str(event).is_ok())
        .map_or(message, |(reference, _)| reference);
    let target = reference
        .strip_suffix(')')
        .and_then(|reference| reference.rsplit_once(" ("))
        .map_or(reference, |(_, id)| id);
    let target = uuid::Uuid::parse_str(target).ok()?;
    if source.is_nil() || target.is_nil() || source == target {
        return None;
    }
    Some(CopilotFork {
        source: source.to_string(),
        target: target.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "11111111-1111-4111-8111-111111111111";
    const TARGET: &str = "22222222-2222-4222-8222-222222222222";

    fn record(message: String) -> Value {
        serde_json::json!({
            "type": "session.info",
            "data": { "infoType": "fork", "message": message }
        })
    }

    #[test]
    fn source_fork_record_identifies_the_successor_without_a_prompt() {
        let expected = Some(CopilotFork {
            source: SOURCE.into(),
            target: TARGET.into(),
        });
        assert_eq!(
            classify(
                &record(format!(
                    "Forked this session into \"Example (fork)\" ({TARGET})."
                )),
                SOURCE,
            ),
            expected
        );
        assert_eq!(
            classify(
                &record(format!("Forked this session into {TARGET}.")),
                SOURCE
            ),
            expected
        );
        assert_eq!(
            classify(
                &record(format!(
                    "Forked this session into \"Copy\" ({TARGET}) before event {SOURCE}."
                )),
                SOURCE
            ),
            expected
        );
    }

    #[test]
    fn malformed_unrelated_and_self_forks_are_not_identity_evidence() {
        for message in [
            format!("Forked this session into \"Example\" ({SOURCE})."),
            "Forked this session into \"Example\" (invalid).".into(),
            format!("Some unrelated message ({TARGET})."),
            format!("Forked from \"Example\" ({SOURCE}) as \"Copy\" ({TARGET})."),
        ] {
            assert!(classify(&record(message), SOURCE).is_none());
        }
        let mut unrelated = record(format!("Forked this session into \"Example\" ({TARGET})."));
        unrelated["data"]["infoType"] = serde_json::json!("other");
        assert!(classify(&unrelated, SOURCE).is_none());
        assert!(
            classify(
                &record(format!(
                    "Forked from \"Example\" ({SOURCE}) as \"Copy\" ({TARGET})."
                )),
                TARGET
            )
            .is_none(),
            "the copied target log cannot claim its source's pane"
        );
    }
}
