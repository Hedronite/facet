use std::{
    env,
    io::{self, Write},
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("tui") {
        return ExitCode::from(facet_cli::run_tui(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("mcp") {
        return ExitCode::from(facet_cli::run_mcp(&args[1..]));
    }
    if args.first().map(String::as_str) == Some("ncl") {
        return facet_cli::ncl_shim::run(&args[1..]);
    }
    let mut stdin = io::stdin().lock();
    let output = facet_cli::run_with_stdin(args, &mut stdin);
    let mut stdout = io::stdout().lock();
    let _ = stdout.write_all(&output.stdout);
    let _ = stdout.flush();
    eprint!("{}", output.stderr);
    ExitCode::from(output.exit_code)
}
