use std::{io::Write, path::PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use ditto_protocol::{ClientCommand, PayloadRef, ServerMessage};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

#[derive(Debug, Parser)]
#[command(name = "ditto", about = "Streaming client for ditto-daemon")]
struct Args {
    #[arg(long, default_value = ".ditto/ditto.sock")]
    socket: PathBuf,

    /// Print context receipts and selected capability cards.
    #[arg(long)]
    inspect: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Run {
        input: String,
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        task: Option<String>,
    },
    Events {
        #[arg(long)]
        session: Option<String>,
        #[arg(long)]
        task: Option<String>,
        #[arg(long, default_value_t = 0)]
        after: i64,
    },
    Cancel {
        #[arg(long)]
        task: String,
    },
    Ping,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let command = match args.command {
        Command::Run {
            input,
            session,
            task,
        } => ClientCommand::Submit {
            input,
            session_id: session,
            task_id: task,
        },
        Command::Events {
            session,
            task,
            after,
        } => ClientCommand::Replay {
            session_id: session,
            task_id: task,
            after_seq: after,
        },
        Command::Cancel { task } => ClientCommand::Cancel { task_id: task },
        Command::Ping => ClientCommand::Ping,
    };

    let stream = UnixStream::connect(&args.socket)
        .await
        .with_context(|| format!("connect to {}", args.socket.display()))?;
    let (read_half, mut write_half) = stream.into_split();
    let encoded = serde_json::to_vec(&command)?;
    write_half.write_all(&encoded).await?;
    write_half.write_all(b"\n").await?;

    let mut lines = BufReader::new(read_half).lines();
    let mut streamed_text = false;
    while let Some(line) = lines.next_line().await? {
        let message: ServerMessage = serde_json::from_str(&line)
            .with_context(|| format!("decode daemon message: {line}"))?;
        match message {
            ServerMessage::Accepted {
                session_id,
                task_id,
            } => eprintln!("session={session_id} task={task_id}"),
            ServerMessage::ContextReceipt { receipt } if args.inspect => {
                eprintln!("\n--- context receipt ---\n{}", receipt.capsule);
                for item in receipt.included {
                    eprintln!("- {} [{}; {}]", item.label, item.epistemic, item.reason);
                }
            }
            ServerMessage::CapabilitiesSelected { capabilities } if args.inspect => {
                eprintln!("\n--- capabilities ---");
                for capability in capabilities {
                    eprintln!("- {} ({:?})", capability.id, capability.maximum_effect);
                }
            }
            ServerMessage::Event { event } if matches!(&command, ClientCommand::Replay { .. }) => {
                println!("{}", serde_json::to_string_pretty(&event)?);
            }
            ServerMessage::Event { event } if args.inspect => {
                let payload = match event.event.payload_ref {
                    PayloadRef::Inline(value) => value.to_string(),
                    PayloadRef::Artifact(reference) => reference,
                    PayloadRef::Empty => "null".to_owned(),
                };
                eprintln!("[{}] {} {payload}", event.seq, event.event.kind);
            }
            ServerMessage::ModelDelta { text } => {
                streamed_text = true;
                print!("{text}");
                std::io::stdout().flush()?;
            }
            ServerMessage::End { verified } => {
                if streamed_text {
                    println!();
                }
                if !verified {
                    eprintln!("completion is unverified");
                }
                break;
            }
            ServerMessage::Error { message } => bail!(message),
            ServerMessage::Pong => {
                println!("pong");
                break;
            }
            ServerMessage::ContextReceipt { .. }
            | ServerMessage::CapabilitiesSelected { .. }
            | ServerMessage::Event { .. } => {}
        }
    }
    Ok(())
}
