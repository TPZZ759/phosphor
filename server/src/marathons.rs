// Marathon & Franchise Watch-Order Mode: 15 curated bingeable franchises with both
// Chronological Story Order and Theatrical Release Order, instant O(1) TMDB catalog
// matching, OLED quality-tier badges, live watch-progress / Next-Up resolution,
// and dynamic local TMDB collection fallback. Zero network calls.
use crate::app::App;
use crate::catalog::{Catalog, Item};
use crate::discover::watched_set;
use crate::meta::{Col, Meta};
use crate::showcase::{best_tier, is_cam_only, QualityTier};
use crate::util::{s, tmdb_img};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug)]
pub struct MarathonEntry {
    /// TMDB media type: `"movie"` or `"tv"`.
    pub mt: &'static str,
    /// Exact TMDB numeric ID as string.
    pub tmdb: &'static str,
    pub title: &'static str,
    pub year: i32,
    /// 1-based position in in-universe chronological story order.
    pub chrono_idx: u16,
    /// 1-based position in original theatrical / broadcast release order.
    pub release_idx: u16,
    pub phase: &'static str,
    /// Fallback runtime in minutes (total season binge duration for TV series).
    pub runtime_mins: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct MarathonDef {
    pub slug: &'static str,
    pub title: &'static str,
    pub tagline: &'static str,
    pub icon: &'static str,
    pub entries: &'static [MarathonEntry],
}

const fn mv(
    tmdb: &'static str,
    title: &'static str,
    year: i32,
    chrono_idx: u16,
    release_idx: u16,
    phase: &'static str,
    runtime_mins: u16,
) -> MarathonEntry {
    MarathonEntry {
        mt: "movie",
        tmdb,
        title,
        year,
        chrono_idx,
        release_idx,
        phase,
        runtime_mins,
    }
}

const fn tv(
    tmdb: &'static str,
    title: &'static str,
    year: i32,
    chrono_idx: u16,
    release_idx: u16,
    phase: &'static str,
    runtime_mins: u16,
) -> MarathonEntry {
    MarathonEntry {
        mt: "tv",
        tmdb,
        title,
        year,
        chrono_idx,
        release_idx,
        phase,
        runtime_mins,
    }
}

// ---------------------------------------------------------------------------
// 1. 15 Curated Franchises (verified TMDB IDs, Chronological + Release Orders)
// ---------------------------------------------------------------------------

pub const MARATHONS: &[MarathonDef] = &[
    MarathonDef {
        slug: "mcu",
        title: "Marvel Cinematic Universe",
        tagline: "From 1940s super-soldiers to the Infinity Stones and the Multiverse",
        icon: "⚡",
        entries: &[
            mv("1771", "Captain America: The First Avenger", 2011, 1, 4, "Phase 1", 124),
            mv("299537", "Captain Marvel", 2019, 2, 20, "Phase 3", 124),
            mv("1726", "Iron Man", 2008, 3, 1, "Phase 1", 126),
            mv("10138", "Iron Man 2", 2010, 4, 2, "Phase 1", 124),
            mv("10195", "Thor", 2011, 5, 3, "Phase 1", 115),
            mv("24428", "The Avengers", 2012, 6, 5, "Phase 1", 143),
            mv("68721", "Iron Man 3", 2013, 7, 6, "Phase 2", 130),
            mv("76338", "Thor: The Dark World", 2013, 8, 7, "Phase 2", 112),
            mv("100402", "Captain America: The Winter Soldier", 2014, 9, 8, "Phase 2", 136),
            mv("118340", "Guardians of the Galaxy", 2014, 10, 9, "Phase 2", 121),
            mv("283995", "Guardians of the Galaxy Vol. 2", 2017, 11, 14, "Phase 3", 137),
            mv("99861", "Avengers: Age of Ultron", 2015, 12, 10, "Phase 2", 141),
            mv("102899", "Ant-Man", 2015, 13, 11, "Phase 2", 117),
            mv("271110", "Captain America: Civil War", 2016, 14, 12, "Phase 3", 147),
            mv("497698", "Black Widow", 2021, 15, 23, "Phase 4", 134),
            mv("284054", "Black Panther", 2018, 16, 17, "Phase 3", 135),
            mv("315635", "Spider-Man: Homecoming", 2017, 17, 15, "Phase 3", 133),
            mv("284052", "Doctor Strange", 2016, 18, 13, "Phase 3", 115),
            mv("284053", "Thor: Ragnarok", 2017, 19, 16, "Phase 3", 130),
            mv("363088", "Ant-Man and the Wasp", 2018, 20, 19, "Phase 3", 118),
            mv("299536", "Avengers: Infinity War", 2018, 21, 18, "Phase 3", 149),
            mv("299534", "Avengers: Endgame", 2019, 22, 21, "Phase 3", 181),
            mv("429617", "Spider-Man: Far From Home", 2019, 23, 22, "Phase 3", 129),
            mv("566525", "Shang-Chi and the Legend of the Ten Rings", 2021, 24, 24, "Phase 4", 132),
            mv("634649", "Spider-Man: No Way Home", 2021, 25, 25, "Phase 4", 148),
            mv("453395", "Doctor Strange in the Multiverse of Madness", 2022, 26, 26, "Phase 4", 126),
            mv("447365", "Guardians of the Galaxy Vol. 3", 2023, 27, 27, "Phase 5", 150),
            mv("533535", "Deadpool & Wolverine", 2024, 28, 28, "Phase 5", 128),
        ],
    },
    MarathonDef {
        slug: "star-wars",
        title: "Star Wars Saga",
        tagline: "From the fall of the Republic to the Rebellion and the Resistance",
        icon: "⚔️",
        entries: &[
            mv("1893", "Star Wars: Episode I - The Phantom Menace", 1999, 1, 4, "Prequel Trilogy", 136),
            mv("1894", "Star Wars: Episode II - Attack of the Clones", 2002, 2, 5, "Prequel Trilogy", 142),
            mv("1895", "Star Wars: Episode III - Revenge of the Sith", 2005, 3, 6, "Prequel Trilogy", 140),
            mv("348350", "Solo: A Star Wars Story", 2018, 4, 10, "Anthology", 135),
            mv("330459", "Rogue One: A Star Wars Story", 2016, 5, 8, "Anthology", 133),
            mv("11", "Star Wars", 1977, 6, 1, "Original Trilogy", 121),
            mv("1891", "The Empire Strikes Back", 1980, 7, 2, "Original Trilogy", 124),
            mv("1892", "Return of the Jedi", 1983, 8, 3, "Original Trilogy", 132),
            mv("140607", "Star Wars: The Force Awakens", 2015, 9, 7, "Sequel Trilogy", 136),
            mv("181808", "Star Wars: The Last Jedi", 2017, 10, 9, "Sequel Trilogy", 152),
            mv("181812", "Star Wars: The Rise of Skywalker", 2019, 11, 11, "Sequel Trilogy", 142),
        ],
    },
    MarathonDef {
        slug: "middle-earth",
        title: "Middle-earth (Hobbit & Lord of the Rings)",
        tagline: "The complete journey from the Shire to Erebor and Mount Doom",
        icon: "💍",
        entries: &[
            mv("49051", "The Hobbit: An Unexpected Journey", 2012, 1, 4, "The Hobbit", 169),
            mv("57158", "The Hobbit: The Desolation of Smaug", 2013, 2, 5, "The Hobbit", 161),
            mv("122917", "The Hobbit: The Battle of the Five Armies", 2014, 3, 6, "The Hobbit", 144),
            mv("120", "The Lord of the Rings: The Fellowship of the Ring", 2001, 4, 1, "The Lord of the Rings", 179),
            mv("121", "The Lord of the Rings: The Two Towers", 2002, 5, 2, "The Lord of the Rings", 179),
            mv("122", "The Lord of the Rings: The Return of the King", 2003, 6, 3, "The Lord of the Rings", 201),
        ],
    },
    MarathonDef {
        slug: "alien-predator",
        title: "Alien & Predator Universe",
        tagline: "Apex hunters on Earth and Xenomorph terror across deep space",
        icon: "👾",
        entries: &[
            mv("766507", "Prey", 2022, 1, 10, "Predator Era", 100),
            mv("106", "Predator", 1987, 2, 3, "Predator Era", 107),
            mv("169", "Predator 2", 1990, 3, 4, "Predator Era", 108),
            mv("34851", "Predators", 2010, 4, 7, "Predator Era", 107),
            mv("70981", "Prometheus", 2012, 5, 8, "Alien Prequels", 124),
            mv("126889", "Alien: Covenant", 2017, 6, 9, "Alien Prequels", 122),
            mv("348", "Alien", 1979, 7, 1, "Xenomorph Saga", 117),
            mv("945961", "Alien: Romulus", 2024, 8, 11, "Xenomorph Saga", 119),
            mv("679", "Aliens", 1986, 9, 2, "Xenomorph Saga", 137),
            mv("8077", "Alien³", 1992, 10, 5, "Xenomorph Saga", 114),
            mv("8078", "Alien Resurrection", 1997, 11, 6, "Xenomorph Saga", 109),
        ],
    },
    MarathonDef {
        slug: "john-wick",
        title: "John Wick",
        tagline: "Baba Yaga's relentless war against the High Table",
        icon: "🎯",
        entries: &[
            mv("245891", "John Wick", 2014, 1, 1, "The High Table", 101),
            mv("324552", "John Wick: Chapter 2", 2017, 2, 2, "The High Table", 122),
            mv("458156", "John Wick: Chapter 3 - Parabellum", 2019, 3, 3, "The High Table", 131),
            mv("541671", "From the World of John Wick: Ballerina", 2025, 4, 5, "Spin-off", 125),
            mv("603692", "John Wick: Chapter 4", 2023, 5, 4, "The High Table", 169),
        ],
    },
    MarathonDef {
        slug: "mission-impossible",
        title: "Mission: Impossible",
        tagline: "Ethan Hunt and the IMF's most audacious operations",
        icon: "💣",
        entries: &[
            mv("954", "Mission: Impossible", 1996, 1, 1, "Classic IMF", 110),
            mv("955", "Mission: Impossible II", 2000, 2, 2, "Classic IMF", 123),
            mv("956", "Mission: Impossible III", 2006, 3, 3, "Classic IMF", 126),
            mv("56292", "Mission: Impossible - Ghost Protocol", 2011, 4, 4, "Syndicate Era", 133),
            mv("177677", "Mission: Impossible - Rogue Nation", 2015, 5, 5, "Syndicate Era", 131),
            mv("353081", "Mission: Impossible - Fallout", 2018, 6, 6, "Syndicate Era", 147),
            mv("575264", "Mission: Impossible - Dead Reckoning Part One", 2023, 7, 7, "The Entity", 163),
            mv("575265", "Mission: Impossible - The Final Reckoning", 2025, 8, 8, "The Entity", 169),
        ],
    },
    MarathonDef {
        slug: "dark-knight-batman",
        title: "Batman & Gotham Cinema",
        tagline: "Gothic shadows, Nolan's realism, and detective noir in Gotham City",
        icon: "🦇",
        entries: &[
            mv("268", "Batman", 1989, 1, 1, "Burton Era", 126),
            mv("364", "Batman Returns", 1992, 2, 2, "Burton Era", 126),
            mv("272", "Batman Begins", 2005, 3, 3, "The Dark Knight Trilogy", 140),
            mv("155", "The Dark Knight", 2008, 4, 4, "The Dark Knight Trilogy", 152),
            mv("49026", "The Dark Knight Rises", 2012, 5, 5, "The Dark Knight Trilogy", 165),
            mv("475557", "Joker", 2019, 6, 6, "Modern Gotham", 122),
            mv("414906", "The Batman", 2022, 7, 7, "Modern Gotham", 176),
        ],
    },
    MarathonDef {
        slug: "harry-potter",
        title: "Wizarding World (Fantastic Beasts + Harry Potter)",
        tagline: "From 1920s magical New York to seven years at Hogwarts",
        icon: "🪄",
        entries: &[
            mv("259316", "Fantastic Beasts and Where to Find Them", 2016, 1, 9, "Fantastic Beasts", 132),
            mv("338952", "Fantastic Beasts: The Crimes of Grindelwald", 2018, 2, 10, "Fantastic Beasts", 134),
            mv("338953", "Fantastic Beasts: The Secrets of Dumbledore", 2022, 3, 11, "Fantastic Beasts", 142),
            mv("671", "Harry Potter and the Philosopher's Stone", 2001, 4, 1, "Hogwarts Years 1–4", 152),
            mv("672", "Harry Potter and the Chamber of Secrets", 2002, 5, 2, "Hogwarts Years 1–4", 161),
            mv("673", "Harry Potter and the Prisoner of Azkaban", 2004, 6, 3, "Hogwarts Years 1–4", 141),
            mv("674", "Harry Potter and the Goblet of Fire", 2005, 7, 4, "Hogwarts Years 1–4", 157),
            mv("675", "Harry Potter and the Order of the Phoenix", 2007, 8, 5, "The Dark Lord's Return", 138),
            mv("767", "Harry Potter and the Half-Blood Prince", 2009, 9, 6, "The Dark Lord's Return", 153),
            mv("12444", "Harry Potter and the Deathly Hallows: Part 1", 2010, 10, 7, "The Dark Lord's Return", 146),
            mv("12445", "Harry Potter and the Deathly Hallows: Part 2", 2011, 11, 8, "The Dark Lord's Return", 130),
        ],
    },
    MarathonDef {
        slug: "dune",
        title: "Dune Saga",
        tagline: "Ten thousand years of Bene Gesserit prophecy and the war for Arrakis",
        icon: "🏜️",
        entries: &[
            tv("90228", "Dune: Prophecy", 2024, 1, 3, "10,000 Years Before Paul", 360),
            mv("438631", "Dune", 2021, 2, 1, "Paul Atreides", 155),
            mv("693134", "Dune: Part Two", 2024, 3, 2, "Paul Atreides", 166),
        ],
    },
    MarathonDef {
        slug: "mad-max",
        title: "Mad Max Wasteland",
        tagline: "Chrome, fury, and survival across Australia's scorched Outback",
        icon: "🔥",
        entries: &[
            mv("9659", "Mad Max", 1979, 1, 1, "Original Trilogy", 91),
            mv("8810", "Mad Max 2", 1981, 2, 2, "Original Trilogy", 96),
            mv("9355", "Mad Max Beyond Thunderdome", 1985, 3, 3, "Original Trilogy", 107),
            mv("786892", "Furiosa: A Mad Max Saga", 2024, 4, 5, "Fury Road Saga", 149),
            mv("76341", "Mad Max: Fury Road", 2015, 5, 4, "Fury Road Saga", 120),
        ],
    },
    MarathonDef {
        slug: "jurassic",
        title: "Jurassic Park & World",
        tagline: "Sixty-five million years in the making — Isla Nublar and beyond",
        icon: "🦖",
        entries: &[
            mv("329", "Jurassic Park", 1993, 1, 1, "Jurassic Park", 127),
            mv("330", "The Lost World: Jurassic Park", 1997, 2, 2, "Jurassic Park", 129),
            mv("331", "Jurassic Park III", 2001, 3, 3, "Jurassic Park", 92),
            mv("135397", "Jurassic World", 2015, 4, 4, "Jurassic World", 124),
            mv("351286", "Jurassic World: Fallen Kingdom", 2018, 5, 5, "Jurassic World", 128),
            mv("507086", "Jurassic World Dominion", 2022, 6, 6, "Jurassic World", 147),
            mv("1234821", "Jurassic World Rebirth", 2025, 7, 7, "New Era", 134),
        ],
    },
    MarathonDef {
        slug: "james-bond-craig",
        title: "James Bond (Daniel Craig Era)",
        tagline: "One continuous 007 arc from first kill to final mission",
        icon: "🍸",
        entries: &[
            mv("36557", "Casino Royale", 2006, 1, 1, "Origin", 144),
            mv("10764", "Quantum of Solace", 2008, 2, 2, "Quantum", 106),
            mv("37724", "Skyfall", 2012, 3, 3, "MI6", 143),
            mv("206647", "Spectre", 2015, 4, 4, "Spectre", 148),
            mv("370172", "No Time to Die", 2021, 5, 5, "Finale", 163),
        ],
    },
    MarathonDef {
        slug: "planet-of-the-apes",
        title: "Planet of the Apes (Modern Saga)",
        tagline: "Caesar's revolution and the centuries-long rise of ape civilization",
        icon: "🦍",
        entries: &[
            mv("61791", "Rise of the Planet of the Apes", 2011, 1, 1, "Caesar's Trilogy", 105),
            mv("119450", "Dawn of the Planet of the Apes", 2014, 2, 2, "Caesar's Trilogy", 130),
            mv("281338", "War for the Planet of the Apes", 2017, 3, 3, "Caesar's Trilogy", 140),
            mv("653346", "Kingdom of the Planet of the Apes", 2024, 4, 4, "Generations Later", 145),
        ],
    },
    MarathonDef {
        slug: "monsterverse",
        title: "MonsterVerse (Godzilla & Kong)",
        tagline: "Titans awaken — from 1973 Skull Island to Hollow Earth",
        icon: "🌋",
        entries: &[
            mv("293167", "Kong: Skull Island", 2017, 1, 2, "1973 Expedition", 118),
            mv("124905", "Godzilla", 2014, 2, 1, "Titan Awakening", 123),
            mv("373571", "Godzilla: King of the Monsters", 2019, 3, 3, "Titan Awakening", 132),
            mv("399566", "Godzilla vs. Kong", 2021, 4, 4, "Hollow Earth", 113),
            mv("823464", "Godzilla x Kong: The New Empire", 2024, 5, 5, "Hollow Earth", 115),
        ],
    },
    MarathonDef {
        slug: "studio-ghibli",
        title: "Studio Ghibli Masterpieces",
        tagline: "Hand-drawn wonder from Hayao Miyazaki and Isao Takahata",
        icon: "🍃",
        entries: &[
            mv("81", "Nausicaä of the Valley of the Wind", 1984, 1, 1, "Early Classics", 117),
            mv("10515", "Castle in the Sky", 1986, 2, 2, "Early Classics", 125),
            mv("8392", "My Neighbor Totoro", 1988, 3, 3, "Early Classics", 86),
            mv("12477", "Grave of the Fireflies", 1988, 4, 4, "Early Classics", 89),
            mv("16859", "Kiki's Delivery Service", 1989, 5, 5, "Golden Era", 103),
            mv("11621", "Porco Rosso", 1992, 6, 6, "Golden Era", 94),
            mv("128", "Princess Mononoke", 1997, 7, 7, "Golden Era", 134),
            mv("129", "Spirited Away", 2001, 8, 8, "Masterworks", 125),
            mv("4935", "Howl's Moving Castle", 2004, 9, 9, "Masterworks", 119),
            mv("12429", "Ponyo", 2008, 10, 10, "Masterworks", 101),
            mv("149870", "The Wind Rises", 2013, 11, 11, "Late Miyazaki", 126),
            mv("508883", "The Boy and the Heron", 2023, 12, 12, "Late Miyazaki", 124),
        ],
    },
];

// ---------------------------------------------------------------------------
// 2. Ordering, Watch-State Snapshot & Resolution Helpers
// ---------------------------------------------------------------------------

fn normalize_order(order: &str) -> &'static str {
    match order.trim().to_ascii_lowercase().as_str() {
        "release" | "released" | "year" | "theatrical" => "release",
        _ => "chrono",
    }
}

fn ordered_entries(def: &MarathonDef, order: &str) -> Vec<&'static MarathonEntry> {
    let mut v: Vec<&'static MarathonEntry> = def.entries.iter().collect();
    if normalize_order(order) == "release" {
        v.sort_by_key(|e| (e.release_idx, e.year, e.chrono_idx));
    } else {
        v.sort_by_key(|e| (e.chrono_idx, e.release_idx));
    }
    v
}

fn has_distinct_order(def: &MarathonDef) -> bool {
    let chrono = ordered_entries(def, "chrono");
    let release = ordered_entries(def, "release");
    chrono
        .iter()
        .zip(release.iter())
        .any(|(a, b)| a.mt != b.mt || a.tmdb != b.tmdb)
}

#[derive(Clone, Copy, Debug, Default)]
struct ProgInfo {
    progress: f64,
    resume: f64,
    updated_at: i64,
}

#[derive(Default)]
struct WatchState {
    watched: HashSet<String>,
    wanted: HashSet<String>,
    in_list: HashSet<String>,
    movie_prog: HashMap<i64, ProgInfo>,
    series_prog: HashMap<i64, ProgInfo>,
    watched_at: HashMap<String, i64>,
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn load_watch_state(app: &App, cat: &Catalog) -> WatchState {
    let watched = watched_set(app);
    let mut ws = WatchState {
        watched,
        ..Default::default()
    };
    app.state.read(|st| {
        if let Some(m) = st["wanted"].as_object() {
            ws.wanted.extend(m.keys().cloned());
        }
        if let Some(m) = st["list"].as_object() {
            ws.in_list.extend(m.keys().cloned());
        }
        if let Some(m) = st["watched"].as_object() {
            for (k, v) in m {
                let t = v["t"].as_i64().unwrap_or(0);
                let cur = ws.watched_at.entry(k.clone()).or_insert(0);
                if t > *cur {
                    *cur = t;
                }
            }
        }
        for h in st["history"].as_array().into_iter().flatten() {
            let id = s(&h["tmdb"]);
            if !id.is_empty() {
                let key = format!("{}:{id}", if h["kind"] == "s" { "tv" } else { "movie" });
                let t = h["t"].as_i64().unwrap_or(0);
                let cur = ws.watched_at.entry(key).or_insert(0);
                if t > *cur {
                    *cur = t;
                }
            }
        }
        if let Some(pm) = st["progress"].as_object() {
            for (key, p) in pm {
                let dur = p["dur"].as_f64().unwrap_or(0.0).max(0.0);
                let raw_pos = p["pos"].as_f64().unwrap_or(0.0).max(0.0);
                let pos = if dur > 0.0 { raw_pos.clamp(0.0, dur) } else { raw_pos };
                let pct = if dur > 0.0 { (pos / dur).clamp(0.0, 1.0) } else { 0.0 };
                let t = p["t"].as_i64().unwrap_or(0);
                let info = ProgInfo {
                    progress: round3(pct),
                    resume: round1(pos),
                    updated_at: t,
                };
                if let Some(rest) = key.strip_prefix("m:") {
                    if let Ok(raw_id) = rest.parse::<i64>() {
                        let canon = cat.movie(raw_id).map(|x| x.id).unwrap_or(raw_id);
                        for id in [raw_id, canon] {
                            let entry = ws.movie_prog.entry(id).or_default();
                            if info.updated_at >= entry.updated_at {
                                *entry = info;
                            }
                        }
                    }
                } else if key.starts_with("e:") || key.starts_with("s:") {
                    let raw_sid = p["item"]["sid"]
                        .as_i64()
                        .or_else(|| p["item"]["sid"].as_str().and_then(|x| x.parse().ok()))
                        .or_else(|| {
                            key.strip_prefix("s:")
                                .and_then(|x| x.parse::<i64>().ok())
                        })
                        .or_else(|| {
                            if p["item"]["k"] == "s" {
                                p["item"]["id"].as_i64()
                            } else {
                                None
                            }
                        });
                    if let Some(raw_sid) = raw_sid {
                        let canon = cat.series_item(raw_sid).map(|x| x.id).unwrap_or(raw_sid);
                        for sid in [raw_sid, canon] {
                            let entry = ws.series_prog.entry(sid).or_default();
                            if info.updated_at >= entry.updated_at {
                                *entry = info;
                            }
                        }
                    }
                }
            }
        }
    });
    ws
}

impl WatchState {
    fn progress_for(&self, it: &Item) -> ProgInfo {
        let map = if it.k == "s" {
            &self.series_prog
        } else {
            &self.movie_prog
        };
        let mut best = map.get(&it.id).copied().unwrap_or_default();
        for v in &it.variants {
            if let Some(&vp) = map.get(&v.id) {
                if vp.updated_at > best.updated_at
                    || (vp.updated_at == best.updated_at && vp.progress > best.progress)
                {
                    best = vp;
                }
            }
        }
        best
    }
}

struct ResolvedEntry {
    card: Value,
    available: bool,
    watched: bool,
    uhd: bool,
    dv: bool,
    cam_only: bool,
    runtime_mins: i64,
    remaining_mins: i64,
    progress: f64,
    last_activity_ts: i64,
    art_score: f64,
    pop: f64,
    poster: String,
    backdrop: String,
}

struct ResolvedMarathon {
    summary: Value,
    items: Vec<Value>,
    in_progress: bool,
    completed: bool,
    available_count: usize,
    last_activity_ts: i64,
    pop_score: f64,
    curated_rank: usize,
}

fn item_has_dv(it: &Item, tier: QualityTier) -> bool {
    tier == QualityTier::Dv4k
        || (!it.cam && it.dv)
        || it.variants.iter().any(|v| !v.cam && v.dv)
}

fn resolve_curated_entry<FCard, FTier, FCam>(
    entry: &MarathonEntry,
    pos_idx: usize,
    cat: &Catalog,
    meta: Option<&Meta>,
    ws: &WatchState,
    make_card: &FCard,
    tier_of: &FTier,
    cam_only_of: &FCam,
) -> ResolvedEntry
where
    FCard: Fn(&Item) -> Value,
    FTier: Fn(&Item) -> QualityTier,
    FCam: Fn(&Item) -> bool,
{
    let tmdb_key = format!("{}:{}", entry.mt, entry.tmdb);
    let watched = ws.watched.contains(&tmdb_key);
    let mut last_ts = ws.watched_at.get(&tmdb_key).copied().unwrap_or(0);

    if let Some(it) = cat.by_tmdb(entry.mt, entry.tmdb) {
        let tier = tier_of(it);
        let cam_only = cam_only_of(it);
        let uhd = !cam_only && tier >= QualityTier::Uhd4k;
        let dv = !cam_only && item_has_dv(it, tier);
        let prog = ws.progress_for(it);
        if prog.updated_at > last_ts {
            last_ts = prog.updated_at;
        }
        let runtime_mins: i64 = if entry.mt == "tv" {
            entry.runtime_mins as i64
        } else {
            meta.map(|m| m.rt)
                .filter(|&rt| rt > 0)
                .unwrap_or(entry.runtime_mins as i64)
        };
        let remaining_mins: i64 = if watched {
            0
        } else if prog.progress > 0.0 && prog.progress < 1.0 {
            ((1.0 - prog.progress) * runtime_mins as f64).round().max(1.0) as i64
        } else {
            runtime_mins
        };

        let mut c = make_card(it);
        if s(&c["title"]).is_empty() {
            c["title"] = json!(entry.title);
        }
        if c["year"].is_null() {
            c["year"] = json!(entry.year);
        }
        if s(&c["poster"]).is_empty() {
            if let Some(m) = meta.filter(|m| !m.pp.is_empty()) {
                c["poster"] = json!(tmdb_img(&m.pp, "w342"));
            }
        }
        if s(&c["backdrop"]).is_empty() {
            if let Some(m) = meta.filter(|m| !m.bd.is_empty()) {
                c["backdrop"] = json!(tmdb_img(&m.bd, "w1280"));
            }
        }
        let poster = s(&c["poster"]);
        let backdrop = s(&c["backdrop"]);
        let rating = c["rating"].as_f64().unwrap_or(it.rating);

        c["available"] = json!(true);
        c["k"] = json!(it.k);
        c["id"] = json!(it.id);
        c["mt"] = json!(entry.mt);
        c["tmdb"] = json!(entry.tmdb);
        c["phase"] = json!(entry.phase);
        c["index"] = json!(pos_idx);
        c["chronoIndex"] = json!(entry.chrono_idx);
        c["releaseIndex"] = json!(entry.release_idx);
        c["runtime"] = json!(runtime_mins);
        if entry.mt == "tv" {
            if let Some(rt) = meta.map(|m| m.rt).filter(|&rt| rt > 0) {
                c["episodeRuntime"] = json!(rt);
            }
        }
        c["rating"] = json!(round1(rating));
        c["uhd"] = json!(uhd);
        c["dv"] = json!(dv);
        c["qualityLabel"] = json!(tier.label());
        c["qualityTier"] = json!(tier.as_u8());
        c["camOnly"] = json!(cam_only);
        c["watched"] = json!(watched);
        c["progress"] = json!(prog.progress);
        c["resume"] = json!(prog.resume);
        c["inList"] = json!(ws.in_list.contains(&format!("{}:{}", it.k, it.id)));

        let bayes = meta
            .filter(|m| m.va > 0.0)
            .map(|m| m.bayes())
            .unwrap_or(rating);
        let votes_bonus = meta
            .map(|m| ((m.vc.max(0) as f64) + 1.0).log10() * 0.25)
            .unwrap_or(0.0);
        let quality_bonus = if cam_only {
            -2.0
        } else if dv {
            0.35
        } else if uhd {
            0.2
        } else {
            0.0
        };
        let art_score = bayes + votes_bonus + quality_bonus;
        let pop = meta.map(|m| m.p.max(0.0)).unwrap_or(rating.max(0.0));

        ResolvedEntry {
            card: c,
            available: true,
            watched,
            uhd,
            dv,
            cam_only,
            runtime_mins,
            remaining_mins,
            progress: prog.progress,
            last_activity_ts: last_ts,
            art_score,
            pop,
            poster,
            backdrop,
        }
    } else {
        let wanted = ws.wanted.contains(&tmdb_key);
        let k = if entry.mt == "tv" { "s" } else { "m" };
        let runtime_mins = if entry.mt == "tv" {
            entry.runtime_mins as i64
        } else {
            meta.map(|m| m.rt)
                .filter(|&rt| rt > 0)
                .unwrap_or(entry.runtime_mins as i64)
        };
        let remaining_mins = if watched { 0 } else { runtime_mins };
        let poster = meta
            .map(|m| tmdb_img(&m.pp, "w342"))
            .unwrap_or_default();
        let backdrop = meta
            .map(|m| tmdb_img(&m.bd, "w1280"))
            .unwrap_or_default();
        let rating = meta.map(|m| round1(m.va)).unwrap_or(0.0);
        let overview = meta.map(|m| m.ov.clone()).unwrap_or_default();

        let mut c = json!({
            "available": false,
            "k": k,
            "mt": entry.mt,
            "tmdb": entry.tmdb,
            "title": entry.title,
            "year": entry.year,
            "phase": entry.phase,
            "index": pos_idx,
            "chronoIndex": entry.chrono_idx,
            "releaseIndex": entry.release_idx,
            "runtime": runtime_mins,
            "poster": poster,
            "backdrop": backdrop,
            "rating": rating,
            "watched": watched,
            "wanted": wanted,
        });
        if !overview.is_empty() {
            c["overview"] = json!(overview);
        }

        ResolvedEntry {
            card: c,
            available: false,
            watched,
            uhd: false,
            dv: false,
            cam_only: false,
            runtime_mins,
            remaining_mins,
            progress: 0.0,
            last_activity_ts: last_ts,
            art_score: rating - 5.0,
            pop: meta.map(|m| m.p.max(0.0)).unwrap_or(0.0),
            poster,
            backdrop,
        }
    }
}

fn assemble_marathon(
    slug: &str,
    title: &str,
    tagline: &str,
    icon: &str,
    order: &str,
    distinct_order: bool,
    is_dynamic: bool,
    collection_id: Option<i64>,
    fallback_backdrop: &str,
    curated_rank: usize,
    resolved: Vec<ResolvedEntry>,
) -> ResolvedMarathon {
    let total = resolved.len();
    let mut available_count = 0usize;
    let mut watched_count = 0usize;
    let mut uhd_count = 0usize;
    let mut dv_count = 0usize;
    let mut total_runtime_mins = 0i64;
    let mut remaining_runtime_mins = 0i64;
    let mut last_activity_ts = 0i64;
    let mut has_active_resume = false;
    let mut pop_sum = 0.0f64;
    let mut pop_cnt = 0usize;
    let mut next_up: Option<Value> = None;

    for r in &resolved {
        total_runtime_mins += r.runtime_mins;
        remaining_runtime_mins += r.remaining_mins;
        if r.watched {
            watched_count += 1;
        }
        if r.last_activity_ts > last_activity_ts {
            last_activity_ts = r.last_activity_ts;
        }
        if r.available {
            available_count += 1;
            if r.uhd {
                uhd_count += 1;
            }
            if r.dv {
                dv_count += 1;
            }
            if !r.watched && r.progress > 0.0 {
                has_active_resume = true;
            }
            if !r.watched && next_up.is_none() {
                next_up = Some(r.card.clone());
            }
            if r.pop > 0.0 {
                pop_sum += r.pop;
                pop_cnt += 1;
            }
        }
    }

    let pct_complete: u32 = if total == 0 {
        0
    } else {
        ((watched_count as f64 / total as f64) * 100.0)
            .round()
            .clamp(0.0, 100.0) as u32
    };
    let in_progress =
        (watched_count > 0 && available_count > 0 && next_up.is_some()) || has_active_resume;
    let completed = available_count > 0 && next_up.is_none() && watched_count > 0;

    // Pick OLED hero backdrop & up to 4 mini-posters from highest-rated available entries.
    let mut art_order: Vec<usize> = (0..resolved.len()).collect();
    art_order.sort_by(|&a, &b| {
        let ra = &resolved[a];
        let rb = &resolved[b];
        rb.available
            .cmp(&ra.available)
            .then_with(|| ra.cam_only.cmp(&rb.cam_only))
            .then_with(|| rb.art_score.total_cmp(&ra.art_score))
            .then(a.cmp(&b))
    });

    let backdrop = art_order
        .iter()
        .map(|&ix| resolved[ix].backdrop.as_str())
        .find(|b| !b.is_empty())
        .map(String::from)
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| fallback_backdrop.to_string());

    let mut posters: Vec<String> = Vec::with_capacity(4);
    for &ix in &art_order {
        let p = &resolved[ix].poster;
        if !p.is_empty() && !posters.contains(p) {
            posters.push(p.clone());
            if posters.len() >= 4 {
                break;
            }
        }
    }

    let avg_pop = if pop_cnt > 0 {
        pop_sum / pop_cnt as f64
    } else {
        0.0
    };
    let pop_score = avg_pop + (available_count as f64) * 1.5;

    let items: Vec<Value> = resolved.into_iter().map(|r| r.card).collect();

    let mut summary = json!({
        "slug": slug,
        "title": title,
        "tagline": tagline,
        "icon": icon,
        "order": order,
        "hasDistinctOrder": distinct_order,
        "total": total,
        "availableCount": available_count,
        "watchedCount": watched_count,
        "uhdCount": uhd_count,
        "dvCount": dv_count,
        "totalRuntimeMins": total_runtime_mins,
        "remainingRuntimeMins": remaining_runtime_mins,
        "pctComplete": pct_complete,
        "inProgress": in_progress,
        "completed": completed,
        "backdrop": backdrop,
        "posters": posters,
        "nextUp": next_up.unwrap_or(Value::Null),
    });
    if is_dynamic {
        summary["isDynamic"] = json!(true);
        if let Some(cid) = collection_id {
            summary["collectionId"] = json!(cid);
        }
    }

    ResolvedMarathon {
        summary,
        items,
        in_progress,
        completed,
        available_count,
        last_activity_ts,
        pop_score,
        curated_rank,
    }
}

fn sort_resolved_marathons(list: &mut [ResolvedMarathon]) {
    let bucket = |m: &ResolvedMarathon| -> u8 {
        if m.in_progress {
            0
        } else if m.available_count > 0 && !m.completed {
            1
        } else if m.completed {
            2
        } else {
            3
        }
    };
    list.sort_by(|a, b| {
        let ba = bucket(a);
        let bb = bucket(b);
        ba.cmp(&bb).then_with(|| match ba {
            0 => b
                .last_activity_ts
                .cmp(&a.last_activity_ts)
                .then_with(|| {
                    b.summary["pctComplete"]
                        .as_u64()
                        .cmp(&a.summary["pctComplete"].as_u64())
                })
                .then_with(|| b.available_count.cmp(&a.available_count))
                .then(a.curated_rank.cmp(&b.curated_rank)),
            2 => b
                .last_activity_ts
                .cmp(&a.last_activity_ts)
                .then_with(|| b.available_count.cmp(&a.available_count))
                .then(a.curated_rank.cmp(&b.curated_rank)),
            _ => b
                .pop_score
                .total_cmp(&a.pop_score)
                .then_with(|| b.available_count.cmp(&a.available_count))
                .then(a.curated_rank.cmp(&b.curated_rank)),
        })
    });
}

fn resolve_curated_with<FCard, FTier, FCam>(
    def: &MarathonDef,
    curated_rank: usize,
    order: &str,
    cat: &Catalog,
    meta_map: &HashMap<String, Meta>,
    ws: &WatchState,
    make_card: &FCard,
    tier_of: &FTier,
    cam_only_of: &FCam,
) -> ResolvedMarathon
where
    FCard: Fn(&Item) -> Value,
    FTier: Fn(&Item) -> QualityTier,
    FCam: Fn(&Item) -> bool,
{
    let norm_order = normalize_order(order);
    let ordered = ordered_entries(def, norm_order);
    let distinct = has_distinct_order(def);
    let resolved: Vec<ResolvedEntry> = ordered
        .into_iter()
        .enumerate()
        .map(|(ix, entry)| {
            let m = meta_map
                .get(&format!("{}:{}", entry.mt, entry.tmdb))
                .filter(|m| !m.missing);
            resolve_curated_entry(entry, ix + 1, cat, m, ws, make_card, tier_of, cam_only_of)
        })
        .collect();
    assemble_marathon(
        def.slug,
        def.title,
        def.tagline,
        def.icon,
        norm_order,
        distinct,
        false,
        None,
        "",
        curated_rank,
        resolved,
    )
}

fn snapshot_curated_meta(app: &App, defs: &[MarathonDef]) -> HashMap<String, Meta> {
    let db = app.meta.read().unwrap();
    let mut out = HashMap::new();
    for def in defs {
        for e in def.entries {
            let k = format!("{}:{}", e.mt, e.tmdb);
            if let Some(m) = db.get(&k) {
                if !m.missing {
                    out.insert(k, m.clone());
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// 3. Dynamic Catalog TMDB Collections (local-only, zero network calls)
// ---------------------------------------------------------------------------

struct DynColEntry {
    movie_idx: usize,
    tmdb: String,
    meta: Meta,
}

fn curated_movie_tmdb_set() -> HashSet<&'static str> {
    let mut set = HashSet::new();
    for def in MARATHONS {
        for e in def.entries {
            if e.mt == "movie" {
                set.insert(e.tmdb);
            }
        }
    }
    set
}

fn resolve_dynamic_collection<FCard, FTier, FCam>(
    col: &Col,
    mut members: Vec<DynColEntry>,
    cat: &Catalog,
    ws: &WatchState,
    rank: usize,
    make_card: &FCard,
    tier_of: &FTier,
    cam_only_of: &FCam,
) -> ResolvedMarathon
where
    FCard: Fn(&Item) -> Value,
    FTier: Fn(&Item) -> QualityTier,
    FCam: Fn(&Item) -> bool,
{
    members.sort_by(|a, b| {
        let da = if a.meta.d.is_empty() { "9999" } else { a.meta.d.as_str() };
        let db = if b.meta.d.is_empty() { "9999" } else { b.meta.d.as_str() };
        da.cmp(db)
            .then_with(|| cat.movies[a.movie_idx].year.unwrap_or(9999).cmp(&cat.movies[b.movie_idx].year.unwrap_or(9999)))
            .then_with(|| cat.movies[a.movie_idx].title.cmp(&cat.movies[b.movie_idx].title))
    });

    let resolved: Vec<ResolvedEntry> = members
        .iter()
        .enumerate()
        .map(|(ix, ent)| {
            let it = &cat.movies[ent.movie_idx];
            let pos_idx = ix + 1;
            let tmdb_key = format!("movie:{}", ent.tmdb);
            let watched = ws.watched.contains(&tmdb_key);
            let mut last_ts = ws.watched_at.get(&tmdb_key).copied().unwrap_or(0);
            let tier = tier_of(it);
            let cam_only = cam_only_of(it);
            let uhd = !cam_only && tier >= QualityTier::Uhd4k;
            let dv = !cam_only && item_has_dv(it, tier);
            let prog = ws.progress_for(it);
            if prog.updated_at > last_ts {
                last_ts = prog.updated_at;
            }
            let runtime_mins = if ent.meta.rt > 0 { ent.meta.rt } else { 110 };
            let remaining_mins = if watched {
                0
            } else if prog.progress > 0.0 && prog.progress < 1.0 {
                ((1.0 - prog.progress) * runtime_mins as f64).round().max(1.0) as i64
            } else {
                runtime_mins
            };

            let mut c = make_card(it);
            if s(&c["poster"]).is_empty() && !ent.meta.pp.is_empty() {
                c["poster"] = json!(tmdb_img(&ent.meta.pp, "w342"));
            }
            if s(&c["backdrop"]).is_empty() && !ent.meta.bd.is_empty() {
                c["backdrop"] = json!(tmdb_img(&ent.meta.bd, "w1280"));
            }
            let poster = s(&c["poster"]);
            let backdrop = s(&c["backdrop"]);
            let rating = c["rating"].as_f64().unwrap_or(it.rating);

            c["available"] = json!(true);
            c["k"] = json!(it.k);
            c["id"] = json!(it.id);
            c["mt"] = json!("movie");
            c["tmdb"] = json!(&ent.tmdb);
            c["phase"] = json!("Release Order");
            c["index"] = json!(pos_idx);
            c["chronoIndex"] = json!(pos_idx);
            c["releaseIndex"] = json!(pos_idx);
            c["runtime"] = json!(runtime_mins);
            c["rating"] = json!(round1(rating));
            c["uhd"] = json!(uhd);
            c["dv"] = json!(dv);
            c["qualityLabel"] = json!(tier.label());
            c["qualityTier"] = json!(tier.as_u8());
            c["camOnly"] = json!(cam_only);
            c["watched"] = json!(watched);
            c["progress"] = json!(prog.progress);
            c["resume"] = json!(prog.resume);
            c["inList"] = json!(ws.in_list.contains(&format!("m:{}", it.id)));

            let bayes = if ent.meta.va > 0.0 {
                ent.meta.bayes()
            } else {
                rating
            };
            let votes_bonus = ((ent.meta.vc.max(0) as f64) + 1.0).log10() * 0.25;
            let quality_bonus = if cam_only { -2.0 } else if dv { 0.35 } else if uhd { 0.2 } else { 0.0 };

            ResolvedEntry {
                card: c,
                available: true,
                watched,
                uhd,
                dv,
                cam_only,
                runtime_mins,
                remaining_mins,
                progress: prog.progress,
                last_activity_ts: last_ts,
                art_score: bayes + votes_bonus + quality_bonus,
                pop: ent.meta.p.max(0.0),
                poster,
                backdrop,
            }
        })
        .collect();

    let clean_name = col.n.trim_end_matches(" Collection").trim();
    let title = if clean_name.is_empty() { col.n.as_str() } else { clean_name };
    let slug = format!("col-{}", col.id);
    let fallback_bd = tmdb_img(col.bd.as_deref().unwrap_or(""), "w1280");
    let tagline = format!("Complete {} film franchise in release order", resolved.len());

    assemble_marathon(
        &slug,
        title,
        &tagline,
        "🎬",
        "release",
        false,
        true,
        Some(col.id),
        &fallback_bd,
        rank,
        resolved,
    )
}

fn dynamic_collections(app: &App, cat: &Catalog, ws: &WatchState) -> Vec<Value> {
    let curated_ids = curated_movie_tmdb_set();
    let grouped: Vec<(Col, Vec<DynColEntry>)> = {
        let db = app.meta.read().unwrap();
        let mut groups: HashMap<i64, (Col, Vec<DynColEntry>)> = HashMap::new();
        for (ix, it) in cat.movies.iter().enumerate() {
            if !it.en {
                continue;
            }
            let Some(tmdb) = it.tmdb.as_deref() else { continue };
            let Some(m) = db.get(&format!("movie:{tmdb}")) else { continue };
            if m.missing {
                continue;
            }
            let Some(col) = &m.col else { continue };
            if col.id <= 0 {
                continue;
            }
            let entry = groups
                .entry(col.id)
                .or_insert_with(|| (col.clone(), Vec::new()));
            entry.1.push(DynColEntry {
                movie_idx: ix,
                tmdb: tmdb.to_string(),
                meta: m.clone(),
            });
        }
        groups
            .into_values()
            .filter(|(_, members)| {
                if members.len() < 3 {
                    return false;
                }
                let overlap = members
                    .iter()
                    .filter(|m| curated_ids.contains(m.tmdb.as_str()))
                    .count();
                overlap * 5 < members.len() * 3 // skip collections >=60% covered by curated marathons
            })
            .collect()
    };

    let make_card = |it: &Item| app.card_meta(it);
    let tier_of = |it: &Item| best_tier(app, it);
    let cam_only_of = |it: &Item| is_cam_only(app, it);

    let mut list: Vec<ResolvedMarathon> = grouped
        .into_iter()
        .enumerate()
        .map(|(rank, (col, members))| {
            resolve_dynamic_collection(&col, members, cat, ws, rank, &make_card, &tier_of, &cam_only_of)
        })
        .collect();
    sort_resolved_marathons(&mut list);
    list.truncate(32);
    list.into_iter().map(|m| m.summary).collect()
}

fn dynamic_collection_detail(app: &App, col_id: i64) -> Option<Value> {
    let cat = app.catalog();
    let ws = load_watch_state(app, &cat);
    let found: Option<(Col, Vec<DynColEntry>)> = {
        let db = app.meta.read().unwrap();
        let mut col_info: Option<Col> = None;
        let mut members: Vec<DynColEntry> = Vec::new();
        for (ix, it) in cat.movies.iter().enumerate() {
            if !it.en {
                continue;
            }
            let Some(tmdb) = it.tmdb.as_deref() else { continue };
            let Some(m) = db.get(&format!("movie:{tmdb}")) else { continue };
            if m.missing {
                continue;
            }
            let Some(col) = &m.col else { continue };
            if col.id == col_id {
                if col_info.is_none() {
                    col_info = Some(col.clone());
                }
                members.push(DynColEntry {
                    movie_idx: ix,
                    tmdb: tmdb.to_string(),
                    meta: m.clone(),
                });
            }
        }
        col_info.filter(|_| !members.is_empty()).map(|c| (c, members))
    };
    let (col, members) = found?;
    let make_card = |it: &Item| app.card_meta(it);
    let tier_of = |it: &Item| best_tier(app, it);
    let cam_only_of = |it: &Item| is_cam_only(app, it);

    let resolved = resolve_dynamic_collection(&col, members, &cat, &ws, 0, &make_card, &tier_of, &cam_only_of);
    let mut out = resolved.summary;
    out["items"] = Value::Array(resolved.items);
    Some(out)
}

// ---------------------------------------------------------------------------
// 4. Public API
// ---------------------------------------------------------------------------

/// List all 15 curated marathons (sorted by in-progress first, then availability & popularity)
/// plus a secondary `collections` shelf of dynamic local TMDB franchises.
pub fn list(app: &App) -> Value {
    let cat = app.catalog();
    let ws = load_watch_state(app, &cat);
    let meta_map = snapshot_curated_meta(app, MARATHONS);
    let make_card = |it: &Item| app.card_meta(it);
    let tier_of = |it: &Item| best_tier(app, it);
    let cam_only_of = |it: &Item| is_cam_only(app, it);

    let mut resolved: Vec<ResolvedMarathon> = MARATHONS
        .iter()
        .enumerate()
        .map(|(rank, def)| {
            resolve_curated_with(
                def,
                rank,
                "chrono",
                &cat,
                &meta_map,
                &ws,
                &make_card,
                &tier_of,
                &cam_only_of,
            )
        })
        .collect();
    sort_resolved_marathons(&mut resolved);

    let marathons: Vec<Value> = resolved.into_iter().map(|m| m.summary).collect();
    let collections = dynamic_collections(app, &cat, &ws);
    json!({
        "marathons": marathons,
        "collections": collections,
    })
}

/// Full marathon watch-order detail for any curated slug (e.g. `"mcu"`, `"star-wars"`)
/// or dynamic TMDB collection slug (`"col-<id>"`).
///
/// `order`: `"chrono"` (default, in-universe chronological order) or `"release"` (original release order).
pub fn detail(app: &App, slug: &str, order: &str) -> Option<Value> {
    let key = slug.trim().to_ascii_lowercase();
    if let Some((rank, def)) = MARATHONS
        .iter()
        .enumerate()
        .find(|(_, m)| m.slug == key)
    {
        let cat = app.catalog();
        let ws = load_watch_state(app, &cat);
        let meta_map = snapshot_curated_meta(app, std::slice::from_ref(def));
        let make_card = |it: &Item| app.card_meta(it);
        let tier_of = |it: &Item| best_tier(app, it);
        let cam_only_of = |it: &Item| is_cam_only(app, it);

        let resolved = resolve_curated_with(
            def,
            rank,
            order,
            &cat,
            &meta_map,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        let mut out = resolved.summary;
        out["items"] = Value::Array(resolved.items);
        return Some(out);
    }

    if let Some(col_id) = key
        .strip_prefix("col-")
        .and_then(|rest| rest.parse::<i64>().ok())
        .filter(|&id| id > 0)
    {
        return dynamic_collection_detail(app, col_id);
    }

    None
}

// ---------------------------------------------------------------------------
// 5. Unit Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn find(slug: &str) -> &'static MarathonDef {
        MARATHONS
            .iter()
            .find(|m| m.slug == slug)
            .unwrap_or_else(|| panic!("missing marathon slug {slug:?}"))
    }

    fn tmdb_order(slug: &str, order: &str) -> Vec<&'static str> {
        ordered_entries(find(slug), order)
            .into_iter()
            .map(|e| e.tmdb)
            .collect()
    }

    #[test]
    fn all_15_marathons_are_valid_and_unique() {
        assert_eq!(MARATHONS.len(), 15, "expected 15 curated marathons");

        let mut slugs = HashSet::new();
        for def in MARATHONS {
            assert!(!def.slug.is_empty());
            assert!(!def.title.is_empty());
            assert!(!def.tagline.is_empty());
            assert!(!def.icon.is_empty());
            assert!(
                slugs.insert(def.slug),
                "duplicate marathon slug: {}",
                def.slug
            );
            assert!(
                !def.entries.is_empty(),
                "marathon {} has no entries",
                def.slug
            );

            let n = def.entries.len() as u16;
            let mut tmdb_seen = HashSet::new();
            let mut chrono_seen = HashSet::new();
            let mut release_seen = HashSet::new();

            for e in def.entries {
                assert!(
                    e.mt == "movie" || e.mt == "tv",
                    "invalid mt {:?} in {}",
                    e.mt,
                    def.slug
                );
                assert!(
                    e.tmdb.parse::<u64>().is_ok(),
                    "non-numeric TMDB id {:?} in {}",
                    e.tmdb,
                    def.slug
                );
                assert!(
                    tmdb_seen.insert((e.mt, e.tmdb)),
                    "duplicate TMDB ({}, {}) in {}",
                    e.mt,
                    e.tmdb,
                    def.slug
                );
                assert!(!e.title.is_empty());
                assert!(!e.phase.is_empty());
                assert!(e.year >= 1970 && e.year <= 2030);
                assert!(e.runtime_mins >= 60);
                assert!(
                    e.chrono_idx >= 1 && e.chrono_idx <= n && chrono_seen.insert(e.chrono_idx),
                    "invalid or duplicate chrono_idx {} in {}",
                    e.chrono_idx,
                    def.slug
                );
                assert!(
                    e.release_idx >= 1 && e.release_idx <= n && release_seen.insert(e.release_idx),
                    "invalid or duplicate release_idx {} in {}",
                    e.release_idx,
                    def.slug
                );
            }

            // Release order must be monotonically non-decreasing by release year.
            let rel = ordered_entries(def, "release");
            for pair in rel.windows(2) {
                assert!(
                    pair[0].year <= pair[1].year,
                    "release order year inversion in {}: {} ({}) before {} ({})",
                    def.slug,
                    pair[0].title,
                    pair[0].year,
                    pair[1].title,
                    pair[1].year
                );
            }
        }
    }

    #[test]
    fn chrono_and_release_orders_differ_for_key_franchises() {
        for slug in [
            "mcu",
            "star-wars",
            "middle-earth",
            "alien-predator",
            "harry-potter",
            "mad-max",
            "john-wick",
            "dune",
            "monsterverse",
        ] {
            let def = find(slug);
            assert!(
                has_distinct_order(def),
                "{slug} should have distinct chrono and release orders"
            );
            assert_ne!(
                tmdb_order(slug, "chrono"),
                tmdb_order(slug, "release"),
                "{slug} chrono and release orders must differ"
            );
        }

        // MCU: Captain America: First Avenger is #1 chrono; Iron Man is #1 release.
        let mcu_c = tmdb_order("mcu", "chrono");
        let mcu_r = tmdb_order("mcu", "release");
        assert_eq!(mcu_c[0], "1771");
        assert_eq!(mcu_c[1], "299537");
        assert_eq!(mcu_r[0], "1726");

        // Star Wars: Phantom Menace #1 chrono, A New Hope #1 release, Rogue One before A New Hope in chrono.
        let sw_c = tmdb_order("star-wars", "chrono");
        let sw_r = tmdb_order("star-wars", "release");
        assert_eq!(sw_c[0], "1893");
        assert_eq!(sw_r[0], "11");
        let rogue_c = sw_c.iter().position(|&id| id == "330459").unwrap();
        let anh_c = sw_c.iter().position(|&id| id == "11").unwrap();
        assert!(rogue_c < anh_c, "Rogue One must precede A New Hope in chrono");

        // Middle-earth: Hobbit #1 chrono, Fellowship #1 release.
        assert_eq!(tmdb_order("middle-earth", "chrono")[0], "49051");
        assert_eq!(tmdb_order("middle-earth", "release")[0], "120");

        // Alien & Predator: Prey #1 chrono, Alien #1 release, Romulus before Aliens in chrono.
        let ap_c = tmdb_order("alien-predator", "chrono");
        let ap_r = tmdb_order("alien-predator", "release");
        assert_eq!(ap_c[0], "766507");
        assert_eq!(ap_r[0], "348");
        let prom_c = ap_c.iter().position(|&id| id == "70981").unwrap();
        let alien_c = ap_c.iter().position(|&id| id == "348").unwrap();
        let rom_c = ap_c.iter().position(|&id| id == "945961").unwrap();
        let aliens_c = ap_c.iter().position(|&id| id == "679").unwrap();
        assert!(prom_c < alien_c && alien_c < rom_c && rom_c < aliens_c);

        // Harry Potter: Fantastic Beasts #1 chrono, Philosopher's Stone #1 release.
        assert_eq!(tmdb_order("harry-potter", "chrono")[0], "259316");
        assert_eq!(tmdb_order("harry-potter", "release")[0], "671");

        // Mad Max: Furiosa precedes Fury Road in chrono; Fury Road precedes Furiosa in release.
        let mm_c = tmdb_order("mad-max", "chrono");
        let mm_r = tmdb_order("mad-max", "release");
        let furiosa_c = mm_c.iter().position(|&id| id == "786892").unwrap();
        let fury_c = mm_c.iter().position(|&id| id == "76341").unwrap();
        assert!(furiosa_c < fury_c, "Furiosa must precede Fury Road in chrono");
        let furiosa_r = mm_r.iter().position(|&id| id == "786892").unwrap();
        let fury_r = mm_r.iter().position(|&id| id == "76341").unwrap();
        assert!(fury_r < furiosa_r, "Fury Road must precede Furiosa in release");
    }

    #[test]
    fn resolution_computes_stats_next_up_quality_and_sorting() {
        let mut cat = Catalog::default();
        // Add 3 Mad Max movies to the mock catalog (leaving Thunderdome & Furiosa unavailable).
        let add_mv = |cat: &mut Catalog, id: i64, tmdb: &str, title: &str, year: i32, uhd: bool, dv: bool, cam: bool| {
            let ix = cat.movies.len();
            let it = Item {
                k: "m",
                id,
                title: title.to_string(),
                year: Some(year),
                poster: format!("https://img/{tmdb}.jpg"),
                backdrop: format!("https://img/{tmdb}_bd.jpg"),
                rating: 8.1,
                tmdb: Some(tmdb.to_string()),
                en: true,
                uhd,
                dv,
                cam,
                ..Default::default()
            };
            cat.movie_raw.insert(id, it.clone());
            cat.movie_by_tmdb.insert(tmdb.to_string(), ix);
            cat.movies.push(it);
        };
        add_mv(&mut cat, 1001, "9659", "Mad Max", 1979, false, false, false);
        add_mv(&mut cat, 1002, "8810", "Mad Max 2", 1981, true, false, false);
        add_mv(&mut cat, 1003, "76341", "Mad Max: Fury Road", 2015, true, true, false);
        // Also add 1 movie in John Wick (unstarted, high pop) to test in-progress sorting.
        add_mv(&mut cat, 2001, "245891", "John Wick", 2014, true, true, false);

        let mut meta_map = HashMap::new();
        meta_map.insert(
            "movie:76341".to_string(),
            Meta {
                rt: 120,
                va: 8.2,
                vc: 22000,
                p: 85.0,
                bd: "/fury_bd.jpg".into(),
                pp: "/fury_pp.jpg".into(),
                ..Default::default()
            },
        );
        meta_map.insert(
            "movie:8810".to_string(),
            Meta {
                rt: 96,
                va: 7.6,
                vc: 4000,
                p: 30.0,
                ..Default::default()
            },
        );
        meta_map.insert(
            "movie:245891".to_string(),
            Meta {
                rt: 101,
                va: 7.5,
                vc: 19000,
                p: 150.0,
                ..Default::default()
            },
        );

        let mut ws = WatchState::default();
        ws.watched.insert("movie:9659".to_string()); // Watched Mad Max 1
        ws.wanted.insert("movie:786892".to_string()); // Furiosa is wanted
        ws.movie_prog.insert(
            1002,
            ProgInfo {
                progress: 0.5,
                resume: 2880.0,
                updated_at: 50_000,
            },
        );

        let make_card = |it: &Item| {
            json!({
                "k": it.k,
                "id": it.id,
                "tmdb": it.tmdb,
                "title": it.title,
                "year": it.year,
                "poster": it.poster,
                "backdrop": it.backdrop,
                "rating": it.rating,
                "uhd": it.uhd,
            })
        };
        let tier_of = |it: &Item| {
            if it.cam {
                QualityTier::Cam
            } else if it.dv {
                QualityTier::Dv4k
            } else if it.uhd {
                QualityTier::Uhd4k
            } else {
                QualityTier::Hd
            }
        };
        let cam_only_of = |it: &Item| it.cam;

        let mm = resolve_curated_with(
            find("mad-max"),
            9,
            "chrono",
            &cat,
            &meta_map,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        assert_eq!(mm.summary["total"], 5);
        assert_eq!(mm.summary["availableCount"], 3);
        assert_eq!(mm.summary["watchedCount"], 1);
        assert_eq!(mm.summary["uhdCount"], 2);
        assert_eq!(mm.summary["dvCount"], 1);
        assert_eq!(mm.summary["pctComplete"], 20);
        assert_eq!(mm.summary["inProgress"], true);
        // Next up is Mad Max 2 (first available & unwatched in chrono order).
        assert_eq!(mm.summary["nextUp"]["tmdb"], "8810");
        assert_eq!(mm.summary["nextUp"]["progress"], 0.5);
        assert_eq!(mm.summary["nextUp"]["resume"], 2880.0);
        assert_eq!(mm.summary["nextUp"]["qualityLabel"], "4K");
        // Highest-rated available entry (Fury Road) supplies cover backdrop.
        assert_eq!(mm.summary["backdrop"], "https://img/76341_bd.jpg");
        // Unavailable Furiosa carries wanted=true.
        let furiosa = mm.items.iter().find(|x| x["tmdb"] == "786892").unwrap();
        assert_eq!(furiosa["available"], false);
        assert_eq!(furiosa["wanted"], true);
        // Remaining runtime excludes watched Mad Max 1 (91m) and half of Mad Max 2 (48m remaining).
        // Total: 91 + 96 + 107 + 149 + 120 = 563; Remaining: 0 + 48 + 107 + 149 + 120 = 424.
        assert_eq!(mm.summary["totalRuntimeMins"], 563);
        assert_eq!(mm.summary["remainingRuntimeMins"], 424);

        // In-progress Mad Max must sort ahead of higher-popularity unstarted John Wick.
        let jw = resolve_curated_with(
            find("john-wick"),
            4,
            "chrono",
            &cat,
            &meta_map,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        let mut all = vec![jw, mm];
        sort_resolved_marathons(&mut all);
        assert_eq!(all[0].summary["slug"], "mad-max");
        assert_eq!(all[1].summary["slug"], "john-wick");
    }

    #[test]
    fn next_up_tracks_watch_order_and_handles_tv_cam_and_dynamic_collections() {
        let mut cat = Catalog::default();
        let add_mv = |cat: &mut Catalog, id: i64, tmdb: &str, title: &str, year: i32, uhd: bool, dv: bool, cam: bool| {
            let ix = cat.movies.len();
            let it = Item {
                k: "m",
                id,
                title: title.to_string(),
                year: Some(year),
                poster: format!("https://img/{tmdb}.jpg"),
                backdrop: format!("https://img/{tmdb}_bd.jpg"),
                rating: 8.0,
                tmdb: Some(tmdb.to_string()),
                en: true,
                uhd,
                dv,
                cam,
                ..Default::default()
            };
            cat.movie_raw.insert(id, it.clone());
            cat.movie_by_tmdb.insert(tmdb.to_string(), ix);
            cat.movies.push(it);
        };
        let add_tv = |cat: &mut Catalog, id: i64, tmdb: &str, title: &str, year: i32, uhd: bool, dv: bool| {
            let ix = cat.series.len();
            let it = Item {
                k: "s",
                id,
                title: title.to_string(),
                year: Some(year),
                poster: format!("https://img/tv_{tmdb}.jpg"),
                backdrop: format!("https://img/tv_{tmdb}_bd.jpg"),
                rating: 7.4,
                tmdb: Some(tmdb.to_string()),
                en: true,
                uhd,
                dv,
                ..Default::default()
            };
            cat.series_raw.insert(id, it.clone());
            cat.series_by_tmdb.insert(tmdb.to_string(), ix);
            cat.series.push(it);
        };

        // Mad Max: Furiosa is CAM-only, Fury Road is 4K DV.
        add_mv(&mut cat, 101, "9659", "Mad Max", 1979, false, false, false);
        add_mv(&mut cat, 102, "8810", "Mad Max 2", 1981, false, false, false);
        add_mv(&mut cat, 103, "9355", "Mad Max Beyond Thunderdome", 1985, false, false, false);
        add_mv(&mut cat, 104, "786892", "Furiosa: A Mad Max Saga", 2024, false, false, true);
        add_mv(&mut cat, 105, "76341", "Mad Max: Fury Road", 2015, true, true, false);

        // Dune: TV series + both movies.
        add_tv(&mut cat, 301, "90228", "Dune: Prophecy", 2024, true, true);
        add_mv(&mut cat, 302, "438631", "Dune", 2021, true, true, false);
        add_mv(&mut cat, 303, "693134", "Dune: Part Two", 2024, true, true, false);

        // Dynamic collection: The Matrix Trilogy.
        add_mv(&mut cat, 401, "603", "The Matrix", 1999, true, true, false);
        add_mv(&mut cat, 402, "604", "The Matrix Reloaded", 2003, true, false, false);
        add_mv(&mut cat, 403, "605", "The Matrix Revolutions", 2003, true, false, false);

        let mut ws = WatchState::default();
        ws.watched.insert("movie:9659".into());
        ws.watched.insert("movie:8810".into());
        ws.watched.insert("movie:9355".into());
        ws.watched.insert("movie:603".into());
        ws.series_prog.insert(
            301,
            ProgInfo {
                progress: 0.25,
                resume: 900.0,
                updated_at: 100_000,
            },
        );

        let make_card = |it: &Item| {
            json!({
                "k": it.k,
                "id": it.id,
                "tmdb": it.tmdb,
                "title": it.title,
                "year": it.year,
                "poster": it.poster,
                "backdrop": it.backdrop,
                "rating": it.rating,
                "uhd": it.uhd,
            })
        };
        let tier_of = |it: &Item| {
            if it.cam {
                QualityTier::Cam
            } else if it.dv {
                QualityTier::Dv4k
            } else if it.uhd {
                QualityTier::Uhd4k
            } else {
                QualityTier::Hd
            }
        };
        let cam_only_of = |it: &Item| it.cam;

        let empty_meta = HashMap::new();
        let mm_chrono = resolve_curated_with(
            find("mad-max"),
            9,
            "chrono",
            &cat,
            &empty_meta,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        let mm_release = resolve_curated_with(
            find("mad-max"),
            9,
            "release",
            &cat,
            &empty_meta,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        // After original trilogy watched, chrono Next Up is Furiosa (CAM), release Next Up is Fury Road (4K DV).
        assert_eq!(mm_chrono.summary["nextUp"]["tmdb"], "786892");
        assert_eq!(mm_chrono.summary["nextUp"]["camOnly"], true);
        assert_eq!(mm_chrono.summary["nextUp"]["qualityLabel"], "CAM");
        assert_eq!(mm_chrono.summary["nextUp"]["qualityTier"], 0);
        assert_eq!(mm_chrono.summary["uhdCount"], 1);
        assert_eq!(mm_release.summary["nextUp"]["tmdb"], "76341");
        assert_eq!(mm_release.summary["nextUp"]["qualityLabel"], "4K DV");

        // Dune chrono starts with TV series Dune: Prophecy (k="s", with series progress); release starts with Dune (2021).
        let dune_c = resolve_curated_with(
            find("dune"),
            8,
            "chrono",
            &cat,
            &empty_meta,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        let dune_r = resolve_curated_with(
            find("dune"),
            8,
            "release",
            &cat,
            &empty_meta,
            &ws,
            &make_card,
            &tier_of,
            &cam_only_of,
        );
        assert_eq!(dune_c.summary["nextUp"]["k"], "s");
        assert_eq!(dune_c.summary["nextUp"]["tmdb"], "90228");
        assert_eq!(dune_c.summary["nextUp"]["progress"], 0.25);
        assert_eq!(dune_c.summary["inProgress"], true);
        assert_eq!(dune_r.summary["nextUp"]["k"], "m");
        assert_eq!(dune_r.summary["nextUp"]["tmdb"], "438631");

        // Dynamic collection builds release-ordered franchise with watched/nextUp stats.
        let col = Col {
            id: 2344,
            n: "The Matrix Collection".into(),
            pp: None,
            bd: Some("/matrix_col.jpg".into()),
        };
        let members = vec![
            DynColEntry {
                movie_idx: cat.movie_by_tmdb["605"],
                tmdb: "605".into(),
                meta: Meta { d: "2003-11-05".into(), rt: 129, va: 6.7, vc: 9000, p: 40.0, ..Default::default() },
            },
            DynColEntry {
                movie_idx: cat.movie_by_tmdb["603"],
                tmdb: "603".into(),
                meta: Meta { d: "1999-03-31".into(), rt: 136, va: 8.2, vc: 25000, p: 90.0, ..Default::default() },
            },
            DynColEntry {
                movie_idx: cat.movie_by_tmdb["604"],
                tmdb: "604".into(),
                meta: Meta { d: "2003-05-15".into(), rt: 138, va: 7.0, vc: 11000, p: 50.0, ..Default::default() },
            },
        ];
        let dyn_col = resolve_dynamic_collection(&col, members, &cat, &ws, 0, &make_card, &tier_of, &cam_only_of);
        assert_eq!(dyn_col.summary["slug"], "col-2344");
        assert_eq!(dyn_col.summary["title"], "The Matrix");
        assert_eq!(dyn_col.summary["isDynamic"], true);
        assert_eq!(dyn_col.summary["total"], 3);
        assert_eq!(dyn_col.summary["watchedCount"], 1);
        assert_eq!(dyn_col.summary["nextUp"]["tmdb"], "604");
        assert_eq!(dyn_col.items[0]["tmdb"], "603");
        assert_eq!(dyn_col.items[1]["tmdb"], "604");
        assert_eq!(dyn_col.items[2]["tmdb"], "605");
    }
}
