//! `hwahap` — a local STDIO MCP server.
//!
//! There is no daemon or HTTP endpoint. The optional usage command manages local measurements.
//! The host starts the default process,
//! speaks MCP over stdin and stdout, and stops it. Everything durable lives in the repository's
//! `.hwahap/` directory, so a restart loses nothing.

use hwahap::mcp::Hwahap;
use rmcp::transport::stdio;
use rmcp::ServiceExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.as_slice() == ["--version"] {
        println!("hwahap {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.first().is_some_and(|arg| arg == "usage") {
        println!(
            "{}",
            serde_json::to_string_pretty(&hwahap::cost::usage_command(&args[1..])?)?
        );
        return Ok(());
    }
    if !args.is_empty() {
        return Err("unknown command; use usage or start without arguments for MCP".into());
    }
    // stdout belongs to the MCP transport. Anything Hwahap wants a human to see goes to stderr,
    // and anything it wants to keep goes into `.hwahap/`.
    let server = Hwahap::new();
    let service = server.clone().serve(stdio()).await?;

    let token = service.cancellation_token();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            token.cancel();
        }
    });

    let quit_reason = service.waiting().await;
    server.shutdown().await;
    let quit_reason = quit_reason?;
    eprintln!("hwahap exited: {quit_reason:?}");

    // `tokio::io::stdin()` parks a blocking-pool thread in a `read` that only returns at EOF, and
    // dropping the runtime waits for that pool. After a cancellation the read is still parked, so
    // returning from main here hangs forever. Exiting explicitly is what actually leaves no
    // process behind, which is one of Hwahap's stated guarantees.
    std::process::exit(0);
}
