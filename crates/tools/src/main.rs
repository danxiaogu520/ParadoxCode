//! Dispatches one repository tooling command and prints its result to stdout.

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match tools::cli::execute(&args) {
        Ok(output) => {
            println!("{output}");
        }
        Err(error) => {
            eprintln!("tools: {error}");
            std::process::exit(1);
        }
    }
}
