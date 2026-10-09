use std::io::Write;

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match mdt::cli::run(args.clone()) {
        Ok(output) => match std::io::stdout().lock().write_all(output.as_bytes()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("io: {e}");
                std::process::ExitCode::from(10)
            }
        },
        Err(e) => {
            eprint!("{}", mdt::cli::error_output(&args, &e));
            std::process::ExitCode::from(e.exit_code())
        }
    }
}
