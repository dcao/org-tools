use std::{ffi::OsStr, fs, path::PathBuf};

use google_calendar::types::EventDateTime;
use jiff::{
    civil::{date, Date},
    tz::TimeZone,
    ToSpan, Zoned,
};
use orgize::{
    export::{Container, Event, TraversalContext, Traverser},
    ParseConfig,
};
use rayon::prelude::*;

const DONE_KEYWORDS: [&str; 2] = ["DONE", "CNCL"];

pub fn get_valid_items(path: PathBuf) -> Vec<AgendaItem> {
    let parse_config = ParseConfig {
        todo_keywords: (
            vec!["TODO".to_string(), "DOIN".to_string()],
            DONE_KEYWORDS.into_iter().map(|s| s.to_string()).collect(),
        ),
        ..Default::default()
    };
    let now = Zoned::now();

    walkdir::WalkDir::new(path)
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
            let parse = parse_config.clone().parse(&data);

            let mut traversal = Traversal {
                items: vec![],
                stack: vec![],
                now: now.clone(),
            };

            parse.traverse(&mut traversal);

            let res = traversal.finish();

            for item in &res {
                assert!(!item.timestamps.is_empty());
            }

            res
        })
        .collect()
}

struct Traversal {
    items: Vec<AgendaItem>,
    stack: Vec<AgendaItem>,
    now: Zoned,
}

// This traversal ignores four timestamps:
// - Timestamps for DONE/CNCL entries
// - Timestamps for all-day entries
// - Timestamps after today
// - Inactive timestamps
impl Traverser for Traversal {
    fn event(&mut self, event: Event, _ctx: &mut TraversalContext) {
        match event {
            Event::Enter(Container::Headline(headline)) => {
                let mut timestamps = vec![];

                if let Some(p) = headline.planning() {
                    if let Some(s) = p.scheduled() {
                        if let Some(ts) = RepeatedDate::from_org(&s, self.now.time_zone().clone()) {
                            timestamps.push(ts);
                        }
                    }

                    if let Some(s) = p.deadline() {
                        if let Some(ts) = RepeatedDate::from_org(&s, self.now.time_zone().clone()) {
                            timestamps.push(ts);
                        }
                    }
                }

                self.stack.push(AgendaItem {
                    name: headline.title_raw(),
                    timestamps,
                });
            }
            Event::Leave(Container::Headline(headline)) => {
                let mut l = self.stack.pop().expect("Left headline before entering?");

                // Immediately return if we're looking at a DONE/CNCL.
                if DONE_KEYWORDS
                    .into_iter()
                    .any(|k| headline.todo_keyword().as_ref().map(|c| c.as_ref()) == Some(k))
                {
                    return;
                }

                // Remove all invalid timestamps
                l.timestamps.retain_mut(|ts| match &ts.start {
                    Dateish::AllDay(_) => false,
                    Dateish::Precise(zoned) => {
                        if let Some(Dateish::Precise(zoned_end)) = &ts.end {
                            zoned_end > self.now
                        } else {
                            ts.end = Some(Dateish::Precise(
                                zoned.checked_add(1.hour()).expect("Overflow duration"),
                            ));

                            zoned > self.now
                        }
                    }
                });

                if !l.timestamps.is_empty() {
                    self.items.push(l);
                }
            }
            Event::Timestamp(ts) => {
                let Some(top) = self.stack.last_mut() else {
                    return;
                };

                if ts.is_inactive() {
                    return;
                }

                let Some(t) = RepeatedDate::from_org(&ts, self.now.time_zone().clone()) else {
                    return;
                };

                top.timestamps.push(t);
            }
            _ => {}
        }
    }
}

impl Traversal {
    fn finish(self) -> Vec<AgendaItem> {
        self.items
    }
}

#[derive(Debug, Clone)]
pub struct AgendaItem {
    pub name: String,
    pub timestamps: Vec<RepeatedDate>,
}

#[derive(Debug, Clone)]
pub struct RepeatedDate {
    start: Dateish,
    end: Option<Dateish>,
    repeat: Option<String>,
}

impl RepeatedDate {
    pub fn into_gcal(self) -> (EventDateTime, Option<EventDateTime>, Option<String>) {
        let start = self.start.into_gcal();
        let end = self.end.map(|e| e.into_gcal());

        let rep = self.repeat;

        (start, end, rep)
    }
}

impl Dateish {
    fn into_gcal(self) -> EventDateTime {
        match self {
            Dateish::AllDay(date) => EventDateTime {
                date: Some(
                    chrono::NaiveDate::from_ymd_opt(
                        date.year() as i32,
                        date.month() as u32,
                        date.day() as u32,
                    )
                    .unwrap(),
                ),
                date_time: None,
                time_zone: TimeZone::system().iana_name().unwrap().to_string(),
            },
            Dateish::Precise(zoned) => {
                let utc = zoned
                    .with_time_zone(TimeZone::UTC)
                    .timestamp()
                    .as_nanosecond();
                EventDateTime {
                    date: None,
                    date_time: Some(chrono::DateTime::from_timestamp_nanos(utc as i64)),
                    time_zone: TimeZone::system().iana_name().unwrap().to_string(),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Dateish {
    AllDay(Date),
    Precise(Zoned),
}

impl RepeatedDate {
    fn from_org(ts: &orgize::ast::Timestamp, tz: TimeZone) -> Option<Self> {
        let year_start = ts.year_start()?.parse().ok()?;
        let month_start = ts.month_start()?.parse().ok()?;
        let day_start = ts.day_start()?.parse().ok()?;

        let mut s = date(year_start, month_start, day_start);

        let hour_start = ts.hour_start().and_then(|h| h.parse().ok());
        let min_start = ts.minute_start().and_then(|m| m.parse().ok());

        let sish = if let (Some(mut h), Some(m)) = (hour_start, min_start) {
            if h >= 24 {
                h %= 24;
                s = s
                    .checked_add(1.day())
                    .expect("Adding one day overflowed date");
            }
            Dateish::Precise(
                s.at(h, m, 0, 0)
                    .to_zoned(tz.clone())
                    .expect("Timezone bullshit"),
            )
        } else {
            Dateish::AllDay(s)
        };

        let year_end = ts.year_end()?.parse().ok()?;
        let month_end = ts.month_end()?.parse().ok()?;
        let day_end = ts.day_end()?.parse().ok()?;

        let eish = if ts.is_range() {
            let mut e = date(year_end, month_end, day_end);

            let hour_end = ts.hour_end().and_then(|h| h.parse().ok());
            let min_end = ts.minute_end().and_then(|m| m.parse().ok());

            let eish = if let (Some(mut h), Some(m)) = (hour_end, min_end) {
                if h >= 24 {
                    h %= 24;
                    e = e
                        .checked_add(1.day())
                        .expect("Adding one day overflowed date");
                }
                Dateish::Precise(
                    e.at(h, m, 0, 0)
                        .to_zoned(tz.clone())
                        .expect("Timezone bullshit"),
                )
            } else {
                Dateish::AllDay(e)
            };

            Some(eish)
        } else {
            None
        };

        let repeat = if let (Some(unit), Some(int)) = (ts.repeater_unit(), ts.repeater_value()) {
            let freq = match unit {
                orgize::ast::TimeUnit::Hour => "HOURLY",
                orgize::ast::TimeUnit::Day => "DAILY",
                orgize::ast::TimeUnit::Week => "WEEKLY",
                orgize::ast::TimeUnit::Month => "MONTHLY",
                orgize::ast::TimeUnit::Year => "YEARLY",
            };

            Some(format!("RRULE:FREQ={freq};INTERVAL={}", int))
        } else {
            None
        };

        Some(Self {
            start: sish,
            end: eish,
            repeat,
        })
    }
}
