fn main() {
    if let Err(error) = kmux::run() {
        eprintln!("kmux: {error:#}");
        std::process::exit(1);
    }
}
