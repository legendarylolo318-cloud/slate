fn main() {
    if let Err(e) = slint_build::compile("ui/app.slint") {
        eprintln!("{e}");
        std::process::exit(1);
    }
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icons/icon.ico");
        res.set("ProductName", "Slate");
        res.set("FileDescription", "Slate - native TIDAL client");
        res.set("LegalCopyright", "GPL-3.0-only");
        if let Err(e) = res.compile() {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
