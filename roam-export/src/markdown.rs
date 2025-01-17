use std::cmp::min;
use std::collections::HashMap;
use std::fmt::{Display, Write as _};
use std::path::PathBuf;
use std::str::FromStr;

use orgize::ast::Headline;
use orgize::export::{Container, Event, TraversalContext, Traverser};
use orgize::rowan::ast::AstNode;
use orgize::TextRange;
use tracing::{trace, warn};
use uuid::Uuid;

use crate::EXPORT_TAG;

#[derive(Debug)]
pub enum ExportContext {
    File,
    Headline(Headline),
}

impl Display for ExportContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExportContext::File => write!(f, "file"),
            ExportContext::Headline(headline) => write!(f, "'{}'", headline.title_raw()),
        }
    }
}

#[derive(Debug)]
pub struct MarkdownExport {
    this_file: PathBuf,
    node_map: HashMap<Uuid, String>,
    headline_map: HashMap<(PathBuf, TextRange), String>,

    file_front_matter: Vec<(String, String)>,
    entered_headline: bool,
    output_stack: Vec<(ExportContext, String)>,
    finished_outputs: Vec<(ExportContext, String)>,
    inside_blockquote: bool,
}

impl MarkdownExport {
    pub fn new(
        this_file: PathBuf,
        node_map: HashMap<Uuid, String>,
        headline_map: HashMap<(PathBuf, TextRange), String>,
    ) -> Self {
        Self {
            this_file,
            node_map,
            headline_map,

            file_front_matter: Vec::new(),
            entered_headline: false,
            output_stack: Vec::new(),
            finished_outputs: Vec::new(),
            inside_blockquote: false,
        }
    }

    pub fn finish(mut self) -> Vec<(ExportContext, String)> {
        while let Some((ctx, res)) = self.output_stack.pop() {
            trace!("out {:?}", self.output_stack);
            trace!("fin {:?}", self.finished_outputs);

            self.finished_outputs.push((ctx, res));
        }

        // Let's check if we need to insert front matter.
        // The file item should always be the last one in finished outputs.
        if let Some((ExportContext::File, output)) = self.finished_outputs.last_mut() {
            let mut preamble = "---\n".to_string();
            for (k, v) in self.file_front_matter {
                preamble += &format!("{k}: {v}");
            }
            preamble += "---\n\n";

            *output = format!("{preamble}{output}");
        }

        self.finished_outputs
    }
}

impl Traverser for MarkdownExport {
    fn event(&mut self, event: Event, ctx: &mut TraversalContext) {
        // First, let's check if we need to add things to the export stack.
        match &event {
            Event::Enter(Container::Keyword(k)) => {
                let raw = k.raw();
                if raw.starts_with("#+filetags") {
                    // Let's get the tags.
                    let (_, all_tags) = raw.split_once(':').unwrap();
                    for tag in all_tags.trim().split(':') {
                        if tag == EXPORT_TAG {
                            self.output_stack.push((ExportContext::File, String::new()));
                            break;
                        }
                    }
                } else {
                    // For other keywords, let's first remove the #+
                    let removed = raw.trim_start_matches("#+");

                    // Let's then split on the colon.
                    let (k, v_ws) = removed.split_once(':').unwrap();

                    // Trim whitespace in the value.
                    let v = v_ws.trim();

                    // And insert.
                    self.file_front_matter.push((k.to_string(), v.to_string()));
                }

                return ctx.skip();
            }
            Event::Enter(Container::PropertyDrawer(ps)) => {
                if self.entered_headline {
                    return ctx.skip();
                }

                // If we're looking at a file-level property drawer, add front matter.
                for p in ps.node_properties() {
                    let raw = p.raw();
                    let kv = raw.trim_start_matches(':');
                    let (k, v_ws) = kv.split_once(':').unwrap();
                    let v = v_ws.trim();

                    self.file_front_matter.push((k.to_string(), v.to_string()));
                }

                return ctx.skip();
            }
            Event::Enter(Container::Headline(h)) => {
                self.entered_headline = true;

                // First, if we see a headline, we need to pop off the stack anything of a lower
                // level than this headline.
                while let Some((ctx, res)) = self.output_stack.pop() {
                    match ctx {
                        ExportContext::File => {
                            self.output_stack.push((ctx, res));
                            break;
                        }
                        ExportContext::Headline(stack_h) => {
                            if h.level() <= stack_h.level() {
                                self.finished_outputs
                                    .push((ExportContext::Headline(stack_h), res));

                                // Keep going!
                            } else {
                                self.output_stack
                                    .push((ExportContext::Headline(stack_h), res));
                                break;
                            }
                        }
                    }
                }

                if h.tags().find(|t| t == EXPORT_TAG).is_some() {
                    // If there's currently something on the output stack,
                    // write an embed link there.
                    if let Some(output) = self.output_stack.last_mut().map(|t| &mut t.1) {
                        let k = (self.this_file.clone(), h.text_range());
                        if let Some(fname) = self.headline_map.get(&k) {
                            *output += &format!("![[{fname}]]\n\n");
                        } else {
                            warn!("exported headline {} with no id", h.title_raw());
                            *output += &format!("exported headline {} with no id", h.title_raw());
                        }
                    }

                    let mut preamble = "---\n".to_owned();
                    // Push the title.
                    preamble += "title: ";
                    preamble += &h.title_raw();
                    preamble += "\n";

                    // Push other properties.
                    if let Some(ps) = h.properties() {
                        for p in ps.node_properties() {
                            let raw = p.raw();
                            let (k, v_ws) = raw[1..].split_once(':').unwrap();
                            let v = v_ws.trim();

                            preamble += &format!("{k}: {v}\n");
                        }
                    }

                    preamble += "---\n\n";

                    self.output_stack
                        .push((ExportContext::Headline(h.clone()), preamble));

                    return;
                }
            }
            _ => {}
        }

        trace!("out {:?}", self.output_stack);
        trace!("fin {:?}", self.finished_outputs);

        // Now, let's do some actual rendering.

        if let Some((ex_ctx, output)) = self.output_stack.last_mut() {
            match event {
                Event::Enter(Container::Document(_)) => {}
                Event::Leave(Container::Document(_)) => {}

                Event::Enter(Container::Headline(headline)) => {
                    // Let's figure out what to do here.
                    // First, we need to pop off

                    if !output.is_empty() && !output.ends_with(['\n', '\r']) {
                        *output += "\n";
                    }

                    let offset = match ex_ctx {
                        ExportContext::File => 0,
                        ExportContext::Headline(h) => h.level(),
                    };

                    let level = min(headline.level().saturating_sub(offset), 6);
                    let _ = write!(output, "{} ", "#".repeat(level));
                    for elem in headline.title() {
                        self.element(elem, ctx);
                    }
                }
                Event::Leave(Container::Headline(_)) => {}

                Event::Enter(Container::Paragraph(_)) => {}
                Event::Leave(Container::Paragraph(_)) => *output += "\n",

                Event::Enter(Container::Section(_)) => {
                    if !output.is_empty() && !output.ends_with(['\n', '\r']) {
                        *output += "\n";
                    }
                }
                Event::Leave(Container::Section(_)) => {}

                Event::Enter(Container::Italic(_)) => *output += "*",
                Event::Leave(Container::Italic(_)) => *output += "*",

                Event::Enter(Container::Bold(_)) => *output += "**",
                Event::Leave(Container::Bold(_)) => *output += "**",

                Event::Enter(Container::Strike(_)) => *output += "~~",
                Event::Leave(Container::Strike(_)) => *output += "~~",

                Event::Enter(Container::Underline(_)) => {}
                Event::Leave(Container::Underline(_)) => {}

                Event::Enter(Container::Verbatim(_))
                | Event::Leave(Container::Verbatim(_))
                | Event::Enter(Container::Code(_))
                | Event::Leave(Container::Code(_)) => *output += "`",

                Event::Enter(Container::SourceBlock(block)) => {
                    if !output.is_empty() && !output.ends_with(['\n', '\r']) {
                        *output += "\n";
                    }
                    *output += "```";
                    if let Some(language) = block.language() {
                        *output += &language;
                    }
                }
                Event::Leave(Container::SourceBlock(_)) => *output += "```\n",

                Event::Enter(Container::QuoteBlock(_)) => {
                    self.inside_blockquote = true;
                    if !output.is_empty() && !output.ends_with(['\n', '\r']) {
                        *output += "\n";
                    }
                    *output += "> ";
                }
                Event::Leave(Container::QuoteBlock(_)) => self.inside_blockquote = false,

                Event::Enter(Container::CommentBlock(_)) => *output += "<!--",
                Event::Leave(Container::CommentBlock(_)) => *output += "-->",

                Event::Enter(Container::Comment(_)) => *output += "<!--",
                Event::Leave(Container::Comment(_)) => *output += "-->",

                Event::Enter(Container::Subscript(_)) => *output += "<sub>",
                Event::Leave(Container::Subscript(_)) => *output += "</sub>",

                Event::Enter(Container::Superscript(_)) => *output += "<sup>",
                Event::Leave(Container::Superscript(_)) => *output += "</sup>",

                Event::Enter(Container::List(_list)) => {}
                Event::Leave(Container::List(_list)) => {}

                Event::Enter(Container::ListItem(list_item)) => {
                    if !output.is_empty() && !output.ends_with(['\n', '\r']) {
                        *output += "\n";
                    }
                    *output += &" ".repeat(list_item.indent());
                    *output += &list_item.bullet();
                }
                Event::Leave(Container::ListItem(_)) => {}

                Event::Enter(Container::OrgTable(table)) => {
                    warn!(
                        "skipping org table in {} ({}): \"{}...\"",
                        self.this_file.to_string_lossy(),
                        ex_ctx,
                        table.raw().split('\n').next().unwrap()
                    );
                    *output += "skipped table\n";
                    return ctx.skip();
                }
                Event::Leave(Container::OrgTable(_)) => {}
                Event::Enter(Container::OrgTableRow(_row)) => {}
                Event::Leave(Container::OrgTableRow(_row)) => {}
                Event::Enter(Container::OrgTableCell(_)) => {}
                Event::Leave(Container::OrgTableCell(_)) => {}

                Event::Enter(Container::Link(link)) => {
                    let path = link.path();

                    if path.starts_with("id:") {
                        let id = path.trim_start_matches("id:");
                        let uuid = Uuid::from_str(id).expect("invalid id");
                        if let Some(fname) = self.node_map.get(&uuid) {
                            if link.has_description() {
                                let _ = write!(output, "[[{fname}][{}]]", link.description_raw());
                            } else {
                                let _ = write!(output, "[[{fname}]]");
                            }
                            return ctx.skip();
                        } else {
                            warn!(
                                %uuid,
                                "link to non-existent uuid '{}' in {} ({})",
                                link.description_raw(),
                                self.this_file.to_string_lossy(),
                                ex_ctx
                            );
                            let _ = write!(output, "link to non-existent uuid {}", uuid);
                            return ctx.skip();
                        }
                    }

                    let path = path.trim_start_matches("file:");

                    if link.is_image() {
                        let _ = write!(output, "![]({path})");
                        return ctx.skip();
                    }

                    if !link.has_description() {
                        let _ = write!(output, r#"[{}]({})"#, &path, &path);
                        return ctx.skip();
                    }

                    *output += "[";
                }
                Event::Leave(Container::Link(link)) => {
                    let _ = write!(output, r#"]({})"#, &*link.path());
                }

                Event::Text(text) => {
                    if self.inside_blockquote {
                        for (idx, line) in text.split('\n').enumerate() {
                            if idx != 0 {
                                *output += "\n>  ";
                            }
                            *output += line;
                        }
                    } else {
                        *output += &*text;
                    }
                }

                Event::LineBreak(_) => {}

                Event::Snippet(_snippet) => {}

                Event::Rule(_) => *output += "\n-----\n",

                Event::Timestamp(_timestamp) => {}

                Event::LatexFragment(latex) => {
                    let _ = write!(output, "{}", latex.syntax());
                }
                Event::LatexEnvironment(latex) => {
                    let _ = write!(output, "{}", latex.syntax());
                }

                Event::Entity(entity) => *output += entity.utf8(),

                _ => {}
            }
        }
    }
}
