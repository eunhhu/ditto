use std::{collections::HashMap, path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use clap::Parser;
use ditto_artifact_store::ArtifactStore;
use ditto_capability_index::{CapabilityIndex, CapabilityManifest};
use ditto_context_compiler::ContextCompiler;
use ditto_context_graph::ContextGraph;
use ditto_event_store::EventStore;
use ditto_kernel::{Kernel, SubmitRequest};
use ditto_model_driver::DevelopmentDriver;
use ditto_protocol::{ClientCommand, ServerMessage, new_id};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{Mutex, mpsc, oneshot},
    task::AbortHandle,
};

#[derive(Debug, Parser)]
#[command(about = "Ditto semantic agent microkernel daemon")]
struct Args {
    /// Directory containing state.db, objects, and the default socket.
    #[arg(long, default_value = ".ditto")]
    data_dir: PathBuf,

    /// Override Unix socket path.
    #[arg(long)]
    socket: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct RunningTask {
    abort: AbortHandle,
    session_id: String,
    updates: mpsc::UnboundedSender<ServerMessage>,
}

type RunningTasks = Arc<Mutex<HashMap<String, RunningTask>>>;

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.data_dir)
        .with_context(|| format!("create data directory {}", args.data_dir.display()))?;
    let socket_path = args
        .socket
        .unwrap_or_else(|| args.data_dir.join("ditto.sock"));
    prepare_socket(&socket_path).await?;

    let kernel = Arc::new(Kernel::new(
        EventStore::open(args.data_dir.join("state.db"))?,
        ArtifactStore::open(args.data_dir.join("objects"))?,
        ContextCompiler::default(),
        ContextGraph::default(),
        CapabilityIndex::new([CapabilityManifest::device_process_run()]),
        Arc::new(DevelopmentDriver::default()),
    ));
    let tasks = RunningTasks::default();
    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("bind socket {}", socket_path.display()))?;
    println!("ditto-daemon listening on {}", socket_path.display());
    println!("model driver: development fixture (configure frontier provider in next slice)");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.context("accept client")?;
                let kernel = Arc::clone(&kernel);
                let tasks = Arc::clone(&tasks);
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, kernel, tasks).await {
                        eprintln!("client connection failed: {error:#}");
                    }
                });
            }
            signal = tokio::signal::ctrl_c() => {
                signal.context("listen for ctrl-c")?;
                break;
            }
        }
    }

    drop(listener);
    if socket_path.exists() {
        std::fs::remove_file(&socket_path)
            .with_context(|| format!("remove socket {}", socket_path.display()))?;
    }
    Ok(())
}

async fn prepare_socket(path: &PathBuf) -> Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    if !path.exists() {
        return Ok(());
    }
    if UnixStream::connect(path).await.is_ok() {
        bail!("another daemon is already listening on {}", path.display());
    }
    std::fs::remove_file(path)
        .with_context(|| format!("remove stale socket {}", path.display()))?;
    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    kernel: Arc<Kernel>,
    tasks: RunningTasks,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let (updates, mut update_receiver) = mpsc::unbounded_channel::<ServerMessage>();
    let writer = tokio::spawn(async move {
        while let Some(message) = update_receiver.recv().await {
            let encoded = serde_json::to_vec(&message)?;
            if write_half.write_all(&encoded).await.is_err()
                || write_half.write_all(b"\n").await.is_err()
            {
                break;
            }
        }
        Ok::<_, serde_json::Error>(())
    });

    while let Some(line) = lines.next_line().await.context("read client command")? {
        let command: ClientCommand = match serde_json::from_str(&line) {
            Ok(command) => command,
            Err(error) => {
                send(
                    &updates,
                    ServerMessage::Error {
                        message: format!("invalid command: {error}"),
                    },
                );
                continue;
            }
        };
        handle_command(command, &kernel, &tasks, &updates).await;
    }

    drop(updates);
    writer.abort();
    Ok(())
}

async fn handle_command(
    command: ClientCommand,
    kernel: &Arc<Kernel>,
    tasks: &RunningTasks,
    updates: &mpsc::UnboundedSender<ServerMessage>,
) {
    match command {
        ClientCommand::Submit {
            input,
            session_id,
            task_id,
        } => {
            let session_id = session_id.unwrap_or_else(|| new_id("session"));
            let task_id = task_id.unwrap_or_else(|| new_id("task"));
            spawn_turn(
                Arc::clone(kernel),
                Arc::clone(tasks),
                updates.clone(),
                SubmitRequest {
                    input,
                    session_id: Some(session_id),
                    task_id: Some(task_id),
                },
            )
            .await;
        }
        ClientCommand::Replay {
            session_id,
            task_id,
            after_seq,
        } => match kernel.replay(session_id.as_deref(), task_id.as_deref(), after_seq) {
            Ok(events) => {
                for event in events {
                    send(updates, ServerMessage::Event { event });
                }
                let verified = task_id
                    .as_deref()
                    .and_then(|id| kernel.task_ledger(id).ok())
                    .is_some_and(|ledger| ledger.status == ditto_task_state::TaskStatus::Completed);
                send(updates, ServerMessage::End { verified });
            }
            Err(error) => send_error(updates, error),
        },
        ClientCommand::Cancel { task_id } => cancel_task(kernel, tasks, updates, &task_id).await,
        ClientCommand::Ping => send(updates, ServerMessage::Pong),
    }
}

async fn cancel_task(
    kernel: &Kernel,
    tasks: &RunningTasks,
    updates: &mpsc::UnboundedSender<ServerMessage>,
    task_id: &str,
) {
    let Some(task) = tasks.lock().await.remove(task_id) else {
        send(
            updates,
            ServerMessage::Error {
                message: format!("task is not running: {task_id}"),
            },
        );
        return;
    };
    task.abort.abort();
    match kernel.record_cancel(Some(&task.session_id), task_id) {
        Ok(event) => {
            send(
                &task.updates,
                ServerMessage::Event {
                    event: event.clone(),
                },
            );
            if !task.updates.same_channel(updates) {
                send(updates, ServerMessage::Event { event });
            }
        }
        Err(error) => send_error(updates, error),
    }
    send(&task.updates, ServerMessage::End { verified: false });
    if !task.updates.same_channel(updates) {
        send(updates, ServerMessage::End { verified: false });
    }
}

async fn spawn_turn(
    kernel: Arc<Kernel>,
    tasks: RunningTasks,
    updates: mpsc::UnboundedSender<ServerMessage>,
    request: SubmitRequest,
) {
    let session_id = request
        .session_id
        .clone()
        .expect("daemon assigned session id");
    let task_id = request.task_id.clone().expect("daemon assigned task id");
    let task_id_for_job = task_id.clone();
    let session_id_for_job = session_id.clone();
    let tasks_for_job = Arc::clone(&tasks);
    let kernel_for_job = Arc::clone(&kernel);
    let updates_for_job = updates.clone();
    let (start_sender, start_receiver) = oneshot::channel();
    let job = tokio::spawn(async move {
        let _ = start_receiver.await;
        if let Err(error) = kernel_for_job
            .run_turn(request, updates_for_job.clone())
            .await
        {
            if let Ok(event) = kernel_for_job.record_failure(
                Some(&session_id_for_job),
                &task_id_for_job,
                &error.to_string(),
            ) {
                send(&updates_for_job, ServerMessage::Event { event });
            }
            send_error(&updates_for_job, error);
            send(&updates_for_job, ServerMessage::End { verified: false });
        }
        tasks_for_job.lock().await.remove(&task_id_for_job);
    });
    tasks.lock().await.insert(
        task_id,
        RunningTask {
            abort: job.abort_handle(),
            session_id,
            updates,
        },
    );
    let _ = start_sender.send(());
}

fn send(updates: &mpsc::UnboundedSender<ServerMessage>, message: ServerMessage) {
    let _ = updates.send(message);
}

fn send_error(updates: &mpsc::UnboundedSender<ServerMessage>, error: impl std::fmt::Display) {
    send(
        updates,
        ServerMessage::Error {
            message: error.to_string(),
        },
    );
}
