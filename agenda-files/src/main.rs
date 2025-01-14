use std::{ffi::OsStr, fs, path::PathBuf};

use argh::FromArgs;
use color_eyre::eyre::Result;
use orgize::{
    export::{Container, Event, TraversalContext, Traverser},
    ParseConfig,
};
use rayon::prelude::*;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

const TODO_KEYWORDS: [&str; 2] = ["TODO", "DOIN"];
const DONE_KEYWORDS: [&str; 2] = ["DONE", "CNCL"];

#[derive(FromArgs)]
/// Sync org and gcal.
struct Args {
    #[argh(positional)]
    path: PathBuf,
}

struct Traversal {
    ok: bool,
}

impl Traverser for Traversal {
    fn event(&mut self, event: Event, ctx: &mut TraversalContext) {
        match event {
            Event::Enter(Container::Headline(headline)) => {
                if headline.todo_keyword().is_some() {
                    self.ok = true;
                    ctx.stop();
                } else if headline
                    .tags()
                    .any(|t| t == "w" || t.starts_with("w@") || t == "big_event")
                {
                    self.ok = true;
                    ctx.stop();
                }
            }
            _ => {}
        }
    }
}

impl Traversal {
    fn finish(self) -> bool {
        self.ok
    }
}

fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(fmt::layer())
        .with(EnvFilter::from_default_env())
        .init();

    let args: Args = argh::from_env();

    let parse_config = ParseConfig {
        todo_keywords: (
            TODO_KEYWORDS.into_iter().map(|s| s.to_string()).collect(),
            DONE_KEYWORDS.into_iter().map(|s| s.to_string()).collect(),
        ),
        ..Default::default()
    };

    let paths = walkdir::WalkDir::new(args.path)
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
        .filter(|entry| {
            // Entry is guaranteed to be Ok and to be an org file.
            // Read entry as str.
            let data = fs::read_to_string(entry.path()).unwrap();

            // Parse our document.
            let org = parse_config.clone().parse(&data);

            let mut traversal = Traversal { ok: false };
            org.traverse(&mut traversal);

            let res = traversal.finish();

            res
        })
        .filter_map(|entry| std::path::absolute(entry.path()).ok())
        .fold(String::new, |mut a, b| {
            let new_str = b.to_string_lossy();
            a.reserve(new_str.len() + 1);
            a.push_str(&new_str);
            a.push_str("\n");

            a
        })
        .reduce(String::new, |mut a, b| {
            a.reserve(b.len() + 1);
            a.push_str(&b);
            a
        });

    let s = paths.trim_end();
    println!("{s}");

    Ok(())
}
