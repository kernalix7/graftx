//! GraftX demo command-line client.
//!
//! A thin wrapper around [`graftx_client`] that shows the session bring-up over
//! a real socket: `graftx connect <addr>` opens a [`TcpStream`], wraps it in a
//! [`StreamTransport`], runs [`graftx_client::handshake`], prints the negotiated
//! [`Welcome`](graftx_protocol::Welcome), and exits. With no subcommand it
//! prints usage and the protocol version this build speaks.
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

/// Exit code returned when a `connect` attempt fails (network or handshake).
const EXIT_CONNECT: u8 = 1;

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
/// `connect` fails, and [`EXIT_USAGE`] for a missing address or unknown
/// subcommand — keeping a usage error distinct from a runtime failure.
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
    fn usage_banner_carries_protocol_version() {
        let (major, minor) = graftx_client::protocol_version();
        let mut buf = Vec::new();
        print_usage(&mut buf);
        let text = String::from_utf8(buf).expect("usage banner is utf-8");
        assert!(text.contains(&format!("protocol {major}.{minor}")));
        assert!(text.contains("connect <addr>"));
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
