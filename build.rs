fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint build failed");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        embed_windows_resources();
    }
}

#[cfg(windows)]
fn embed_windows_resources() {
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "SKJ Flash");
    res.set("FileDescription", "SKJ Flash");
    res.set("CompanyName", "SKJ Tech");
    let _ = res.compile();
}

#[cfg(not(windows))]
fn embed_windows_resources() {}
