use std::path::PathBuf;

use argh::FromArgs;
use color_eyre::eyre::Result;
use tracing::{debug, info};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

mod gcal;
mod org;

#[derive(FromArgs)]
/// Sync org and gcal.
struct Args {
    #[argh(positional)]
    path: PathBuf,

    #[argh(option)]
    /// name (summary) of target calendar
    calendar: String,

    #[argh(option)]
    /// credential path
    creds: PathBuf,

    #[argh(option)]
    /// token path
    token: PathBuf,

    #[argh(switch)]
    /// don't actually modify gcal
    dry: bool,

    #[argh(switch)]
    /// print error to stdout
    show_err: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let args: Args = argh::from_env();

    let before_items = jiff::Timestamp::now();
    let items = org::get_valid_items(args.path);
    let after_items = jiff::Timestamp::now();

    info!("{} items", items.len());
    for item in &items {
        debug!("{} {:?}", item.name, item.timestamps);
    }

    if !args.dry {
        let client = match gcal::get_client(args.creds, args.token).await {
            Ok(o) => o,
            Err(e) => {
                println!("✗ err");
                if args.show_err {
                    return Err(e);
                } else {
                    return Ok(());
                }
            }
        };

        let before_sync = jiff::Timestamp::now();
        match gcal::sync(client, items, &args.calendar).await {
            Ok(()) => {}
            Err(e) => {
                println!("✗ err");
                if args.show_err {
                    return Err(e);
                } else {
                    return Ok(());
                }
            }
        }
        let after_sync = jiff::Timestamp::now();

        println!("---");
        println!("parsed org files in {:#}", after_items - before_items);
        println!("updated gcal in {:#}", after_sync - before_sync);
        println!("finished in {:#}", after_sync - before_items);
        println!(
            "it is {}",
            jiff::fmt::strtime::format("%b %-d %-I:%M%P", &jiff::Zoned::now()).unwrap()
        );
    } else {
        println!("✓ {}", items.len());
        println!("---");
        println!("parsed org files in {:#}", after_items - before_items);
        println!(
            "it is {}",
            jiff::fmt::strtime::format("%b %-d %-I:%M%P", &jiff::Zoned::now()).unwrap()
        );
    }

    Ok(())
}
