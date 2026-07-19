fn main() {
    match kmux::run() {
        Ok(0) => {}
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("kmux: {error:#}");
            std::process::exit(1);
        }
    }
}
