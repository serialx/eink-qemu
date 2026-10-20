//! Read-only native discovery probe. No driver, helper, or network installation.
fn main() {
    match eink_bridge::inspect() {
        Ok(inspection) => println!("{}", serde_json::to_string_pretty(&inspection).unwrap()),
        Err(error) => { eprintln!("{}", serde_json::to_string(&error).unwrap()); std::process::exit(1); }
    }
}
