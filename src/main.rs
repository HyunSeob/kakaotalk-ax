//! `kax` — Single native CLI and MCP stdio server for KakaoTalk on macOS and Windows.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{json, Map, Value};

use kakaotalk_ax::backend::create_live_backend;
use kakaotalk_ax::error::KaxError;
use kakaotalk_ax::mcp::{dispatch_method, McpServer};
use kakaotalk_ax::state::{
    default_send_state_root, legacy_send_state_root, migrate_legacy_journal,
};

#[derive(Parser)]
#[command(
    name = "kax",
    version,
    about = "Native KakaoTalk operations on macOS and Windows via CLI and MCP."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Clone, Copy, ValueEnum)]
enum BuildKind {
    Send,
    Read,
    Control,
}

impl BuildKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Read => "read",
            Self::Control => "control",
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
#[value(rename_all = "snake_case")]
enum AxMethod {
    Windows,
    Rooms,
    SearchRooms,
    OpenRoom,
    CloseRoom,
    ReadMessages,
    ReadAttachment,
    ReadMany,
    LoadMore,
    SearchMessages,
    CloseSearch,
    SendMessage,
    SendResult,
}

impl AxMethod {
    fn as_str(self) -> &'static str {
        match self {
            Self::Windows => "windows",
            Self::Rooms => "rooms",
            Self::SearchRooms => "search_rooms",
            Self::OpenRoom => "open_room",
            Self::CloseRoom => "close_room",
            Self::ReadMessages => "read_messages",
            Self::ReadAttachment => "read_attachment",
            Self::ReadMany => "read_many",
            Self::LoadMore => "load_more",
            Self::SearchMessages => "search_messages",
            Self::CloseSearch => "close_search",
            Self::SendMessage => "send_message",
            Self::SendResult => "send_result",
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// Serve the AX tools over MCP stdio
    Mcp,
    /// Verify platform readiness (Windows OCR/app check; native macOS binary is pre-built)
    Build {
        #[arg(long, value_enum, default_value = "send")]
        kind: BuildKind,
    },
    /// Invoke one of the 13 KakaoTalk AX methods with an optional UTF-8 JSON args file
    #[command(name = "ax-call")]
    AxCall {
        #[arg(value_enum)]
        method: AxMethod,
        #[arg(long = "args-file")]
        args_file: Option<PathBuf>,
    },
    /// List open KakaoTalk chat windows
    #[command(name = "ax-windows")]
    AxWindows,
    /// Alias for ax-windows
    Status,
    /// Read messages from an open KakaoTalk chat window
    #[command(name = "ax-read")]
    AxRead {
        #[arg(long = "window-title")]
        window_title: String,
        #[arg(long, default_value_t = 50, allow_hyphen_values = true)]
        limit: i64,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        offset: i64,
        #[arg(long = "snapshot-id", default_value = "")]
        snapshot_id: String,
    },
    /// Migrate legacy Python v1 send journals one-way into the Rust v2 journal directory
    #[command(name = "migrate-journal")]
    MigrateJournal {
        #[arg(long)]
        from: Option<PathBuf>,
        #[arg(long)]
        to: Option<PathBuf>,
    },
}

fn read_args_file(path: &PathBuf) -> Result<Map<String, Value>, KaxError> {
    let bytes = std::fs::read(path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            KaxError::runtime("FileNotFoundError", err.to_string())
        } else {
            KaxError::from(err)
        }
    })?;
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    let raw = std::str::from_utf8(bytes)
        .map_err(|err| KaxError::runtime("UnicodeDecodeError", err.to_string()))?;
    let parsed: Value = serde_json::from_str(raw)
        .map_err(|err| KaxError::runtime("JSONDecodeError", err.to_string()))?;
    match parsed {
        Value::Object(map) => Ok(map),
        _ => Err(KaxError::validation("AX arguments must be a JSON object")),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    if matches!(cli.command, Command::Mcp) {
        let backend = create_live_backend();
        let server = McpServer::new(backend.as_ref());
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        return match server.serve(stdin.lock(), stdout.lock()) {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::FAILURE,
        };
    }

    if let Command::MigrateJournal { from, to } = cli.command {
        let src = from.unwrap_or_else(legacy_send_state_root);
        let dst = to.unwrap_or_else(default_send_state_root);
        match migrate_legacy_journal(&src, &dst) {
            Ok(report) => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_string())
                );
                return ExitCode::SUCCESS;
            }
            Err(err) => {
                println!(
                    "{}",
                    serde_json::to_string(&json!({
                        "status": "failed",
                        "errorType": err.error_type(),
                        "detail": err.message(),
                    }))
                    .unwrap_or_default()
                );
                return ExitCode::FAILURE;
            }
        }
    }

    let is_read_cmd = matches!(
        cli.command,
        Command::AxRead { .. } | Command::AxWindows | Command::Status
    );
    let mut sending_operation: Option<String> = None;

    let result: Result<Value, KaxError> = (|| {
        let backend = create_live_backend();
        match cli.command {
            Command::AxCall { method, args_file } => {
                let args = match args_file {
                    Some(ref path) => read_args_file(path)?,
                    None => Map::new(),
                };
                if matches!(method, AxMethod::SendMessage) {
                    if let Some(op) = args.get("operation_id").and_then(|v| v.as_str()) {
                        sending_operation = Some(op.to_string());
                    }
                }
                dispatch_method(backend.as_ref(), method.as_str(), &args)
            }
            Command::AxWindows | Command::Status => backend.windows(),
            Command::AxRead {
                window_title,
                limit,
                offset,
                snapshot_id,
            } => backend.read_messages(&window_title, limit, offset, &snapshot_id),
            Command::Build { kind } => backend.build(kind.as_str()),
            Command::Mcp | Command::MigrateJournal { .. } => unreachable!(),
        }
    })();

    match result {
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
            );
            let status = value.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if matches!(status, "ok" | "built" | "partial" | "observed_in_ax") {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(err) => {
            let detail = match &err {
                KaxError::Validation(msg) => msg.clone(),
                KaxError::Runtime { error_type, detail }
                    if matches!(error_type.as_str(), "JSONDecodeError" | "UnicodeDecodeError") =>
                {
                    detail.clone()
                }
                _ => {
                    if is_read_cmd {
                        "AX read failed".to_string()
                    } else {
                        "Operation failed; inspect saved result before retrying".to_string()
                    }
                }
            };
            let mut obj = Map::new();
            if let Some(op_id) = sending_operation {
                obj.insert("status".to_string(), json!("outcome_unknown"));
                obj.insert("operationId".to_string(), json!(op_id));
                obj.insert("retrySafe".to_string(), json!(false));
            } else {
                obj.insert("status".to_string(), json!("failed"));
            }
            obj.insert("errorType".to_string(), json!(err.error_type()));
            obj.insert("detail".to_string(), json!(detail));
            println!("{}", serde_json::to_string(&Value::Object(obj)).unwrap_or_default());
            ExitCode::FAILURE
        }
    }
}
