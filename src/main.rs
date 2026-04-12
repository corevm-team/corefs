use corefs::cli;

fn main() {
    if let Err(error) = cli::run(std::env::args()) {
        eprintln!("corefs error: {error}");
        std::process::exit(1);
    }
}
