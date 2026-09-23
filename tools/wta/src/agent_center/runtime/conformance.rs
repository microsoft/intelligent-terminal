//! Deterministic end-to-end adapter conformance, not evidence of live AI autonomy.
use super::super::wire::EntityRef;
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persistent_work_executor_retains_server_and_loads_after_hold() -> Result<()> {
    use tokio::io::AsyncReadExt;
    let root = std::env::current_dir()?
        .join("target")
        .join(format!("executor-conformance-{}", Uuid::new_v4()));
    let source = root.join("source");
    let state = root.join("state");
    tokio::fs::create_dir_all(&source).await?;
    tokio::fs::create_dir_all(&state).await?;
    workspace::git(&source, &["init"]).await?;
    tokio::fs::write(source.join("README.txt"), "executor fixture").await?;
    workspace::git(&source, &["add", "--all"]).await?;
    workspace::git(
        &source,
        &[
            "-c",
            "user.name=Conformance",
            "-c",
            "user.email=conformance@localhost",
            "commit",
            "-m",
            "baseline",
        ],
    )
    .await?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("agent_center")
        .join("runtime")
        .join("conformance-agent.ps1");
    tokio::fs::write(state.join("adapters.json"),serde_json::to_vec(&json!({"capabilities":[{
        "id":"executor-fixture","adapter":{"kind":"ACP","executable":"powershell.exe",
        "approvedModelDestination":"Local deterministic executor conformance subprocess",
        "args":["-NoLogo","-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-File",fixture]}}]}))?).await?;
    let handle = super::super::transport::start_engine(state.clone()).await?;
    let runtime = Runtime::new(handle.clone(), state.clone())?;
    runtime.register().await?;
    let project=human(&handle,"project.configure",json!({"name":"Persistent executor fixture","root":source,
        "coordinatorCapabilityId":"executor-fixture","workerCapabilityId":"executor-fixture","checkCapabilityId":"native-check",
        "limits":{"concurrency":2,"executionAttempts":6,"evaluationAttempts":2,"coordinationTurns":2,
        "contextRounds":2,"executionSeconds":90,"coordinationSeconds":1}}),&[]).await?;
    let stop = CancellationToken::new();
    let pump_stop = stop.clone();
    let pump_handle = handle.clone();
    let pump_runtime = runtime.clone();
    let mut pump = tokio::spawn(async move {
        let mut changes = pump_handle.watch_changes();
        loop {
            changes.borrow_and_update();
            for effect in pump_handle.effects().await? {
                pump_runtime.execute(effect).await?;
            }
            tokio::select! {
                _=pump_stop.cancelled()=>return Ok::<_,anyhow::Error>(()),
                changed=changes.changed()=>changed.context("effect stream closed")?,
            }
        }
    });
    let result=async {
        let draft=human(&handle,"work.create_draft",json!({"projectId":project["projectId"],
            "goal":"Keep the fixture server running between chat replies","scope":["workspace"],"exclusions":[],
            "criteria":[{"id":"server","description":"Owned server responds","evidenceRule":"manual"}],
            "context":[],"delivery":{"kind":"LocalCode"},"sourceMessageIds":[]}),&[]).await?;
        let work_id=text(&draft,"workId")?;
        let view=human(&handle,"work.get",json!({"workId":work_id}),&[]).await?;
        let grant=human(&handle,"grant.preview",json!({"workId":work_id,"specRevision":1,"policyRevision":1}),&[view["work"].clone()]).await?;
        human(&handle,"work.start",json!({"workId":work_id,"specRevision":1,"projectPolicyRevision":1,"grantProposalId":grant["grantProposalId"]}),&[view["work"].clone()]).await?;
        let idle=wait_executor(&handle,work_id,"Idle",1).await?;
        anyhow::ensure!(idle["work"]["lifecycle"]=="Active","A normal executor reply completed the work");
        let cwd=runtime.workspace(text(&idle["work"],"workspaceId")?).await?;
        let server:Value=serde_json::from_slice(&tokio::fs::read(cwd.join("executor-server.json")).await?)?;
        let server_process = open_fixture_process(&server)?;
        let original = runtime.inner.invocations.lock().await.get(text(&idle["work"]["primarySession"],"invocationId")?).cloned().context("original executor owner missing")?;
        let owned_job = original.state.lock().await.owned_job.clone().context("executor job missing")?;
        anyhow::ensure!(owned_job.contains(&server_process)?, "Fixture server escaped the executor's exact job");
        anyhow::ensure!(!fixture_process_exited(&server_process)?, "Fixture server exited before the retention assertion");
        let port=server["port"].as_u64().context("fixture server port missing")? as u16;
        let mut socket=tokio::net::TcpStream::connect(("127.0.0.1",port)).await?;
        let mut bytes=[0;5];
        tokio::time::timeout(Duration::from_secs(10),socket.read_exact(&mut bytes)).await??;
        anyhow::ensure!(&bytes==b"alive","Owned background server stopped at the normal reply boundary");
        drop(socket);
        let first_session=idle["work"]["primarySession"].clone();
        let open=human(&handle,"work.open",json!({"workId":work_id}),&[]).await?;
        let mut context=open["context"].clone();
        context.as_object_mut().context("work context missing")?.remove("conversationId");
        human(&handle,"conversation.submit",json!({"conversationId":open["conversation"]["id"],
            "clientMessageId":Uuid::new_v4().to_string(),"attachments":[],"text":"Make the next workspace change without restarting the server","context":context}),&[]).await?;
        let second=wait_executor(&handle,work_id,"Idle",2).await?;
        anyhow::ensure!(second["work"]["primarySession"]==first_session,"Busy/idle chat created a second provider session");
        let content=tokio::fs::read_to_string(cwd.join("executor-turns.txt")).await?;
        let pids=content.lines().map(|line|line.rsplit(':').next().unwrap_or_default()).collect::<Vec<_>>();
        anyhow::ensure!(pids.len()==2 && pids[0]==pids[1],"Serial inputs did not execute in the same actual ACP process: {content}");
        let mut socket=tokio::net::TcpStream::connect(("127.0.0.1",port)).await?;
        tokio::time::timeout(Duration::from_secs(10),socket.read_exact(&mut bytes)).await??;
        anyhow::ensure!(&bytes==b"alive" && !fixture_process_exited(&server_process)?, "The same owned server did not survive the second reply");
        drop(socket);
        human(&handle,"work.control",json!({"workId":work_id,"action":"Hold"}),&[second["work"].clone()]).await?;
        let paused=wait_executor(&handle,work_id,"Paused",2).await?;
        anyhow::ensure!(owned_job.is_settled()? && fixture_process_exited(&server_process)?,
            "Hold returned Paused before the exact owned server/process tree exited (server: {server})");
        {
            let owner = original.state.lock().await;
            anyhow::ensure!(owner.released && owner.settled && owner.settlement_proof.as_deref()==Some(owner.execution_identity.as_str()),
                "Hold did not record release and exact execution settlement");
        }
        // A freed port is not an execution identity. Another parallel fixture may
        // bind it immediately; deliberately reproduce that case after proven exit.
        let replacement=tokio::net::TcpSocket::new_v4()?;
        replacement.set_reuseaddr(true)?;
        replacement.bind(std::net::SocketAddr::from(([127,0,0,1],port)))?;
        let replacement=replacement.listen(1)?;
        let unrelated=tokio::net::TcpStream::connect(("127.0.0.1",port)).await?;
        anyhow::ensure!(fixture_process_exited(&server_process)? && owned_job.is_settled()?,
            "Reusing a port changed the recorded process settlement");
        drop(unrelated);
        drop(replacement);
        human(&handle,"work.continue",json!({"workId":work_id}),&[paused["work"].clone()]).await?;
        let resumed=wait_executor(&handle,work_id,"Idle",3).await?;
        anyhow::ensure!(resumed["work"]["primarySession"]["providerSessionId"]==first_session["providerSessionId"]
            && resumed["work"]["primarySession"]["invocationId"]!=first_session["invocationId"],"Resume did not restore the same saved executor");
        let invocation=runtime.inner.invocations.lock().await.get(text(&resumed["work"]["primarySession"],"invocationId")?).cloned().context("resumed owner missing")?;
        anyhow::ensure!(invocation.input["resumeSession"]["providerSessionId"]==first_session["providerSessionId"],
            "No actual load_session path was selected");
        human(&handle,"work.control",json!({"workId":work_id,"action":"Cancel"}),&[resumed["work"].clone()]).await?;
        wait_executor(&handle,work_id,"Cancelled",3).await?;
        Ok::<_,anyhow::Error>(())
    }.await;
    let shutdown = runtime.shutdown().await;
    stop.cancel();
    let pumping = match tokio::time::timeout(Duration::from_secs(15), &mut pump).await {
        Ok(joined) => joined
            .context("executor fixture pump failed")
            .and_then(|result| result),
        Err(error) => {
            pump.abort();
            let _ = pump.await;
            Err(error).context("executor fixture pump did not finish")
        }
    };
    let closing = tokio::time::timeout(Duration::from_secs(15), handle.shutdown())
        .await
        .context("executor fixture authority did not finish")
        .and_then(|result| result);
    let cleaning = if shutdown.is_ok() && closing.is_ok() {
        cleanup_after_shutdown(&root).await
    } else {
        Err(anyhow::anyhow!(
            "Retained fixture directory {} because shutdown was not proven complete",
            root.display()
        ))
    };
    let errors = [
        ("execution", result),
        ("runtime shutdown", shutdown),
        ("effect pump", pumping),
        ("authority shutdown", closing),
        ("cleanup", cleaning),
    ]
    .into_iter()
    .filter_map(|(stage, result)| result.err().map(|error| format!("{stage}: {error:#}")))
    .collect::<Vec<_>>();
    anyhow::ensure!(
        errors.is_empty(),
        "Persistent executor conformance failed:\n{}",
        errors.join("\n")
    );
    Ok(())
}

fn open_fixture_process(server: &Value) -> Result<std::os::windows::io::OwnedHandle> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };
    let pid = u32::try_from(
        server["pid"]
            .as_u64()
            .context("fixture server PID missing")?,
    )?;
    // SAFETY: this opens only the fixture-published PID, with query/wait rights.
    let handle = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        )
    };
    anyhow::ensure!(
        !handle.is_null(),
        "Could not open fixture server {pid}: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: OpenProcess returned a new owned handle; retaining it prevents PID-reuse ambiguity.
    Ok(unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(handle) })
}

fn fixture_process_exited(process: &std::os::windows::io::OwnedHandle) -> Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
    // SAFETY: the owned process handle remains valid throughout this nonblocking wait.
    match unsafe {
        windows_sys::Win32::System::Threading::WaitForSingleObject(process.as_raw_handle(), 0)
    } {
        WAIT_OBJECT_0 => Ok(true),
        WAIT_TIMEOUT => Ok(false),
        _ => Err(std::io::Error::last_os_error()).context("query exact fixture process exit"),
    }
}

#[tokio::test]
async fn executor_fixture_cleanup_waits_for_transient_sharing_lock() -> Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let root = std::env::current_dir()?
        .join("target")
        .join(format!("executor-cleanup-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&root)?;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .share_mode(0)
        .open(root.join("late-close.log"))?;
    let cleaning = cleanup_after_shutdown(&root);
    tokio::pin!(cleaning);
    assert!(futures::poll!(&mut cleaning).is_pending());
    drop(file);
    cleaning.await?;
    assert!(!root.exists());
    Ok(())
}

async fn cleanup_after_shutdown(root: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if !root.exists() {
            return Ok(());
        }
        match cleanup(root) {
            Ok(()) => return Ok(()),
            Err(error)
                if error.downcast_ref::<std::io::Error>().is_some_and(|error| {
                    matches!(error.raw_os_error(), Some(5 | 32 | 33 | 145))
                }) && tokio::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("clean settled fixture {}", root.display()))
            }
        }
    }
}

async fn wait_executor(
    handle: &ServiceHandle,
    work_id: &str,
    activity: &str,
    turns: u64,
) -> Result<Value> {
    let mut changes = handle.watch_changes();
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            changes.borrow_and_update();
            let view = human(handle, "work.get", json!({"workId":work_id}), &[]).await?;
            if view["continuation"]["activity"] == activity
                && view["work"]["usage"]["executionAttempts"]
                    .as_u64()
                    .unwrap_or(0)
                    >= turns
            {
                return Ok(view);
            }
            if view["continuation"]["state"] == "Unavailable" {
                bail!("Executor became unavailable: {view}");
            }
            changes
                .changed()
                .await
                .context("executor change stream ended")?;
        }
    })
    .await
    .context("executor did not reach its expected bounded state")?
}

async fn human(
    handle: &ServiceHandle,
    method: &str,
    params: Value,
    subjects: &[Value],
) -> Result<Value> {
    let mut request = Request::new(method, params);
    if !super::super::schemas::is_read(method) {
        request.command_id = Some(Uuid::new_v4().to_string());
    }
    request.if_match = subjects
        .iter()
        .map(|record| EntityRef {
            kind: record["kind"].as_str().unwrap_or_default().to_owned(),
            id: record["id"].as_str().unwrap_or_default().to_owned(),
            version: record["version"].as_u64().unwrap_or_default(),
        })
        .collect();
    let response = handle.request(Principal::Human, request).await;
    if !matches!(response.status.as_str(), "ok" | "pending") {
        bail!("{method}: {}", serde_json::to_string(&response)?);
    }
    response.data.context("response data missing")
}

async fn wait_work(handle: &ServiceHandle, work_id: &str, completed: bool) -> Result<Value> {
    let mut changes = handle.watch_changes();
    tokio::time::timeout(Duration::from_secs(150), async {
        loop {
            changes.borrow_and_update();
            let view = human(handle, "work.get", json!({"workId":work_id}), &[]).await?;
            if (!completed && view.get("candidate").is_some())
                || (completed && view["work"]["lifecycle"] == "Completed")
            {
                return Ok(view);
            }
            if let Some(problem) = view["obligations"].as_array().and_then(|items| {
                items.iter().find(|item| {
                    matches!(
                        item["reason"].as_str().unwrap_or_default(),
                        "PROTOCOL_INCOMPLETE"
                            | "EffectFailed"
                            | "MissingSubmission"
                            | "CoordinationAllowanceExhausted"
                    )
                })
            }) {
                bail!("conformance execution has a blocking obligation: {problem}; work: {view}");
            }
            changes
                .changed()
                .await
                .context("service change stream ended")?;
        }
    })
    .await
    .context("conformance work did not reach its bounded terminal target")?
}

async fn evidence(handle: &ServiceHandle, reference: &Value) -> Result<Value> {
    let record = human(
        handle,
        "artifact.get",
        json!({"artifactId":reference["artifactId"]}),
        &[],
    )
    .await?;
    let record = record.get("artifact").unwrap_or(&record);
    let directory = artifacts::verify(record)?;
    serde_json::from_slice(&tokio::fs::read(directory.join("process.json")).await?)
        .context("native evidence JSON")
}

async fn journey(
    handle: &ServiceHandle,
    source: &Path,
    project: &Value,
    label: &str,
) -> Result<Value> {
    let draft = human(handle, "work.create_draft", json!({
        "executionMode":"LegacyTasks","projectId":project["projectId"],"goal":format!("Work {label}: deliver a file whose parsed integer is one"),
        "scope":["value.txt"],"exclusions":[],"criteria":[{"id":"value","description":"Value equals one","evidenceRule":if label == "A" {
            "The captured value must pass the executable check for the integer one."
        } else {
            "command:value-check"
        }}],
        "context":[],"delivery":{"kind":"LocalCode"},"sourceMessageIds":[]
    }), &[]).await?;
    let work_id = text(&draft, "workId")?;
    let view = human(handle, "work.get", json!({"workId":work_id}), &[]).await?;
    let preview = human(
        handle,
        "grant.preview",
        json!({"workId":work_id,"specRevision":1,"policyRevision":1}),
        &[view["work"].clone()],
    )
    .await?;
    human(handle, "work.start", json!({
        "workId":work_id,"specRevision":1,"projectPolicyRevision":1,"grantProposalId":preview["grantProposalId"]
    }), &[view["work"].clone()]).await?;

    let ready = wait_work(handle, work_id, false).await?;
    let candidate = &ready["candidate"];
    let accepted = human(
        handle,
        "result.get",
        json!({"resultId":candidate["integrationResultId"]}),
        &[],
    )
    .await?;
    assert_eq!(accepted["disposition"], "Accepted");
    let prior_id = text(&accepted["body"], "supersedesResultId")?;
    let rejected = human(handle, "result.get", json!({"resultId":prior_id}), &[]).await?;
    assert_eq!(rejected["gates"][0]["outcome"], "Failed");
    assert_eq!(accepted["gates"][0]["outcome"], "Passed");
    let failed_evidence = evidence(handle, &rejected["gates"][0]["evidence"][0]).await?;
    let passed_evidence = evidence(handle, &accepted["gates"][0]["evidence"][0]).await?;
    assert_eq!(failed_evidence["outcome"]["exitCode"], 7);
    assert_eq!(passed_evidence["outcome"]["exitCode"], 0);
    assert_eq!(failed_evidence["outcome"]["stderr"], "expected 1, actual 0");
    assert_eq!(passed_evidence["outcome"]["stdout"], "actual check passed");
    assert_ne!(
        failed_evidence["inputManifestDigest"],
        passed_evidence["inputManifestDigest"]
    );
    let task = human(
        handle,
        "task.get",
        json!({"taskId":accepted["result"]["taskId"]}),
        &[],
    )
    .await?;
    assert!(task["contextRequests"]
        .as_array()
        .context("context requests missing")?
        .iter()
        .any(|question| question["status"] == "Applied"));
    assert!(task["contextRequests"]
        .as_array()
        .context("context requests missing")?
        .iter()
        .all(|question| question["workId"] == work_id));
    let directory = PathBuf::from(text(&candidate["destination"], "localPath")?);
    assert_eq!(
        tokio::fs::read_to_string(directory.join("value.txt")).await?,
        "1"
    );
    assert!(text(&candidate["destination"], "commitId")?.len() >= 40);
    assert_eq!(
        tokio::fs::read_to_string(source.join("value.txt")).await?,
        "uncommitted user value"
    );
    human(
        handle,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        &[ready["work"].clone(), candidate.clone()],
    )
    .await?;
    let completed = wait_work(handle, work_id, true).await?;
    assert_eq!(completed["work"]["lifecycle"], "Completed");
    Ok(json!({
        "workId":work_id,"workspaceId":ready["work"]["workspaceId"],"candidateId":candidate["id"],
        "artifactId":candidate["destination"]["artifact"]["artifactId"],"localPath":candidate["destination"]["localPath"]
    }))
}

fn cleanup(path: &Path) -> Result<()> {
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            cleanup(&entry.path())?;
        } else {
            let mut permissions = entry.metadata()?.permissions();
            #[allow(clippy::permissions_set_readonly_false)]
            permissions.set_readonly(false);
            std::fs::set_permissions(entry.path(), permissions)?;
            std::fs::remove_file(entry.path())?;
        }
    }
    std::fs::remove_dir(path)?;
    Ok(())
}

async fn report_journey(
    handle: &ServiceHandle,
    project: &Value,
    pinned_code: bool,
) -> Result<Value> {
    let goal = if pinned_code {
        "PINNED_REPORT_CONFORMANCE"
    } else {
        "FILE_REPORT_CONFORMANCE"
    };
    let draft = human(handle, "work.create_draft", json!({
        "executionMode":"LegacyTasks","projectId":project["projectId"],"goal":goal,
        "scope":["value.txt","report.txt","evidence.json"],"exclusions":[],
        "criteria":[{"id":"report","description":"The fixed report and its required inputs pass the declared check","evidenceRule":"command:report-check"}],
        "context":[],"delivery":{"kind":if pinned_code { "LocalCode" } else { "Report" }},"sourceMessageIds":[]
    }), &[]).await?;
    let work_id = text(&draft, "workId")?;
    let view = human(handle, "work.get", json!({"workId":work_id}), &[]).await?;
    let preview = human(
        handle,
        "grant.preview",
        json!({
            "workId":work_id,"specRevision":1,"policyRevision":1
        }),
        &[view["work"].clone()],
    )
    .await?;
    human(handle, "work.start", json!({
        "workId":work_id,"specRevision":1,"projectPolicyRevision":1,"grantProposalId":preview["grantProposalId"]
    }), &[view["work"].clone()]).await?;
    let ready = wait_work(handle, work_id, false).await?;
    let candidate = &ready["candidate"];
    let accepted = human(
        handle,
        "result.get",
        json!({"resultId":candidate["integrationResultId"]}),
        &[],
    )
    .await?;
    assert_eq!(accepted["disposition"], "Accepted");
    let outputs = accepted["body"]["outputs"]
        .as_array()
        .context("report outputs missing")?;
    assert_eq!(outputs.len(), 2);
    for output in outputs {
        let record = human(
            handle,
            "artifact.get",
            json!({"artifactId":output["artifact"]["artifactId"]}),
            &[],
        )
        .await?;
        let record = record.get("artifact").unwrap_or(&record);
        assert_eq!(record["artifactKind"], "File");
        let root = artifacts::verify(record)?;
        if output["slot"] == "report" {
            assert_eq!(
                tokio::fs::read_to_string(root.join("report.txt")).await?,
                "fixed report"
            );
        }
    }
    assert_eq!(accepted["gates"][0]["outcome"], "Passed");
    let process = evidence(handle, &accepted["gates"][0]["evidence"][0]).await?;
    assert_eq!(process["outcome"]["stdout"], "file-report-check-passed");
    assert_eq!(process["outcome"]["exitCode"], 0);
    assert_eq!(
        process["inputManifestDigest"],
        accepted["gates"][0]["inputManifestDigest"]
    );
    if pinned_code {
        let source_id = text(&candidate["destination"], "sourceResultId")?;
        assert_ne!(source_id, text(candidate, "integrationResultId")?);
        let upstream = human(handle, "result.get", json!({"resultId":source_id}), &[]).await?;
        assert_eq!(upstream["disposition"], "Accepted");
        assert_eq!(
            candidate["destination"]["artifact"],
            upstream["body"]["outputs"][0]["artifact"]
        );
        let destination = PathBuf::from(text(&candidate["destination"], "localPath")?);
        assert_eq!(
            tokio::fs::read_to_string(destination.join("value.txt")).await?,
            "1"
        );
    }
    human(
        handle,
        "delivery.accept",
        json!({"candidateId":candidate["id"]}),
        &[ready["work"].clone(), candidate.clone()],
    )
    .await?;
    assert_eq!(
        wait_work(handle, work_id, true).await?["work"]["lifecycle"],
        "Completed"
    );
    Ok(json!({"workId":work_id,"candidateId":candidate["id"]}))
}

async fn decline_journey(handle: &ServiceHandle, runtime: &Runtime, project: &Value) -> Result<()> {
    let draft = human(handle, "work.create_draft", json!({
        "executionMode":"LegacyTasks","projectId":project["projectId"],"goal":"DECLINE_CONFORMANCE retain the unavailable-evidence reason",
        "scope":["value.txt"],"exclusions":[],"criteria":[{"id":"value","description":"Value equals one","evidenceRule":"command:value-check"}],
        "context":[],"delivery":{"kind":"LocalCode"},"sourceMessageIds":[]
    }), &[]).await?;
    let work_id = text(&draft, "workId")?;
    let view = human(handle, "work.get", json!({"workId":work_id}), &[]).await?;
    let preview = human(
        handle,
        "grant.preview",
        json!({"workId":work_id,"specRevision":1,"policyRevision":1}),
        &[view["work"].clone()],
    )
    .await?;
    human(handle, "work.start", json!({
        "workId":work_id,"specRevision":1,"projectPolicyRevision":1,"grantProposalId":preview["grantProposalId"]
    }), &[view["work"].clone()]).await?;
    let mut changes = handle.watch_changes();
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            changes.borrow_and_update();
            let tasks = human(
                handle,
                "task.list",
                json!({"workId":work_id,"limit":100}),
                &[],
            )
            .await?;
            if let Some(task) = tasks["items"].as_array().and_then(|items| items.first()) {
                if task["attempt"]["state"] == "Failed"
                    && task["attempt"]["reservationHeld"] == false
                {
                    assert_eq!(task["attempt"]["endReason"], "ContractDeclined");
                    assert_eq!(
                        task["attempt"]["declineReason"],
                        "Required diagnostic evidence is unavailable in this assignment"
                    );
                    let invocations: Vec<_> = runtime
                        .inner
                        .invocations
                        .lock()
                        .await
                        .values()
                        .cloned()
                        .collect();
                    for invocation in invocations {
                        let input = &invocation.input["coordinationInput"];
                        if input["scope"]["workId"] == work_id
                            && input["snapshot"]["declinedAttempts"]
                                .as_array()
                                .is_some_and(|items| !items.is_empty())
                        {
                            let state = invocation.state.lock().await;
                            if state.released && !state.terminal_record_ids.is_empty() {
                                return Ok::<_, anyhow::Error>(());
                            }
                        }
                    }
                }
            }
            changes
                .changed()
                .await
                .context("decline change stream ended")?;
        }
    })
    .await
    .context("declined ACP contract did not settle and relay its reason")?
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_stdio_mcp_capture_context_rework_check_and_delivery() -> Result<()> {
    let root = std::env::current_dir()?
        .join("target")
        .join(format!("center-conformance-{}", Uuid::new_v4()));
    let source = root.join("source");
    let state = root.join("state");
    tokio::fs::create_dir_all(&source).await?;
    tokio::fs::create_dir_all(&state).await?;
    workspace::git(&source, &["init"]).await?;
    tokio::fs::write(source.join("value.txt"), "baseline").await?;
    workspace::git(&source, &["add", "--all"]).await?;
    workspace::git(
        &source,
        &[
            "-c",
            "user.name=Conformance",
            "-c",
            "user.email=conformance@localhost",
            "commit",
            "-m",
            "baseline",
        ],
    )
    .await?;
    let baseline = workspace::git(&source, &["rev-parse", "HEAD"]).await?;
    tokio::fs::write(source.join("value.txt"), "uncommitted user value").await?;
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("agent_center")
        .join("runtime")
        .join("conformance-agent.ps1");
    tokio::fs::write(state.join("adapters.json"), serde_json::to_vec(&json!({
        "capabilities":[{"id":"conformance-agent","adapter":{"kind":"ACP","executable":"powershell.exe",
            "approvedModelDestination":"Local deterministic conformance subprocess; no external model or network service",
            "args":["-NoLogo","-NoProfile","-NonInteractive","-ExecutionPolicy","Bypass","-File",fixture]}}]
    }))?).await?;
    let handle = super::super::transport::start_engine(state.clone()).await?;
    let runtime = Runtime::new(handle.clone(), state.clone())?;
    runtime.register().await?;
    let project = human(&handle, "project.configure", json!({
        "name":"Two-work scripted adapter conformance, not live AI",
        "root":source,"coordinatorCapabilityId":"conformance-agent","workerCapabilityId":"conformance-agent",
        "checkCapabilityId":"native-check",
        "limits":{"concurrency":3,"executionAttempts":4,"evaluationAttempts":4,"coordinationTurns":12,
            "contextRounds":2,"executionSeconds":90,"coordinationSeconds":90}
    }), &[]).await?;
    let stop = CancellationToken::new();
    let pump_stop = stop.clone();
    let pump_handle = handle.clone();
    let pump_runtime = runtime.clone();
    let pump = tokio::spawn(async move {
        let mut changes = pump_handle.watch_changes();
        loop {
            changes.borrow_and_update();
            for effect in pump_handle.effects().await? {
                pump_runtime.execute(effect).await?;
            }
            tokio::select! {
                _ = pump_stop.cancelled() => return Ok::<_, anyhow::Error>(()),
                changed = changes.changed() => changed.context("effect stream closed")?,
            }
        }
    });
    let result = async {
        let (a, b) = tokio::try_join!(
            journey(&handle, &source, &project, "A"),
            journey(&handle, &source, &project, "B")
        )?;
        for key in [
            "workId",
            "workspaceId",
            "candidateId",
            "artifactId",
            "localPath",
        ] {
            anyhow::ensure!(a[key] != b[key], "concurrent works shared {key}: {a}; {b}");
        }
        let (files, pinned) = tokio::try_join!(
            report_journey(&handle, &project, false),
            report_journey(&handle, &project, true)
        )?;
        assert_ne!(files["workId"], pinned["workId"]);
        assert_ne!(files["candidateId"], pinned["candidateId"]);
        decline_journey(&handle, &runtime, &project).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    let shutdown = runtime.shutdown().await;
    stop.cancel();
    let pumping = pump.await?;
    handle.shutdown().await?;
    if let Err(error) = &result {
        eprintln!("Conformance failure: {error:#}");
        let invocations: Vec<_> = runtime
            .inner
            .invocations
            .lock()
            .await
            .values()
            .cloned()
            .collect();
        for invocation in invocations {
            let observation = invocation.state.lock().await.terminal_observation.clone();
            if let Some(observation) = observation {
                eprintln!(
                    "Invocation {} terminal observation: {observation}",
                    invocation.input["id"]
                );
            }
        }
        if let Ok(logs) = std::fs::read_dir(state.join("invocation-logs")) {
            for log in logs.flatten() {
                if let Ok(content) = std::fs::read_to_string(log.path()) {
                    eprintln!(
                        "{}: {}",
                        log.file_name().to_string_lossy(),
                        content.chars().take(12000).collect::<String>()
                    );
                }
            }
        }
    }
    assert_eq!(
        workspace::git(&source, &["rev-parse", "HEAD"]).await?,
        baseline
    );
    // Every process has settled before removing the isolated test repository/state.
    cleanup(&root)?;
    shutdown?;
    pumping?;
    result
}
