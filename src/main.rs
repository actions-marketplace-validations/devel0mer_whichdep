use whichdep::cli::{self, Command};

fn main() {
    let code = match cli::parse(std::env::args().skip(1)) {
        Ok(Command::Run(args)) => whichdep::session::run(args),
        Ok(Command::Adapters { json }) => whichdep::session::run_adapters(json),
        Ok(Command::History {
            experiment_id,
            format,
            out,
        }) => whichdep::session::run_history(experiment_id.as_deref(), format, out.as_deref()),
        Ok(Command::Clean { build_only }) => whichdep::session::run_clean(build_only),
        Ok(Command::Format { input, format, out }) => {
            whichdep::session::run_format(&input, format, out.as_deref())
        }
        Ok(Command::Help) => {
            print!("{}", cli::HELP);
            0
        }
        Ok(Command::Version) => {
            println!("whichdep {}", cli::VERSION);
            0
        }
        Err(msg) => {
            eprintln!("error: {msg}\n\nFor more information try --help");
            2
        }
    };
    std::process::exit(code);
}
