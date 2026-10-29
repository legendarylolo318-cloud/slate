fn main() {
    // Icons and images referenced from .slint files go inside the binary, so
    // packaged builds never look for them on the build machine's paths.
    let config = slint_build::CompilerConfiguration::new()
        .embed_resources(slint_build::EmbedResourcesKind::EmbedFiles);
    if let Err(e) = slint_build::compile_with_config("ui/app.slint", config) {
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
