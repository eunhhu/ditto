//! Usage: cargo run -p ditto-capability --example package_header -- path/to/capability.toml
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("expected one capability.toml path")?;
    if args.next().is_some() {
        return Err("expected one capability.toml path".into());
    }
    print!("{}", ditto_capability::generate_package_header(path)?);
    Ok(())
}
