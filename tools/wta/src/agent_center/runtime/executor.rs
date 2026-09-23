// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

use super::*;

pub(super) fn input_receipt(input: &Value) -> Result<Value> {
    Ok(
        json!({"digest":artifacts::digest(&serde_json::to_vec(input)?),
        "turnNumber":input["turnNumber"],"replyMessageId":input["replyMessageId"]}),
    )
}

impl Runtime {
    pub(super) async fn execution_check(&self, invocation: &Invocation) -> Result<()> {
        success(
            self.bound(
                invocation,
                "work.execution_check",
                json!({"workId":invocation.input["executorInput"]["workId"]}),
            )
            .await,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn actual_legacy_worker_without_primary_metadata_can_be_verified_but_not_coordinator(
    ) -> Result<()> {
        let root = std::env::current_dir()?
            .join("target")
            .join(format!("executor-provenance-{}", Uuid::new_v4()));
        let work = Uuid::new_v4().to_string();
        let workspace = Uuid::new_v4().to_string();
        let source = Uuid::new_v4().to_string();
        let runtime_id = Uuid::new_v4().to_string();
        let cwd = root.join("workspaces").join(&workspace);
        tokio::fs::create_dir_all(&cwd).await?;
        let cwd = process::launch_directory(&tokio::fs::canonicalize(cwd).await?).await?;
        let handle = super::super::super::transport::start_engine(root.clone()).await?;
        let runtime = Runtime::new(handle.clone(), root.clone())?;
        let adapter =
            json!({"kind":"ACP","executable":"fixture","approvedModelDestination":"Local test"});
        let original = json!({"id":source,"subject":{"kind":"Task","id":Uuid::new_v4().to_string()},
                "runtimeId":runtime_id,"bindingGeneration":1,"capabilityId":"provider",
                "dispatch":{"kind":"ProduceResult","workId":work,"workspaceId":workspace}});
        let mut wire = original.clone();
        wire["adapter"] = adapter.clone();
        let state = InvocationState {
            state: "Ended".into(),
            released: true,
            settled: true,
            acknowledged: true,
            execution_identity: "acp:fixture-owned-job".into(),
            settlement_proof: Some("acp:fixture-owned-job".into()),
            ..Default::default()
        };
        let ledger = json!({"inputDigest":artifacts::digest(&serde_json::to_vec(&wire)?),
                "binding":{"invocationId":source,"runtimeId":runtime_id,"bindingGeneration":1},"state":state});
        tokio::fs::write(runtime.ledger(&source)?, serde_json::to_vec(&ledger)?).await?;
        let input = json!({"adapter":adapter,"capabilityId":"provider","executorInput":{"workId":work,"workspaceId":workspace,
                "sessionSource":{"invocationId":source,"legacyWorker":true,"originalInvocation":original,
                "providerSessionId":"actual-worker-session","capabilityId":"provider",
                "providerConfigurationDigest":artifacts::digest(&serde_json::to_vec(&wire["adapter"])?),
                "cwd":cwd,"executionIdentity":"acp:fixture-owned-job"}}});
        assert_eq!(
            runtime
                .saved_executor_session(&source, &input)?
                .provider_session_id,
            "actual-worker-session"
        );
        for (pointer, value) in [
            ("/executorInput/workId", json!(Uuid::new_v4().to_string())),
            (
                "/executorInput/sessionSource/originalInvocation/subject/kind",
                json!("Coordination"),
            ),
            ("/executorInput/sessionSource/legacyWorker", json!(false)),
            ("/executorInput/sessionSource/cwd", json!(root)),
            ("/adapter/executable", json!("different-provider")),
            (
                "/executorInput/sessionSource/executionIdentity",
                json!("acp:other-job"),
            ),
        ] {
            let mut changed = input.clone();
            *changed
                .pointer_mut(pointer)
                .context("test pointer absent")? = value;
            assert!(
                runtime.saved_executor_session(&source, &changed).is_err(),
                "{pointer}"
            );
        }
        let mut unsettled = ledger;
        unsettled["state"]["settled"] = json!(false);
        tokio::fs::write(runtime.ledger(&source)?, serde_json::to_vec(&unsettled)?).await?;
        assert!(runtime.saved_executor_session(&source, &input).is_err());
        handle.shutdown().await?;
        tokio::fs::remove_dir_all(root).await?;
        Ok(())
    }
}

impl Runtime {
    pub(super) async fn work_input(&self, params: &Value) -> Result<Response> {
        let invocation = self.lookup(params).await?;
        let input = &params["input"];
        let id = text(input, "turnId")?;
        let receipt = input_receipt(input)?;
        let mut state = invocation.state.lock().await;
        if let Some(previous) = state.work_inputs.get(id) {
            anyhow::ensure!(
                previous == &receipt,
                "Executor input identity reused with different content"
            );
            return Ok(Response::ok(
                "",
                json!({"invocationId":invocation.input["id"],"turnId":id,"disposition":"AlreadyRecorded"}),
            ));
        }
        anyhow::ensure!(
            invocation.input["subject"]["kind"] == "Work"
                && input["workId"] == invocation.input["executorInput"]["workId"]
                && input["workspaceId"] == invocation.input["executorInput"]["workspaceId"]
                && !state.released
                && !invocation.cancel.is_cancelled()
                && state.state == "Idle"
                && input["turnNumber"].as_u64() == Some(state.turn + 1),
            "Executor input requires the idle bound owner and the next serial turn"
        );
        deadline(&json!({"limits":{"deadlineUtc":input["deadlineUtc"]}}))?;
        state.work_inputs.insert(id.into(), receipt);
        self.persist(&invocation, &state)?;
        if let Err(error) = invocation
            .commands
            .try_send(Control::WorkInput(input.clone()))
        {
            state.work_inputs.remove(id);
            self.persist(&invocation, &state)?;
            bail!("Executor input queue unavailable: {error}");
        }
        Ok(Response::ok(
            "",
            json!({"invocationId":invocation.input["id"],"turnId":id,"disposition":"Recorded"}),
        ))
    }

    pub(super) fn saved_executor_session(
        &self,
        reference: &str,
        input: &Value,
    ) -> Result<PrimarySession> {
        let source = &input["executorInput"]["sessionSource"];
        anyhow::ensure!(
            source["invocationId"] == reference,
            "Executor source reference mismatch"
        );
        let ledger: Value = serde_json::from_slice(&std::fs::read(self.ledger(reference)?)?)?;
        let state: InvocationState = serde_json::from_value(ledger["state"].clone())?;
        anyhow::ensure!(
            state.released
                && state.settled
                && !state.execution_identity.is_empty()
                && (state.settlement_proof.as_deref() == Some(state.execution_identity.as_str())
                    || state
                        .terminal_observation
                        .as_ref()
                        .is_some_and(|end| end["quiescent"] == true)),
            "Actual executor execution is not proven settled and released"
        );
        let session = if source["legacyWorker"] == true {
            let mut original = source["originalInvocation"].clone();
            anyhow::ensure!(
                original["id"] == reference
                    && original["subject"]["kind"] == "Task"
                    && original["dispatch"]["kind"] == "ProduceResult"
                    && original["dispatch"]["workId"] == input["executorInput"]["workId"]
                    && original["dispatch"]["workspaceId"] == input["executorInput"]["workspaceId"]
                    && original["capabilityId"] == input["capabilityId"]
                    && original.get("sessionReuseRef").is_none()
                    && source["executionIdentity"] == state.execution_identity,
                "Only the original actual worker can be adopted"
            );
            original["adapter"] = input["adapter"].clone();
            anyhow::ensure!(
                ledger["inputDigest"] == artifacts::digest(&serde_json::to_vec(&original)?)
                    && ledger["binding"]["invocationId"] == original["id"]
                    && ledger["binding"]["runtimeId"] == original["runtimeId"]
                    && ledger["binding"]["bindingGeneration"] == original["bindingGeneration"],
                "Original worker intent/provider configuration does not match its durable ledger"
            );
            PrimarySession {
                work_id: text(&input["executorInput"], "workId")?.into(),
                capability_id: text(source, "capabilityId")?.into(),
                provider_configuration_digest: text(source, "providerConfigurationDigest")?.into(),
                provider_session_id: text(source, "providerSessionId")?.into(),
                cwd: PathBuf::from(text(source, "cwd")?),
            }
        } else {
            anyhow::ensure!(
                state.execution_kind == "Work",
                "A coordinator cannot be loaded as a work executor"
            );
            state
                .primary_session
                .context("Executor session association missing")?
        };
        Uuid::parse_str(&session.work_id)?;
        let workspace = text(&input["executorInput"], "workspaceId")?;
        Uuid::parse_str(workspace)?;
        let root = std::fs::canonicalize(self.inner.root.join("workspaces"))?;
        let expected = std::fs::canonicalize(root.join(workspace))?;
        let cwd = std::fs::canonicalize(&session.cwd)?;
        anyhow::ensure!(
            expected.starts_with(&root)
                && cwd == expected
                && session.cwd.is_absolute()
                && source["providerSessionId"] == session.provider_session_id
                && source["cwd"] == serde_json::to_value(&session.cwd)?
                && !session.provider_session_id.trim().is_empty()
                && input["executorInput"]["workId"] == session.work_id
                && input["capabilityId"] == session.capability_id
                && artifacts::digest(&serde_json::to_vec(&input["adapter"])?)
                    == session.provider_configuration_digest,
            "Saved executor work/provider/canonical workspace provenance does not match"
        );
        Ok(session)
    }
}
