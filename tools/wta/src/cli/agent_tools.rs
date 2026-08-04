use anyhow::{Context, Result};
use std::path::Path;
use tokio::io::{AsyncWriteExt, BufReader};

pub(crate) async fn run_command_resolution(
    token: &str,
    shell: &str,
    cwd: Option<&Path>,
    json_mode: bool,
) -> Result<()> {
    let result = crate::agent_tools::command_resolution::resolve(token, shell, cwd).await;
    if json_mode {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "{}",
            crate::agent_tools::command_resolution::format_human(&result)
        );
    }
    Ok(())
}

pub(crate) async fn run_action_proposal(channel: String, payload: String) -> Result<()> {
    let channel = channel
        .parse::<crate::agent_tools::action_proposal::channel::ProposalChannel>()
        .context("invalid --channel")?;
    if payload.len() > crate::agent_tools::action_proposal::schema::MAX_PAYLOAD_BYTES {
        anyhow::bail!(
            "--payload-json exceeds the {}-byte inline limit",
            crate::agent_tools::action_proposal::schema::MAX_PAYLOAD_BYTES
        );
    }
    let pipe = open_pipe(&channel.pipe_name()).await?;
    let (read_half, mut write_half) = tokio::io::split(pipe);
    let request = crate::agent_tools::action_proposal::pipe::ProposalPipeRequest {
        version: crate::agent_tools::action_proposal::pipe::PROTOCOL_VERSION,
        channel: channel.to_string(),
        payload,
    };
    let mut request_line = serde_json::to_vec(&request)?;
    request_line.push(b'\n');
    write_half
        .write_all(&request_line)
        .await
        .context("write proposal request")?;
    write_half.flush().await.context("flush proposal request")?;

    let mut reader = BufReader::new(read_half);
    let validation: crate::agent_tools::action_proposal::pipe::ProposalValidationResponse =
        read_response(&mut reader).await?;
    println!("{}", serde_json::to_string(&validation)?);
    std::io::Write::flush(&mut std::io::stdout()).context("flush validation response")?;
    if validation.status
        != crate::agent_tools::action_proposal::channel::ProposalValidationStatus::Accepted
    {
        return Ok(());
    }

    let final_response: crate::agent_tools::action_proposal::pipe::ProposalFinalResponse =
        read_response(&mut reader).await?;
    println!("{}", serde_json::to_string(&final_response)?);
    Ok(())
}

async fn open_pipe(pipe_name: &str) -> Result<tokio::net::windows::named_pipe::NamedPipeClient> {
    const ERROR_FILE_NOT_FOUND: i32 = 2;
    const ERROR_PIPE_BUSY: i32 = 231;
    const BACKOFF_MS: &[u64] = &[20, 50, 100, 200, 500, 1000];

    for (attempt, wait_ms) in BACKOFF_MS.iter().enumerate() {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(pipe_name) {
            Ok(pipe) => return Ok(pipe),
            Err(error)
                if matches!(
                    error.raw_os_error(),
                    Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)
                ) =>
            {
                tracing::debug!(
                    target: "proposal_cli",
                    pipe = %pipe_name,
                    attempt = attempt + 1,
                    wait_ms,
                    "proposal pipe not ready"
                );
                tokio::time::sleep(std::time::Duration::from_millis(*wait_ms)).await;
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("open owning Helper pipe '{pipe_name}'"));
            }
        }
    }
    anyhow::bail!("owning Helper pipe is unavailable")
}

async fn read_response<R, T>(reader: &mut R) -> Result<T>
where
    R: tokio::io::AsyncBufRead + Unpin,
    T: serde::de::DeserializeOwned,
{
    use tokio::io::AsyncBufReadExt;

    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf().await.context("read proposal response")?;
        if available.is_empty() {
            if line.is_empty() {
                anyhow::bail!("owning Helper disconnected before responding");
            }
            anyhow::bail!("owning Helper response is not newline terminated");
        }
        let take = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if line.len() + take > crate::agent_tools::action_proposal::pipe::MAX_FRAME_BYTES {
            anyhow::bail!("proposal response exceeds the frame limit");
        }
        line.extend_from_slice(&available[..take]);
        reader.consume(take);
        if line.last() == Some(&b'\n') {
            break;
        }
    }
    serde_json::from_slice(&line).context("decode proposal response")
}
