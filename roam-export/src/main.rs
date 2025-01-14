//! Exports org-roam database.
//! We create a file for every node with a particular tag.
//! Files are given a slug name
//!

use std::{collections::HashMap, ffi::OsStr, fs, path::PathBuf, str::FromStr};

use argh::FromArgs;
use color_eyre::eyre::Result;
use orgize::{
    ast::Headline,
    export::{Container, Event, TraversalContext, Traverser},
    ParseConfig, TextRange,
};
use rayon::prelude::*;
use tracing::{error, info, warn};
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};
use uuid::Uuid;

mod markdown;

const TODO_KEYWORDS: [&str; 2] = ["TODO", "DOIN"];
const DONE_KEYWORDS: [&str; 2] = ["DONE", "CNCL"];

pub const EXPORT_TAG: &str = "export";

#[derive(FromArgs)]
/// Sync org and gcal.
struct Args {
    #[argh(positional)]
    notes: PathBuf,

    #[argh(positional)]
    output: PathBuf,

    #[argh(switch)]
    /// don't do anything
    dry: bool,
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

    let walk_one = jiff::Timestamp::now();
    let nodes: Vec<(Uuid, Node)> = walkdir::WalkDir::new(&args.notes)
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
        .flat_map(|entry| {
            // Entry is guaranteed to be Ok and to be an org file.
            // Read entry as str.
            let data = fs::read_to_string(entry.path()).unwrap();

            // Parse our document.
            let org = parse_config.clone().parse(&data);

            let mut traversal = IdTraversal::default();
            org.traverse(&mut traversal);

            let mut res = Vec::new();

            if let Some(fu) = traversal.file_uuid {
                res.push((fu, Node::File(entry.path().to_owned())));
            }

            for (u, h) in traversal.headline_uuids {
                res.push((
                    u,
                    Node::Headline(entry.path().to_owned(), h.title_raw(), h.text_range()),
                ));
            }

            res
        })
        .collect();
    let walk_one_end = jiff::Timestamp::now();
    info!("id pass finished in {:#}", walk_one_end - walk_one);

    let filenames: HashMap<Uuid, String> = nodes
        .clone()
        .into_iter()
        .map(|(u, n)| {
            let fname = match n {
                Node::File(path_buf) => {
                    path_buf.file_stem().unwrap().to_string_lossy().into_owned()
                }
                Node::Headline(_, title, _) => slug::slugify(title),
            };

            (u, fname)
        })
        .collect();

    let headline_names: HashMap<(PathBuf, TextRange), String> = nodes
        .clone()
        .into_iter()
        .filter_map(|(_, n)| match n {
            Node::File(_) => None,
            Node::Headline(path_buf, title, range) => {
                Some(((path_buf, range), format!("{}.org", slug::slugify(title))))
            }
        })
        .collect();

    let file_node_names: HashMap<PathBuf, String> = nodes
        .clone()
        .into_iter()
        .filter_map(|(_, n)| match n {
            Node::File(pathbuf) => Some((
                pathbuf.clone(),
                pathbuf.file_stem().unwrap().to_string_lossy().into_owned(),
            )),
            Node::Headline(_, _, _) => None,
        })
        .collect();

    if !args.dry {
        fs::create_dir_all(&args.output)?;
    }

    let walk_two = jiff::Timestamp::now();
    walkdir::WalkDir::new(args.notes)
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
            // Entry is guaranteed to be Ok and to be an org file.
            // Read entry as str.
            let data = fs::read_to_string(entry.path()).unwrap();

            // Parse our document.
            let org = parse_config.clone().parse(&data);

            // This time, traverse with markdown!

            let mut traversal = markdown::MarkdownExport::new(
                entry.path().to_owned(),
                filenames.clone(),
                headline_names.clone(),
            );
            org.traverse(&mut traversal);

            for (ex_ctx, contents) in traversal.finish() {
                let out_path = match ex_ctx {
                    markdown::ExportContext::File => {
                        if let Some(fname) = file_node_names.get(entry.path()) {
                            let new_path = args.output.join(fname).with_extension("org");
                            Some(new_path)
                        } else {
                            warn!(
                                "file {} with export tag {} but no id",
                                entry.path().to_string_lossy(),
                                EXPORT_TAG
                            );

                            None
                        }
                    }
                    markdown::ExportContext::Headline(headline) => {
                        let k = (entry.path().to_owned(), headline.text_range());

                        if let Some(fname) = headline_names.get(&k) {
                            let new_path = args.output.join(fname).with_extension("org");
                            Some(new_path)
                        } else {
                            warn!(
                                "headline {} with export tag {} but no id",
                                headline.title_raw(),
                                EXPORT_TAG
                            );

                            None
                        }
                    }
                };

                if let Some(p) = out_path {
                    println!("{}", p.to_str().unwrap());
                    if !args.dry {
                        // if p.exists() {
                        //     warn!("path {} exists!", p.to_str().unwrap());
                        // }

                        fs::write(&p, contents).expect(&format!("couldn't write file {:?}", p))
                    }
                }
            }
        });
    let walk_two_end = jiff::Timestamp::now();
    info!("wrote files in {:#}", walk_two_end - walk_two);
    info!("finished in {:#}", walk_two_end - walk_one);

    Ok(())
}

/// In org-roam, an ID can correspond to either a file or a headline in a note file.
#[derive(Debug, Clone)]
enum Node {
    File(PathBuf),
    Headline(PathBuf, String, TextRange),
}

/// Builds a map from IDs to Nodes.
#[derive(Debug, Default)]
struct IdTraversal {
    file_uuid: Option<Uuid>,
    headline_uuids: HashMap<Uuid, Headline>,
    entered_headline: bool,
}

impl Traverser for IdTraversal {
    fn event(&mut self, event: Event, _ctx: &mut TraversalContext) {
        match event {
            Event::Enter(Container::PropertyDrawer(ps)) => {
                if self.entered_headline {
                    return;
                }

                for prop in ps.node_properties() {
                    let raw = prop.raw();
                    if raw.starts_with(":ID:") {
                        let id = raw[4..].trim();
                        self.file_uuid =
                            Some(Uuid::from_str(id).expect(&format!("invalid uuid {id} for file")));
                        break;
                    }
                }
            }
            Event::Enter(Container::Headline(h)) => {
                self.entered_headline = true;

                if let Some(ps) = h.properties() {
                    for prop in ps.node_properties() {
                        let raw = prop.raw();
                        if raw.starts_with(":ID:") {
                            let id = raw[4..].trim();
                            self.headline_uuids.insert(
                                Uuid::from_str(id).expect(&format!(
                                    "invalid uuid {id} for headline {}",
                                    h.title_raw()
                                )),
                                h,
                            );
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}
