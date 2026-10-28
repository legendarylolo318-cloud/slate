fn main() {
    if let Err(e) = slint_build::compile("ui/app.slint") {
        eprintln!("{e}");
        std::process::exit(1);
    }
}
