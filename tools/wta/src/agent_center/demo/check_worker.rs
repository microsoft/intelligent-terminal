// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! A bounded local executor for the opt-in decision-handoff demonstration.

use super::scenario::work_graph::{Handoff, Receipt, WorkerUpdate};
use anyhow::{ensure, Context, Result};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    sync::mpsc,
    task::JoinHandle,
};

pub(in crate::agent_center) struct Worker {
    receipts: mpsc::UnboundedReceiver<Receipt>,
    task: Option<JoinHandle<()>>,
}

impl Worker {
    pub(in crate::agent_center) fn start(root: PathBuf, handoff: Handoff) -> Self {
        let (sender, receipts) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let result =
                tokio::time::timeout(Duration::from_secs(30), execute(root, &handoff, &sender))
                    .await
                    .context("Local compatibility worker exceeded its 30-second limit")
                    .and_then(|result| result);
            if let Err(error) = result {
                tracing::error!(target: "wta::agent_center::demo", error = %error, "Local compatibility worker failed");
                let message: String = format!("{error:#}").chars().take(1000).collect();
                if sender
                    .send(handoff.receipt(WorkerUpdate::Failed { message }))
                    .is_err()
                {
                    tracing::debug!(target: "wta::agent_center::demo", "Demo UI closed before worker failure delivery");
                }
            }
        });
        Self {
            receipts,
            task: Some(task),
        }
    }

    pub(in crate::agent_center) async fn next(&mut self) -> Result<Receipt> {
        self.receipts
            .recv()
            .await
            .context("Local compatibility worker closed without a final receipt")
    }

    pub(in crate::agent_center) async fn finish(mut self) -> Result<()> {
        if let Some(task) = self.task.take() {
            task.await.context("Joining local compatibility worker")?;
        }
        Ok(())
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn execute(
    root: PathBuf,
    handoff: &Handoff,
    sender: &mpsc::UnboundedSender<Receipt>,
) -> Result<()> {
    tokio::time::sleep(Duration::from_secs(2)).await;
    let parent = root.join("decision-checks");
    tokio::fs::create_dir_all(&parent)
        .await
        .context("Creating isolated check directory")?;
    let directory = parent.join(uuid::Uuid::new_v4().to_string());
    tokio::fs::create_dir(&directory)
        .await
        .context("Creating unique local check attempt")?;
    tokio::fs::write(
        directory.join("check-worker.cjs"),
        include_str!("check-worker.cjs"),
    )
    .await
    .context("Writing the fixed local check worker")?;
    tokio::fs::write(
        directory.join("test-compatibility.cjs"),
        include_str!("test-compatibility.cjs"),
    )
    .await
    .context("Writing the unchanged compatibility fixture")?;
    let request = serde_json::to_vec(handoff)?;
    tokio::fs::write(directory.join("decision.json"), &request)
        .await
        .context("Saving the exact worker decision")?;
    let mut command = tokio::process::Command::new("node");
    command
        .arg("check-worker.cjs")
        .current_dir(&directory)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command
        .spawn()
        .context("Starting local Node compatibility worker")?;
    let mut input = child.stdin.take().context("Worker stdin unavailable")?;
    input
        .write_all(&request)
        .await
        .context("Delivering the recorded decision")?;
    input.shutdown().await.context("Closing decision input")?;
    drop(input);
    let output = child.stdout.take().context("Worker stdout unavailable")?;
    let errors = child.stderr.take().context("Worker stderr unavailable")?;
    let mut lines = BufReader::new(output).lines();
    let receive = async {
        let mut final_receipt = None;
        while let Some(line) = lines.next_line().await.context("Reading worker receipt")? {
            ensure!(
                line.len() <= 131_072,
                "Worker receipt exceeds the local demo limit"
            );
            ensure!(
                final_receipt.is_none(),
                "Worker sent data after its final result"
            );
            let receipt: Receipt =
                serde_json::from_str(&line).context("Decoding worker receipt")?;
            if matches!(receipt.update, WorkerUpdate::CheckCompleted { .. }) {
                final_receipt = Some(receipt);
            } else {
                sender
                    .send(receipt)
                    .context("Demo UI closed before receipt delivery")?;
            }
        }
        Ok::<_, anyhow::Error>(final_receipt)
    };
    let read_errors = async {
        let mut stderr = String::new();
        errors
            .take(16_385)
            .read_to_string(&mut stderr)
            .await
            .context("Reading worker errors")?;
        ensure!(
            stderr.len() <= 16_384,
            "Worker error output exceeds the local demo limit"
        );
        Ok::<_, anyhow::Error>(stderr)
    };
    let (final_receipt, stderr) = tokio::try_join!(receive, read_errors)?;
    let status = child
        .wait()
        .await
        .context("Waiting for local check worker")?;
    ensure!(
        status.success(),
        "Local check worker exited {status}: {stderr}"
    );
    let receipt = final_receipt.context("Local worker exited without a check result")?;
    tokio::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )
    .await
    .context("Saving local check output")?;
    sender
        .send(receipt)
        .context("Demo UI closed before check-result delivery")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_center::demo::{
        scenario::{work_graph::*, Scenario},
        Store,
    };

    #[tokio::test]
    #[ignore = "Requires local Node.js; executes only the embedded read-only compatibility fixture"]
    async fn graph_local_worker_delivers_acknowledges_checks_and_persists_actual_output() {
        let root =
            std::env::temp_dir().join(format!("wta-local-decision-check-{}", uuid::Uuid::new_v4()));
        let (store, _) = Store::open(&root, false).unwrap();
        let mut saved = Scenario::new();
        saved.enable_clock().unwrap();
        saved.apply("Open Work graph").unwrap();
        saved.apply(PLAN).unwrap();
        saved.apply(START).unwrap();
        saved.advance(Duration::from_secs(9)).unwrap();
        saved.apply(HANDOFF).unwrap();
        store.save(&saved).unwrap();
        let request = saved.graph().unwrap().handoff.clone().unwrap();
        let mut worker = Worker::start(root.clone(), request);
        let mut count = 0;
        while !saved
            .graph()
            .unwrap()
            .handoff
            .as_ref()
            .unwrap()
            .stage
            .terminal()
        {
            let receipt = worker.next().await.unwrap();
            assert!(
                !matches!(receipt.update, WorkerUpdate::Failed { .. }),
                "{receipt:?}"
            );
            saved.receive_graph_receipt(receipt).unwrap();
            store.save(&saved).unwrap();
            count += 1;
        }
        worker.finish().await.unwrap();
        assert_eq!(count, 4);
        assert_eq!(
            saved.graph().unwrap().handoff.as_ref().unwrap().stage,
            HandoffStage::Completed
        );
        let evidence = saved
            .works
            .iter()
            .find(|work| work.id == COMPATIBILITY)
            .unwrap()
            .evidence
            .last()
            .unwrap();
        assert_eq!(evidence.exit_code, 1);
        assert_eq!(evidence.id, RECHECK_ID);
        let actual = saved
            .events
            .last()
            .unwrap()
            .executor_receipt
            .as_ref()
            .unwrap();
        match &actual.update {
            WorkerUpdate::CheckCompleted { stdout, .. } => {
                assert!(stdout.contains("ERR_ASSERTION"));
                assert!(stdout.contains("# fail 1"));
            }
            other => panic!("Missing actual check output: {other:?}"),
        }
        drop(store);
        let (store, reopened) = Store::open(&root, false).unwrap();
        assert_eq!(saved, reopened);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }
}
