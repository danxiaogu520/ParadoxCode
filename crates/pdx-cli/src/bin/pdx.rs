fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match pdx_cli::execute_pdx(&args) {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("pdx: {error}");
            std::process::exit(error.exit_code());
        }
    }
}
