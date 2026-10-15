//! Private, transient progress for standard ACP authentication.

use std::sync::Arc;

use acp::schema::v1::{AuthMethodId, AuthenticateRequest, ExtNotification};
use agent_client_protocol as acp;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub(crate) struct AcpAuthenticationAttempt {
    pub attempt_id: uuid::Uuid,
    pub method_id: AuthMethodId,
    pub cancelled: CancellationToken,
}

pub(crate) const AUTH_BROWSER_NOTIFICATION: &str = "_intellterm.wta/auth_browser";
pub(crate) const AUTH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

pub(crate) fn authentication_result_for_helper<T>(
    result: acp::Result<T>,
    explicit_attempt: bool,
) -> acp::Result<T> {
    if explicit_attempt {
        // Strip provider data before the SDK can log the outgoing error response.
        result.map_err(|error| acp::Error::new(error.code.into(), safe_auth_error_message(&error)))
    } else {
        result
    }
}

pub(crate) fn safe_auth_error_message(error: &acp::Error) -> String {
    // Never format Error/its entire data object: providers may attach credentials.
    let message = safe_auth_reason(&error.message);
    let detail = error.data.as_ref().and_then(|data| {
        data.as_str()
            .or_else(|| {
                ["reason", "error_description", "message"]
                    .into_iter()
                    .find_map(|key| data.get(key).and_then(serde_json::Value::as_str))
            })
            .and_then(safe_auth_reason)
    });
    match (message, detail) {
        (Some(message), Some(detail)) if message != detail => format!("{message}: {detail}"),
        (Some(message), _) => message,
        (None, Some(detail)) => detail,
        (None, None) => t!("system.authentication_failed").to_string(),
    }
}

fn safe_auth_reason(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    let sensitive_start = [
        "http://",
        "https://",
        "://",
        "%",
        "access_token",
        "refresh_token",
        "id_token",
        "client_secret",
        "token",
        "secret",
        "password",
        "credential",
        "authorization:",
        "authorization=",
        "authorization code",
        "auth code",
        "verification code",
        "device_code",
        "user_code",
        "code=",
        "code:",
        "\"code\"",
        "'code'",
        "code ",
        "state",
        "bearer",
        "ya29.",
        "eyj",
    ]
    .iter()
    .filter_map(|needle| lower.find(needle))
    .chain(
        text.char_indices()
            .find(|(_, ch)| {
                ch.is_control() || matches!(ch, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
            })
            .map(|(index, _)| index),
    )
    .min();
    // Keep only the reason prefix. After a credential label or URL, even
    // whitespace-separated/quoted values and subsequent lines are untrusted.
    let prefix = text[..sensitive_start.unwrap_or(text.len())].trim();
    if prefix.is_empty() {
        return None;
    }
    let mut reason = prefix.chars().take(512).collect::<String>();
    if sensitive_start.is_some() {
        reason.push_str(" [redacted]");
    }
    Some(reason)
}

pub(crate) fn take_auth_attempt_id(
    request: &mut AuthenticateRequest,
) -> acp::Result<Option<uuid::Uuid>> {
    let Some(wta) = request
        .meta
        .as_mut()
        .and_then(|meta| meta.get_mut("wta"))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(None);
    };
    let Some(value) = wta.remove("auth_attempt_id") else {
        return Ok(None);
    };
    value
        .as_str()
        .and_then(|value| uuid::Uuid::parse_str(value).ok())
        .map(Some)
        .ok_or_else(|| acp::Error::invalid_params().data("invalid authentication attempt id"))
}

pub(crate) fn browser_notification(
    attempt_id: uuid::Uuid,
    url: &str,
) -> acp::Result<ExtNotification> {
    if !valid_browser_url(url) {
        return Err(acp::Error::invalid_params().data("invalid authentication browser URL"));
    }
    let params = serde_json::json!({
        "auth_attempt_id": attempt_id.to_string(),
        "url": url,
    });
    let raw = serde_json::value::RawValue::from_string(params.to_string())
        .map_err(|_| acp::Error::internal_error())?;
    Ok(ExtNotification::new(
        AUTH_BROWSER_NOTIFICATION,
        Arc::from(raw),
    ))
}

pub(crate) fn parse_browser_notification(
    args: &ExtNotification,
) -> Option<Result<(uuid::Uuid, String), &'static str>> {
    if !crate::session_registry::ext_method_matches(&args.method, AUTH_BROWSER_NOTIFICATION) {
        return None;
    }
    Some((|| {
        let params: serde_json::Value = serde_json::from_str(args.params.get())
            .map_err(|_| "invalid authentication browser notification")?;
        let attempt_id = params
            .get("auth_attempt_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|id| uuid::Uuid::parse_str(id).ok())
            .ok_or("invalid authentication attempt id")?;
        let url = params
            .get("url")
            .and_then(serde_json::Value::as_str)
            .filter(|url| valid_browser_url(url))
            .ok_or("invalid authentication browser URL")?;
        Ok((attempt_id, url.to_string()))
    })())
}

pub(crate) fn browser_url_from_stderr(line: &str) -> Option<String> {
    line.split_whitespace()
        .map(|word| word.trim_matches(['"', '\'', '<', '>']))
        .find(|url| valid_browser_url(url))
        .map(str::to_string)
}

// A deliberately narrow parser: only the two observed Google endpoints and
// loopback HTTP callbacks are accepted, without URL normalization surprises.
pub(crate) fn valid_browser_url(url: &str) -> bool {
    if !url.is_ascii()
        || url.bytes().any(|byte| byte <= b' ' || byte == 127)
        || url.contains(['\\', '#', '"', '\'', '<', '>'])
    {
        return false;
    }
    let Some(rest) = url.strip_prefix("https://accounts.google.com/") else {
        return false;
    };
    let Some((path, query)) = rest.split_once('?') else {
        return false;
    };
    if !matches!(path, "o/oauth2/v2/auth" | "o/oauth2/auth") {
        return false;
    }
    let mut redirect_seen = false;
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            return false;
        };
        let (Some(key), Some(value)) = (decode_query_component(key), decode_query_component(value))
        else {
            return false;
        };
        if key == "redirect_uri" {
            if redirect_seen || !valid_loopback_redirect(&value) {
                return false;
            }
            redirect_seen = true;
        }
    }
    redirect_seen
}

fn decode_query_component(value: &str) -> Option<String> {
    let mut bytes = value.bytes();
    let mut decoded = Vec::with_capacity(value.len());
    while let Some(byte) = bytes.next() {
        let byte = match byte {
            b'%' => {
                let high = char::from(bytes.next()?).to_digit(16)?;
                let low = char::from(bytes.next()?).to_digit(16)?;
                (high * 16 + low) as u8
            }
            b'+' => b' ',
            byte => byte,
        };
        if byte < b' ' || byte == 127 {
            return None;
        }
        decoded.push(byte);
    }
    let decoded = String::from_utf8(decoded).ok()?;
    (!decoded.chars().any(char::is_control)).then_some(decoded)
}

fn valid_loopback_redirect(url: &str) -> bool {
    if !url.is_ascii()
        || url.bytes().any(|byte| byte <= b' ' || byte == 127)
        || url.contains(['\\', '#', '%', '@', '"', '\'', '<', '>'])
    {
        return false;
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    matches!(host, "127.0.0.1" | "localhost")
        && port.is_none_or(|port| {
            !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && port.parse::<u16>().is_ok_and(|port| port != 0)
        })
}

pub(crate) fn redact_auth_stderr(line: &str) -> &str {
    let lower = line.to_ascii_lowercase();
    if [
        "http://",
        "https://",
        "oauth",
        "authorization",
        "token",
        "code=",
        "code:",
        "client_secret",
        "redirect_uri",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        "[redacted agent authentication progress]"
    } else {
        line
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://accounts.google.com/o/oauth2/v2/auth?client_id=fixture&redirect_uri=http%3A%2F%2F127.0.0.1%3A8181%2Fcallback&state=fixture";

    #[test]
    fn auth_bridge_serialized_error_keeps_code_and_safe_reason_without_secret_data() {
        let error = acp::Error::new(-32042, format!("Consent denied; open {URL}")).data(
            serde_json::json!({
                "reason": "Account policy blocks sign-in; code=private-code-marker",
                "url": URL,
                "access_token": "private-token-marker",
                "nested": {"refresh_token": "private-refresh-marker"},
            }),
        );
        let original_code = error.code;
        let sanitized = authentication_result_for_helper::<()>(Err(error), true).unwrap_err();
        assert_eq!(sanitized.code, original_code);
        assert!(sanitized.data.is_none());
        let serialized = serde_json::to_string(&sanitized).unwrap();
        let value: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(value["code"], -32042);
        for secret in [
            "accounts.google.com",
            "private-code-marker",
            "private-token-marker",
            "private-refresh-marker",
        ] {
            assert!(!serialized.contains(secret));
        }
        assert!(sanitized.message.contains("Consent denied"));
        assert!(sanitized.message.contains("Account policy blocks sign-in"));
    }

    #[test]
    fn auth_bridge_error_boundary_preserves_success_and_legacy_results() {
        let success = acp::schema::v1::AuthenticateResponse::new();
        let expected = serde_json::to_value(&success).unwrap();
        let forwarded = authentication_result_for_helper(Ok(success), true).unwrap();
        assert_eq!(serde_json::to_value(&forwarded).unwrap(), expected);

        let error = acp::Error::new(-32042, "Legacy authentication reason")
            .data(serde_json::json!({"provider_detail": "retained"}));
        let expected = serde_json::to_value(&error).unwrap();
        let forwarded = authentication_result_for_helper::<()>(Err(error), false).unwrap_err();
        assert_eq!(serde_json::to_value(&forwarded).unwrap(), expected);
    }

    #[test]
    fn auth_bridge_error_presentation_redacts_secrets_but_retains_reason() {
        let error = acp::Error::new(-32603, format!("Consent denied by provider; open {URL}"))
            .data(serde_json::json!({
                "reason": "Account policy blocks sign-in; access_token=private-marker",
                "refresh_token": "must-not-display",
            }));
        let message = safe_auth_error_message(&error);
        assert!(message.contains("Consent denied by provider"));
        assert!(message.contains("Account policy blocks sign-in"));
        assert!(!message.contains("accounts.google.com"));
        assert!(!message.contains("private-marker"));
        assert!(!message.contains("must-not-display"));
        assert!(!message.contains('\n'));
    }

    #[test]
    fn auth_bridge_error_presentation_handles_multiline_quoted_and_encoded_secrets() {
        for secret in [
            "code=private-marker",
            "\"code\": \"private-marker\"",
            "authorization code private-marker",
            "Bearer private-marker",
            "refresh_token private-marker",
            "password private-marker",
            "https%3A%2F%2Faccounts.google.com%2Foauth%3Fcode%3Dprivate-marker",
        ] {
            let error = acp::Error::new(
                -32603,
                format!("Provider rejected login.\n{secret}\nUntrusted trailing detail"),
            );
            let message = safe_auth_error_message(&error);
            assert!(message.contains("Provider rejected login."));
            assert!(!message.contains("private-marker"));
            assert!(!message.contains("Untrusted trailing detail"));
            assert!(!message.chars().any(char::is_control));
        }
        let error = acp::Error::new(-32603, "Provider unavailable; try again later");
        assert_eq!(safe_auth_error_message(&error), error.message);
    }

    #[test]
    fn auth_bridge_browser_notification_survives_actual_sdk_wire_dispatch() {
        use acp::JsonRpcMessage;

        let id = uuid::Uuid::new_v4();
        let outbound = browser_notification(id, URL).unwrap();
        let params: serde_json::Value = serde_json::from_str(outbound.params.get()).unwrap();
        let inbound = acp::schema::v1::AgentNotification::parse_message(&outbound.method, &params)
            .expect(
                "The actual SDK dispatcher must accept the private authentication notification.",
            );
        let acp::schema::v1::AgentNotification::ExtNotification(inbound) = inbound else {
            panic!("Browser progress must arrive as an extension notification.");
        };
        assert_eq!(
            parse_browser_notification(&inbound),
            Some(Ok((id, URL.to_string())))
        );
    }

    #[test]
    fn auth_bridge_browser_notification_validates_identity_and_url() {
        let id = uuid::Uuid::new_v4();
        let notification = browser_notification(id, URL).unwrap();
        assert_eq!(
            parse_browser_notification(&notification),
            Some(Ok((id, URL.to_string())))
        );
        let wrong = ExtNotification::new("unrelated", notification.params.clone());
        assert!(parse_browser_notification(&wrong).is_none());
        let bad = ExtNotification::new(
            AUTH_BROWSER_NOTIFICATION,
            Arc::from(
                serde_json::value::RawValue::from_string(
                    serde_json::json!({"auth_attempt_id": "not-a-uuid", "url": URL}).to_string(),
                )
                .unwrap(),
            ),
        );
        assert!(parse_browser_notification(&bad).unwrap().is_err());
    }

    #[test]
    fn auth_bridge_rejects_spoofed_authorities_and_untrusted_redirects() {
        assert!(valid_browser_url(URL));
        assert!(valid_browser_url(&URL.replace("/v2/auth", "/auth")));
        assert!(valid_browser_url(&URL.replace("127.0.0.1", "localhost")));
        for url in [
            URL.replace("accounts.google.com", "accounts.google.com.evil.test"),
            URL.replace("accounts.google.com", "accounts.google.com@evil.test"),
            URL.replace("https://", "http://"),
            URL.replace("/v2/auth", "/v2/auth/"),
            URL.replace("127.0.0.1", "evil.test"),
            URL.replace("http%3A", "javascript%3A"),
            URL.replace("http%3A", "https%3A"),
            URL.replace("state=fixture", "state=%0a"),
            URL.replace("state=fixture", "state=%ZZ"),
            format!("{URL}&redirect_uri=http%3A%2F%2Flocalhost"),
            format!("{URL}\r"),
        ] {
            assert!(!valid_browser_url(&url));
        }
        assert!(browser_url_from_stderr("https://evil.test/oauth?code=secret").is_none());
        assert_eq!(
            browser_url_from_stderr(&format!("Open {URL}")),
            Some(URL.to_string())
        );
    }

    #[test]
    fn auth_bridge_strips_only_internal_attempt_metadata() {
        let id = uuid::Uuid::new_v4();
        let mut request = AuthenticateRequest::new(AuthMethodId::new("google"));
        request.meta = Some(
            serde_json::json!({
                "provider": {"untouched": true},
                "wta": {"auth_attempt_id": id.to_string(), "other": 42}
            })
            .as_object()
            .unwrap()
            .clone(),
        );
        assert_eq!(take_auth_attempt_id(&mut request).unwrap(), Some(id));
        assert_eq!(
            request.meta.unwrap(),
            serde_json::json!({
                "provider": {"untouched": true}, "wta": {"other": 42}
            })
            .as_object()
            .unwrap()
            .clone()
        );
        let mut legacy = AuthenticateRequest::new(AuthMethodId::new("legacy"));
        assert_eq!(take_auth_attempt_id(&mut legacy).unwrap(), None);
    }
}
