//! WTA's ACP extension request for an autofix turn.

use acp::schema::v1::{PromptRequest, PromptResponse};
use agent_client_protocol as acp;
use serde::{Deserialize, Serialize};

/// Helper-to-proxy request marking a fully resolved prompt as autofix traffic.
///
/// The payload intentionally has the same wire shape as `session/prompt`; the
/// autofix proxy changes only the method and forwards the inner prompt unchanged.
#[derive(Debug, Clone, Serialize, Deserialize, acp::JsonRpcRequest)]
#[request(method = "_intellterm.wta/autofix", response = PromptResponse)]
#[serde(transparent)]
pub struct AutofixPromptRequest(PromptRequest);

impl AutofixPromptRequest {
    pub fn new(prompt: PromptRequest) -> Self {
        Self(prompt)
    }

    pub fn prompt(&self) -> &PromptRequest {
        &self.0
    }

    pub fn into_prompt(self) -> PromptRequest {
        self.0
    }
}
