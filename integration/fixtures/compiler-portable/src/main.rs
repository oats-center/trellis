fn main() -> Result<(), Box<dyn std::error::Error>> {
    let value: compiler_fixture::types::Value =
        serde_json::from_str(r#"{"value":"relocated compiler"}"#)?;
    let wire = serde_json::to_value(value)?;
    assert_eq!(wire["value"], "relocated compiler");
    Ok(())
}
