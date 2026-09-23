// Copyright (c) Microsoft Corporation.
// Licensed under the MIT license.

//! Fixed local fixture executor, never a provider session or a user-workspace mutation.

use super::scenario::release_journey::{
    DiagnosisReport, DiagnosisUpdate, Receipt, Update, DIAGNOSIS,
};
use anyhow::{ensure, Context, Result};
use std::{path::PathBuf, process::Stdio, time::Duration};
use tokio::{sync::mpsc, task::JoinHandle};

const INITIAL: &str = "exports.quote = value => \"'\" + value + \"'\";\n";
const REPAIRED: &str = "exports.quote = value => '\"' + value + '\"';\n";
const UNIT: &str = r#"const {test} = require('node:test');
const assert = require('node:assert/strict');
const {quote} = require('./implementation.cjs');
test('keeps argument contents', () => assert.equal(quote('hello world').slice(1, -1), 'hello world'));
test('adds matching quote delimiters', () => {
  const result = quote('value');
  assert.equal(result[0], result.at(-1));
  assert.ok(["'", '"'].includes(result[0]));
});
test('handles an empty argument', () => assert.equal(quote('').length, 2));
"#;
const COMPATIBILITY: &str = r#"const {test} = require('node:test');
const assert = require('node:assert/strict');
const {quote} = require('./implementation.cjs');
test('preserves legacy double-quote contract', () => assert.equal(quote('hello world'), '"hello world"'));
"#;
const STARTUP_CONFIG: &str = r#"{"startup":{"legacyCachePath":"cache-v1"}}"#;
const DIAGNOSE_SCRIPT: &str = r#"const fs = require('node:fs');
const config = JSON.parse(fs.readFileSync('startup-config.json', 'utf8'));
if (typeof config.startup?.legacyCachePath !== 'string') throw new Error('Expected legacy cache configuration');
console.log(JSON.stringify({
  cause: 'Deprecated legacyCachePath triggers the startup warning.',
  recommendation: 'Replace legacyCachePath with cachePath after review.',
  filesChanged: 0
}));
"#;

#[derive(Clone, Copy)]
enum Job {
    Release { attempt: u8, parallel_story: bool },
    Diagnosis,
}

pub(in crate::agent_center) struct Worker {
    receipts: mpsc::UnboundedReceiver<Receipt>,
    task: Option<JoinHandle<()>>,
}

impl Worker {
    pub(in crate::agent_center) fn start(root: PathBuf, attempt: u8) -> Self {
        Self::launch(
            root,
            Job::Release {
                attempt,
                parallel_story: false,
            },
        )
    }

    pub(in crate::agent_center) fn start_parallel(root: PathBuf, attempt: u8) -> Self {
        Self::launch(
            root,
            Job::Release {
                attempt,
                parallel_story: true,
            },
        )
    }

    pub(in crate::agent_center) fn start_diagnosis(root: PathBuf) -> Self {
        Self::launch(root, Job::Diagnosis)
    }

    fn launch(root: PathBuf, job: Job) -> Self {
        let (sender, receipts) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let result = tokio::time::timeout(Duration::from_secs(30), async {
                match job {
                    Job::Release {
                        attempt,
                        parallel_story,
                    } => execute(root, attempt, parallel_story, &sender).await,
                    Job::Diagnosis => execute_diagnosis(root, &sender).await,
                }
            })
            .await
            .context("Local Work worker exceeded 30 seconds")
            .and_then(|result| result);
            if let Err(error) = result {
                tracing::error!(target: "wta::agent_center::demo", %error, "Local Work execution failed");
                let message: String = format!("{error:#}").chars().take(1000).collect();
                let receipt = match job {
                    Job::Release { attempt, .. } => Receipt {
                        attempt,
                        update: Update::Failed { message },
                    },
                    Job::Diagnosis => Receipt {
                        attempt: 1,
                        update: Update::Diagnosis {
                            update: DiagnosisUpdate::Failed { message },
                        },
                    },
                };
                if sender.send(receipt).is_err() {
                    tracing::debug!(target: "wta::agent_center::demo", "Release UI closed before error delivery");
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
            .context("Release worker closed without a final receipt")
    }

    pub(in crate::agent_center) async fn finish(mut self) -> Result<()> {
        if let Some(task) = self.task.take() {
            task.await.context("Joining release fixture worker")?;
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
    attempt: u8,
    parallel_story: bool,
    sender: &mpsc::UnboundedSender<Receipt>,
) -> Result<()> {
    ensure!((1..=2).contains(&attempt), "Invalid release attempt");
    let directory = root
        .join("release-checks")
        .join(format!("{attempt}-{}", uuid::Uuid::new_v4()));
    tokio::fs::create_dir_all(&directory)
        .await
        .context("Creating isolated release check directory")?;
    for (name, contents) in [
        ("implementation-before.cjs", INITIAL),
        (
            "implementation.cjs",
            if attempt == 1 { INITIAL } else { REPAIRED },
        ),
        ("test-unit.cjs", UNIT),
        ("test-compatibility.cjs", COMPATIBILITY),
    ] {
        tokio::fs::write(directory.join(name), contents)
            .await
            .context("Writing fixed release fixture")?;
    }
    tokio::fs::write(
        directory.join("request.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "attempt": attempt, "workId": "fix-4821",
            "instruction": "Preserve legacy double quotes; run unit and compatibility checks",
            "repairAuthorized": attempt == 2,
            "coordination": "scripted", "execution": "local-node-fixture"
        }))?,
    )
    .await
    .context("Saving release check request")?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    sender
        .send(Receipt {
            attempt,
            update: Update::Started,
        })
        .context("Release UI closed")?;
    for kind in ["unit", "compatibility"] {
        let delay = if kind == "unit" {
            3
        } else if parallel_story && attempt == 1 {
            16
        } else {
            2
        };
        tokio::time::sleep(Duration::from_secs(delay)).await;
        let mut command = tokio::process::Command::new("node");
        command
            .args(["--test", "--test-reporter=tap", &format!("test-{kind}.cjs")])
            .current_dir(&directory)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        let output = command
            .output()
            .await
            .context("Running local release check")?;
        let exit_code = output
            .status
            .code()
            .context("Release check terminated without exit code")?;
        ensure!(
            output.stdout.len() <= 65536 && output.stderr.len() <= 16384,
            "Release output too large"
        );
        let stdout = String::from_utf8(output.stdout).context("Invalid UTF-8 in release output")?;
        let stderr = String::from_utf8(output.stderr).context("Invalid UTF-8 in release errors")?;
        let update = if kind == "unit" {
            Update::Unit {
                exit_code,
                stdout,
                stderr,
            }
        } else {
            Update::Compatibility {
                exit_code,
                stdout,
                stderr,
            }
        };
        let receipt = Receipt { attempt, update };
        tokio::fs::write(
            directory.join(format!("{kind}-result.json")),
            serde_json::to_vec_pretty(&receipt)?,
        )
        .await
        .context("Persisting exact release check result")?;
        sender
            .send(receipt)
            .context("Release UI closed before result delivery")?;
    }
    Ok(())
}

async fn execute_diagnosis(root: PathBuf, sender: &mpsc::UnboundedSender<Receipt>) -> Result<()> {
    let directory = root
        .join("diagnosis-checks")
        .join(uuid::Uuid::new_v4().to_string());
    tokio::fs::create_dir_all(&directory)
        .await
        .context("Creating separate diagnosis directory")?;
    for (name, contents) in [
        ("startup-config.json", STARTUP_CONFIG),
        ("diagnose.cjs", DIAGNOSE_SCRIPT),
    ] {
        tokio::fs::write(directory.join(name), contents)
            .await
            .context("Writing read-only diagnosis fixture")?;
    }
    tokio::fs::write(directory.join("request.json"), serde_json::to_vec_pretty(&serde_json::json!({
        "workId": DIAGNOSIS, "executorSessionId": "scripted-startup-diagnosis-session",
        "goal": "Explain the startup warning",
        "instruction": "Report the cause and a safe fix; do not change files.",
        "context": ["startup-config.json"], "coordination": "scripted", "execution": "local-node-fixture"
    }))?).await.context("Saving scoped diagnosis request")?;
    tokio::time::sleep(Duration::from_secs(2)).await;
    sender
        .send(Receipt {
            attempt: 1,
            update: Update::Diagnosis {
                update: DiagnosisUpdate::Started,
            },
        })
        .context("Diagnosis UI closed")?;
    tokio::time::sleep(Duration::from_secs(8)).await;
    let mut command = tokio::process::Command::new("node");
    command
        .arg("diagnose.cjs")
        .current_dir(&directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let output = command
        .output()
        .await
        .context("Running separate local diagnosis")?;
    ensure!(
        output.stdout.len() <= 16384 && output.stderr.len() <= 16384,
        "Diagnosis output too large"
    );
    let exit_code = output
        .status
        .code()
        .context("Diagnosis terminated without exit code")?;
    let stdout = String::from_utf8(output.stdout).context("Invalid diagnosis output encoding")?;
    let stderr = String::from_utf8(output.stderr).context("Invalid diagnosis error encoding")?;
    ensure!(
        tokio::fs::read_to_string(directory.join("startup-config.json")).await? == STARTUP_CONFIG,
        "Read-only diagnosis changed its input"
    );
    if exit_code == 0 {
        let report: DiagnosisReport =
            serde_json::from_str(&stdout).context("Parsing diagnosis report")?;
        ensure!(
            report.files_changed == 0
                && !report.cause.trim().is_empty()
                && !report.recommendation.trim().is_empty(),
            "Invalid read-only diagnosis report"
        );
    }
    let receipt = Receipt {
        attempt: 1,
        update: Update::Diagnosis {
            update: DiagnosisUpdate::Completed {
                exit_code,
                stdout,
                stderr,
            },
        },
    };
    tokio::fs::write(
        directory.join("result.json"),
        serde_json::to_vec_pretty(&receipt)?,
    )
    .await
    .context("Saving actual diagnosis result")?;
    sender
        .send(receipt)
        .context("Diagnosis UI closed before result delivery")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_center::demo::{scenario::release_journey::*, Store};

    #[tokio::test]
    #[ignore = "Requires Node.js; runs two isolated local workers concurrently and a release repair"]
    async fn parallel_work_journey_actual_workers_keep_context_results_and_bindings_separate() {
        let root = std::env::temp_dir().join(format!("wta-parallel-work-{}", uuid::Uuid::new_v4()));
        let (store, mut state) = Store::open(&root, false).unwrap();
        state.enable_clock().unwrap();
        for input in [
            OPEN_PARALLEL,
            GOAL,
            CONFIRM,
            START,
            DIAGNOSE,
            START_DIAGNOSIS,
        ] {
            state.apply(input).unwrap();
        }
        let identities = state
            .works
            .iter()
            .map(|w| w.executor_session_id.clone())
            .collect::<Vec<_>>();
        let mut a = Worker::start_parallel(root.clone(), 1);
        let mut b = Worker::start_diagnosis(root.clone());
        let (mut a_count, mut b_count) = (0, 0);
        while a_count < 3 || b_count < 2 {
            let receipt = tokio::select! {
                r = a.next(), if a_count < 3 => { a_count += 1; r.unwrap() },
                r = b.next(), if b_count < 2 => { b_count += 1; r.unwrap() },
            };
            let before_a = state.works[..3].to_vec();
            let before_b = state.works[3].clone();
            let diagnosis = matches!(receipt.update, Update::Diagnosis { .. });
            assert!(
                !matches!(
                    receipt.update,
                    Update::Failed { .. }
                        | Update::Diagnosis {
                            update: DiagnosisUpdate::Failed { .. }
                        }
                ),
                "{receipt:?}"
            );
            state.receive_release(receipt).unwrap();
            if diagnosis {
                assert_eq!(state.works[..3], before_a);
            } else {
                assert_eq!(state.works[3], before_b);
            }
            let before_question = state.clone();
            state.apply(PROGRESS).unwrap();
            assert_eq!(state.works, before_question.works);
            assert_eq!(state.release_journey, before_question.release_journey);
            store.save(&state).unwrap();
        }
        a.finish().await.unwrap();
        b.finish().await.unwrap();
        assert_eq!(
            state.release_journey.as_ref().unwrap().stage,
            Stage::Blocked
        );
        assert_eq!(state.works[3].status, "Result ready");
        let progress = state.release_progress();
        assert!(progress.contains("Release checks: 1 / 2 passed."));
        assert!(progress.contains("Explain the startup warning: Result ready\nNext: Review"));
        let diagnosis = state.works[3].clone();
        state.apply(REPAIR).unwrap();
        let mut repair = Worker::start_parallel(root.clone(), 2);
        for _ in 0..3 {
            state.receive_release(repair.next().await.unwrap()).unwrap();
            assert_eq!(state.works[3], diagnosis);
            store.save(&state).unwrap();
        }
        repair.finish().await.unwrap();
        assert_eq!(state.release_journey.as_ref().unwrap().stage, Stage::Ready);
        assert_eq!(
            identities,
            state
                .works
                .iter()
                .map(|w| w.executor_session_id.clone())
                .collect::<Vec<_>>()
        );
        for entry in std::fs::read_dir(root.join("release-checks")).unwrap() {
            let request =
                std::fs::read_to_string(entry.unwrap().path().join("request.json")).unwrap();
            assert!(!request.contains("startup"));
            assert!(!request.contains(PROGRESS));
        }
        let directory = std::fs::read_dir(root.join("diagnosis-checks"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let request = std::fs::read_to_string(directory.join("request.json")).unwrap();
        assert!(!request.contains("4821"));
        assert!(!request.contains(PROGRESS));
        assert!(!request.contains("double quotes"));
        assert_eq!(
            std::fs::read_to_string(directory.join("startup-config.json")).unwrap(),
            STARTUP_CONFIG
        );
        let result: Receipt =
            serde_json::from_slice(&std::fs::read(directory.join("result.json")).unwrap()).unwrap();
        assert_eq!(
            state
                .events
                .iter()
                .find_map(|e| e.release_receipt.as_ref().filter(|r| matches!(
                    r.update,
                    Update::Diagnosis {
                        update: DiagnosisUpdate::Completed { .. }
                    }
                )))
                .unwrap(),
            &result
        );
        drop(store);
        let (store, loaded) = Store::open(&root, false).unwrap();
        assert_eq!(state, loaded);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    #[ignore = "Requires Node.js; executes the fixed initial and repaired local fixtures"]
    async fn release_journey_real_checks_fail_then_pass_and_survive_sqlite_reopen() {
        let root = std::env::temp_dir().join(format!("wta-release-{}", uuid::Uuid::new_v4()));
        let (store, mut state) = Store::open(&root, false).unwrap();
        state.enable_clock().unwrap();
        for input in [OPEN, GOAL, CONFIRM, START] {
            state.apply(input).unwrap();
        }
        for attempt in [1, 2] {
            if attempt == 2 {
                state.apply(REPAIR).unwrap();
            }
            store.save(&state).unwrap();
            let mut worker = Worker::start(root.clone(), attempt);
            for _ in 0..3 {
                let receipt = worker.next().await.unwrap();
                assert!(
                    !matches!(receipt.update, Update::Failed { .. }),
                    "{receipt:?}"
                );
                state.receive_release(receipt).unwrap();
                store.save(&state).unwrap();
            }
            worker.finish().await.unwrap();
            assert_eq!(
                state.release_journey.as_ref().unwrap().stage,
                if attempt == 1 {
                    Stage::Blocked
                } else {
                    Stage::Ready
                }
            );
        }
        assert_eq!(
            state.works[0]
                .evidence
                .iter()
                .map(|e| e.exit_code)
                .collect::<Vec<_>>(),
            [0, 1, 0, 0]
        );
        assert!(state.works[0].evidence[1].summary.contains("ERR_ASSERTION"));
        assert!(state.works[0].evidence[3].summary.contains("# pass 1"));
        assert!(state.works[0].evidence[2].summary.contains("# pass 3"));
        drop(store);
        let (store, loaded) = Store::open(&root, false).unwrap();
        assert_eq!(state, loaded);
        drop(store);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
