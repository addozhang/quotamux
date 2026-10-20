//! Prepares a QuotaMux data directory for an older binary after a statistics
//! schema upgrade.
//!
//! An upgrade replaces the derived rollup tables with a newer stored value type,
//! which an older binary refuses to open. This tool drops only those derived
//! tables and rewrites the schema marker, so the older binary rebuilds its own
//! rollups from the untouched request and attempt ledger. No request, attempt,
//! circuit or state record is modified.
//!
//! Run it with the container stopped. It refuses to run without `--confirm`.

use std::path::PathBuf;

use clap::Parser;
use quotamux::store::Store;

#[derive(Parser, Debug)]
#[command(about = "Reset derived statistics so an older QuotaMux binary can serve the same ledger")]
struct Args {
    /// Data directory holding quotamux.redb.
    #[arg(long, env = "QUOTAMUX_DATA_DIR", default_value = "./data")]
    data_dir: PathBuf,

    /// Statistics schema version the older binary expects.
    #[arg(long, default_value_t = 1)]
    schema_version: u32,

    /// Required acknowledgement: derived statistics are dropped and rebuilt by the older binary.
    #[arg(long)]
    confirm: bool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();
    if !args.confirm {
        return Err(
            "refusing to run without --confirm: this drops the derived statistics tables (the request and attempt ledger is kept)".into(),
        );
    }
    let store = Store::open(&args.data_dir)?;
    let before = store.ledger_counts()?;
    store.downgrade_stats_schema(args.schema_version)?;
    let after = store.ledger_counts()?;
    if before != after {
        return Err(
            format!("ledger size changed during the downgrade: {before:?} -> {after:?}").into(),
        );
    }
    println!(
        "statistics schema set to {}; ledger kept at {} requests and {} attempts",
        args.schema_version, after.0, after.1
    );
    Ok(())
}
