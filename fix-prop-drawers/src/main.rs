use std::{ffi::OsStr, fs, path::PathBuf};

use argh::FromArgs;
use rayon::prelude::*;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

#[derive(FromArgs)]
/// Sync org and gcal.
struct Args {
    #[argh(positional)]
    notes: PathBuf,

    #[argh(switch)]
    /// don't do anything
    dry: bool,
}

fn main() {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let args: Args = argh::from_env();

    walkdir::WalkDir::new(&args.notes)
        .into_iter()
        .par_bridge()
        .filter_map(|entry| {
            let Ok(e) = entry else {
                return None;
            };

            if e.path().extension().and_then(OsStr::to_str) == Some("org") {
                Some(e)
            } else {
                None
            }
        })
        .for_each(|entry| {
            let f = fs::read_to_string(entry.path())
                .expect(&format!("couldn't read {}", entry.path().to_string_lossy()));

            // Remove empty line after
            let new_f = f.clone();
            let new_f = new_f.replace(":PROPERTIES:\n\n", ":PROPERTIES:\n");
            let new_f = new_f.replace("\n\n:END:", "\n:END:");
            let new_f = new_f.replace(":PROPERTIES:\n:END:\n", "");

            if f != new_f {
                println!("{}", entry.path().to_string_lossy());

                if !args.dry {
                    fs::write(entry.path(), new_f).expect(&format!(
                        "couldn't write {}",
                        entry.path().to_string_lossy()
                    ));
                }
            }
        });
}
