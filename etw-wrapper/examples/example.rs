//! Demonstrates how to generate and use an ETW wrapper.
//!
//! This example does not build or register the message and metadata tables, so the capture file
//! only indicates that the events were emitted.

use etw_wrapper::{FILETIME, gen_etw_wrapper};

// The path is resolved relative to this crate's manifest directory
// By default each provider generates a PascalCaseSymbolLogger struct
// Here ProviderWidgetserviceLogger is overridden with the name WidgetLogger
gen_etw_wrapper!("manifests/widgetservice.man", PROVIDER_WIDGETSERVICE -> WidgetLogger);

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let provider = WidgetLogger::register()?;

    println!("provider registered.");
    println!("start a session, then press Enter to emit:");
    println!(
        "  tracelog -start Example -guid \"#8B3A1F42-6C7D-4E9A-9F21-3D5E0A7C1B84\" -f Example.etl"
    );
    std::io::stdin().read_line(&mut String::new())?;

    let start_time = FILETIME {
        dwLowDateTime: 0,
        dwHighDateTime: 0,
    };
    provider.service_started("1.0.0", 8, start_time)?;
    provider.request_failed(0x12ABCDEF, 500, 42, "failed to succeed")?;

    println!("events emitted. run: tracelog -stop Example");
    Ok(())
}
