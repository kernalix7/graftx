//! GraftX demo command-line client.
//!
//! A thin wrapper around [`graftx_client`] that shows the session bring-up over
//! a real socket: `graftx connect <addr>` opens a [`TcpStream`], wraps it in a
//! [`StreamTransport`], runs [`graftx_client::handshake`], prints the negotiated
//! [`Welcome`](graftx_protocol::Welcome), and exits. `graftx noop <addr>` does
//! the same bring-up and then issues a single [`graftx_client::noop`] to confirm
//! the pipe round-trips, printing `ok`. `graftx apis` prints the API namespace
//! table (the high byte of every opcode) as a markdown table without touching
//! the network. With no subcommand it prints usage and the protocol version this
//! build speaks.
//!
//! Deliberately depends only on the protocol, transport, and client crates —
//! never the server — so the CLI stays a pure client.
#![forbid(unsafe_op_in_unsafe_fn)]

use std::io::{self, Write};
use std::net::TcpStream;
use std::process::ExitCode;

use graftx_transport::StreamTransport;

/// Exit code returned for an unknown or malformed invocation.
const EXIT_USAGE: u8 = 2;

/// Exit code returned when a `connect` or `noop` attempt fails (network,
/// handshake, or the no-op round-trip itself).
const EXIT_CONNECT: u8 = 1;

/// `req_id`/`seq` used for the post-handshake no-op. The handshake spends 1, so
/// the first follow-up request on the session is 2.
const NOOP_REQ_ID: u32 = 2;
const NOOP_SEQ: u64 = 2;

/// The API-namespace table the `apis` subcommand prints: the `(id, name)` pairs
/// occupying the high byte of every opcode.
///
/// Held locally rather than reflected off `graftx_protocol::ApiId` so the CLI
/// stays a thin client with nothing to enumerate. The ids must track that enum
/// (Core = 0x00 .. Amf = 0x0B); [`apis_table_matches_protocol`] guards the match.
const API_IDS: &[(u8, &str)] = &[
    (0x00, "Core"),
    (0x01, "Vulkan"),
    (0x02, "OpenGl"),
    (0x03, "Cuda"),
    (0x04, "OpenCl"),
    (0x05, "Hip"),
    (0x06, "LevelZero"),
    (0x07, "Video"),
    (0x08, "WebGpu"),
    (0x09, "OptiX"),
    (0x0A, "Sycl"),
    (0x0B, "Amf"),
];

fn main() -> ExitCode {
    // Skip argv[0] (the binary path); the dispatcher only cares about the
    // subcommand and its arguments.
    let args: Vec<String> = std::env::args().skip(1).collect();
    run(&args)
}

/// Dispatch a single CLI invocation.
///
/// `args` is the argument list *without* the program name. The first element
/// selects the subcommand; with no arguments we print usage and exit cleanly.
///
/// Returns [`ExitCode::SUCCESS`] for usage/help, [`EXIT_CONNECT`] when a
/// `connect` or `noop` fails, and [`EXIT_USAGE`] for a missing address or
/// unknown subcommand — keeping a usage error distinct from a runtime failure.
fn run(args: &[String]) -> ExitCode {
    let command = args.first().map(String::as_str);

    match command {
        None | Some("help") | Some("-h") | Some("--help") => {
            print_usage(&mut io::stdout());
            ExitCode::SUCCESS
        }
        Some("connect") => match args.get(1) {
            Some(addr) => connect(addr, &mut io::stdout()),
            None => {
                eprintln!("graftx: connect requires an <addr> (e.g. 127.0.0.1:7000)");
                ExitCode::from(EXIT_USAGE)
            }
        },
        Some("noop") => match args.get(1) {
            Some(addr) => noop(addr, &mut io::stdout()),
            None => {
                eprintln!("graftx: noop requires an <addr> (e.g. 127.0.0.1:7000)");
                ExitCode::from(EXIT_USAGE)
            }
        },
        Some("apis") => {
            print_apis(&mut io::stdout());
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("graftx: unknown subcommand `{other}`");
            print_usage(&mut io::stderr());
            ExitCode::from(EXIT_USAGE)
        }
    }
}

/// Connect to `addr`, perform the opening handshake, and print the `Welcome`.
///
/// Diagnostics go to stderr; the negotiated parameters are written to `out`.
fn connect<W: Write>(addr: &str, out: &mut W) -> ExitCode {
    let stream = match TcpStream::connect(addr) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("graftx: connect to {addr} failed: {err}");
            return ExitCode::from(EXIT_CONNECT);
        }
    };

    let mut transport = StreamTransport::new(stream);
    let welcome = match graftx_client::handshake(&mut transport) {
        Ok(welcome) => welcome,
        Err(err) => {
            eprintln!("graftx: handshake with {addr} failed: {err}");
            return ExitCode::from(EXIT_CONNECT);
        }
    };

    if let Err(err) = print_welcome(addr, &welcome, out) {
        eprintln!("graftx: writing output failed: {err}");
        return ExitCode::from(EXIT_CONNECT);
    }
    ExitCode::SUCCESS
}

/// Connect to `addr`, handshake, then issue a single no-op round-trip.
///
/// Prints `ok` to `out` on success; diagnostics go to stderr. Any network,
/// handshake, or no-op failure yields [`EXIT_CONNECT`].
fn noop<W: Write>(addr: &str, out: &mut W) -> ExitCode {
    let stream = match TcpStream::connect(addr) {
        Ok(stream) => stream,
        Err(err) => {
            eprintln!("graftx: connect to {addr} failed: {err}");
            return ExitCode::from(EXIT_CONNECT);
        }
    };

    let mut transport = StreamTransport::new(stream);
    if let Err(err) = graftx_client::handshake(&mut transport) {
        eprintln!("graftx: handshake with {addr} failed: {err}");
        return ExitCode::from(EXIT_CONNECT);
    }

    if let Err(err) = graftx_client::noop(&mut transport, NOOP_REQ_ID, NOOP_SEQ) {
        eprintln!("graftx: noop to {addr} failed: {err}");
        return ExitCode::from(EXIT_CONNECT);
    }

    if let Err(err) = writeln!(out, "ok") {
        eprintln!("graftx: writing output failed: {err}");
        return ExitCode::from(EXIT_CONNECT);
    }
    ExitCode::SUCCESS
}

/// Render a [`Welcome`](graftx_protocol::Welcome) as the connection summary.
fn print_welcome<W: Write>(
    addr: &str,
    welcome: &graftx_protocol::Welcome,
    out: &mut W,
) -> io::Result<()> {
    writeln!(out, "connected to {addr}")?;
    writeln!(
        out,
        "  protocol:       {}.{}",
        welcome.proto_major, welcome.proto_minor
    )?;
    writeln!(out, "  session id:     {}", welcome.session_id)?;
    writeln!(out, "  features:       {:#010x}", welcome.features)?;
    writeln!(out, "  max frame body: {} bytes", welcome.max_frame_body)
}

/// Print the [`API_IDS`] table as a `| ApiId | API |` markdown table.
///
/// Best-effort, like [`print_usage`]: the listing is informational, so a broken
/// pipe on `out` shouldn't disturb the exit code the caller already chose.
fn print_apis<W: Write>(out: &mut W) {
    let _ = writeln!(out, "| ApiId | API |");
    let _ = writeln!(out, "| ----- | --- |");
    for (id, name) in API_IDS {
        let _ = writeln!(out, "| {id:#04x} | {name} |");
    }
}

/// Print the usage banner and the protocol version this build speaks.
fn print_usage<W: Write>(out: &mut W) {
    let (major, minor) = graftx_client::protocol_version();
    // Best-effort: usage output is informational, so a broken pipe on stdout
    // shouldn't change the exit code the caller already decided on.
    let _ = writeln!(
        out,
        "graftx — GraftX demo client (protocol {major}.{minor})"
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "USAGE:");
    let _ = writeln!(
        out,
        "    graftx connect <addr>    connect, handshake, print the Welcome"
    );
    let _ = writeln!(
        out,
        "    graftx noop <addr>       connect, handshake, run one no-op, print ok"
    );
    let _ = writeln!(
        out,
        "    graftx apis              print the API namespace table"
    );
    let _ = writeln!(out, "    graftx help              show this message");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_args_prints_usage_and_succeeds() {
        assert_eq!(run(&[]), ExitCode::SUCCESS);
    }

    #[test]
    fn help_succeeds() {
        for flag in ["help", "-h", "--help"] {
            assert_eq!(run(&args(&[flag])), ExitCode::SUCCESS);
        }
    }

    #[test]
    fn unknown_subcommand_is_usage_error() {
        assert_eq!(run(&args(&["bogus"])), ExitCode::from(EXIT_USAGE));
    }

    #[test]
    fn connect_without_addr_is_usage_error() {
        assert_eq!(run(&args(&["connect"])), ExitCode::from(EXIT_USAGE));
    }

    #[test]
    fn noop_without_addr_is_usage_error() {
        assert_eq!(run(&args(&["noop"])), ExitCode::from(EXIT_USAGE));
    }

    #[test]
    fn usage_banner_lists_noop_subcommand() {
        let mut buf = Vec::new();
        print_usage(&mut buf);
        let text = String::from_utf8(buf).expect("usage banner is utf-8");
        assert!(text.contains("noop <addr>"));
    }

    #[test]
    fn usage_banner_carries_protocol_version() {
        let (major, minor) = graftx_client::protocol_version();
        let mut buf = Vec::new();
        print_usage(&mut buf);
        let text = String::from_utf8(buf).expect("usage banner is utf-8");
        assert!(text.contains(&format!("protocol {major}.{minor}")));
        assert!(text.contains("connect <addr>"));
    }

    #[test]
    fn apis_subcommand_succeeds() {
        assert_eq!(run(&args(&["apis"])), ExitCode::SUCCESS);
    }

    #[test]
    fn apis_table_is_markdown_with_first_and_last_rows() {
        let mut buf = Vec::new();
        print_apis(&mut buf);
        let text = String::from_utf8(buf).expect("apis table is utf-8");
        assert!(text.contains("| ApiId | API |"));
        assert!(text.contains("| 0x00 | Core |"));
        assert!(text.contains("| 0x0b | Amf |"));
    }

    #[test]
    fn usage_banner_lists_apis_subcommand() {
        let mut buf = Vec::new();
        print_usage(&mut buf);
        let text = String::from_utf8(buf).expect("usage banner is utf-8");
        assert!(text.contains("graftx apis"));
    }

    #[test]
    fn apis_table_matches_protocol() {
        use graftx_protocol::ApiId;

        // The local table mirrors `ApiId`; pin both ends and the length so a new
        // variant in the protocol crate forces this list to be updated too.
        assert_eq!(API_IDS.first(), Some(&(ApiId::Core as u8, "Core")));
        assert_eq!(API_IDS.last(), Some(&(ApiId::Amf as u8, "Amf")));
        assert_eq!(
            API_IDS.len(),
            (ApiId::Amf as usize) - (ApiId::Core as usize) + 1
        );

        // Ids are contiguous and ascending from Core.
        for (offset, (id, _name)) in API_IDS.iter().enumerate() {
            assert_eq!(usize::from(*id), (ApiId::Core as usize) + offset);
        }
    }

    #[test]
    fn welcome_summary_includes_negotiated_fields() {
        let welcome = graftx_protocol::Welcome {
            proto_major: 3,
            proto_minor: 7,
            features: 0x5,
            max_frame_body: 4096,
            session_id: 42,
        };
        let mut buf = Vec::new();
        print_welcome("198.51.100.1:7000", &welcome, &mut buf).expect("write summary");
        let text = String::from_utf8(buf).expect("summary is utf-8");
        assert!(text.contains("connected to 198.51.100.1:7000"));
        assert!(text.contains("3.7"));
        assert!(text.contains("42"));
        assert!(text.contains("4096"));
    }
}
