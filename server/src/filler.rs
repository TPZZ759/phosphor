// Anime filler database & skip-filler helper: built-in curated exact episode tables for the top
// long-running shonen anime, persistent on-disk cache (`data/filler-cache.json`, 30-day TTL),
// and live fallback scraping from `https://www.animefillerlist.com/shows/<slug>`.
use crate::app::App;
use crate::util::{norm, now_ms};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

const CACHE_TTL_MS: i64 = 30 * 86_400_000;
const MISS_TTL_MS: i64 = 7 * 86_400_000;
const INDEX_TTL_MS: i64 = 24 * 3_600_000;
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) tv-rs/1.0";

/// Inclusive episode ranges `(start, end)` for a show's filler, mixed-canon, and canon episodes.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ShowFiller {
    pub title: String,
    pub slug: String,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub filler: Vec<(i64, i64)>,
    #[serde(default)]
    pub mixed: Vec<(i64, i64)>,
    #[serde(default)]
    pub canon: Vec<(i64, i64)>,
    #[serde(default)]
    pub source: String, // "builtin" | "animefillerlist"
}

impl ShowFiller {
    pub fn is_filler(&self, abs_ep: i64) -> bool {
        episode_tag(self, abs_ep) == "filler"
    }
    pub fn is_mixed(&self, abs_ep: i64) -> bool {
        episode_tag(self, abs_ep) == "mixed"
    }
    pub fn filler_count(&self) -> i64 {
        count_ranges(&self.filler)
    }
    pub fn mixed_count(&self) -> i64 {
        count_ranges(&self.mixed)
    }
    pub fn filler_pct(&self) -> f64 {
        if self.total <= 0 {
            return 0.0;
        }
        ((self.filler_count() as f64 / self.total as f64) * 1000.0).round() / 10.0
    }
    /// Next non-filler episode strictly after `from_ep` (within known episode range).
    pub fn next_canon(&self, from_ep: i64) -> Option<i64> {
        let max_ep = self.total.max(max_range_end(&self.canon)).max(max_range_end(&self.mixed));
        if max_ep <= 0 {
            return None;
        }
        let mut ep = (from_ep + 1).max(1);
        while ep <= max_ep {
            let tag = episode_tag(self, ep);
            if tag == "canon" || tag == "mixed" {
                return Some(ep);
            }
            ep += 1;
        }
        None
    }
    pub fn to_summary_json(&self) -> Value {
        json!({
            "title": self.title,
            "slug": self.slug,
            "total": self.total,
            "fillerCount": self.filler_count(),
            "mixedCount": self.mixed_count(),
            "fillerPct": self.filler_pct(),
            "filler": self.filler,
            "mixed": self.mixed,
            "source": self.source,
        })
    }
}

/// Persistent thread-safe cache backed by `data/filler-cache.json`.
pub struct FillerCache {
    file: PathBuf,
    map: Mutex<HashMap<String, Value>>,
}

impl FillerCache {
    pub fn load(data: &Path) -> Self {
        let file = data.join("filler-cache.json");
        let map: HashMap<String, Value> = std::fs::read(&file)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        FillerCache { file, map: Mutex::new(map) }
    }

    pub fn get_fresh(&self, key: &str) -> Option<Option<ShowFiller>> {
        let v = self.map.lock().unwrap().get(key).cloned()?;
        let t = v["t"].as_i64().unwrap_or(0);
        let age = now_ms() - t;
        if v["miss"].as_bool().unwrap_or(false) {
            return if age < MISS_TTL_MS { Some(None) } else { None };
        }
        if age < CACHE_TTL_MS {
            serde_json::from_value::<ShowFiller>(v["v"].clone()).ok().map(Some)
        } else {
            None
        }
    }

    pub fn get_stale(&self, key: &str) -> Option<ShowFiller> {
        let v = self.map.lock().unwrap().get(key).cloned()?;
        if v["miss"].as_bool().unwrap_or(false) {
            return None;
        }
        serde_json::from_value::<ShowFiller>(v["v"].clone()).ok()
    }

    pub fn set_hit(&self, key: String, info: &ShowFiller) {
        let val = json!({ "t": now_ms(), "v": info });
        self.write_entry(key, val);
    }

    pub fn set_miss(&self, key: String) {
        let val = json!({ "t": now_ms(), "miss": true });
        self.write_entry(key, val);
    }

    fn write_entry(&self, key: String, val: Value) {
        let body = {
            let mut g = self.map.lock().unwrap();
            g.insert(key, val);
            serde_json::to_vec(&*g).unwrap()
        };
        let tmp = self.file.with_extension("json.tmp");
        if std::fs::write(&tmp, body).is_ok() {
            let _ = std::fs::rename(&tmp, &self.file);
        }
    }
}

static GLOBAL_CACHE: OnceLock<FillerCache> = OnceLock::new();
static AFL_INDEX: OnceLock<Mutex<(i64, Vec<(String, String)>)>> = OnceLock::new();

fn cache(app: &App) -> &'static FillerCache {
    GLOBAL_CACHE.get_or_init(|| FillerCache::load(&app.data))
}

fn in_ranges(ranges: &[(i64, i64)], ep: i64) -> bool {
    ranges.iter().any(|&(a, b)| ep >= a && ep <= b)
}

fn count_ranges(ranges: &[(i64, i64)]) -> i64 {
    ranges.iter().map(|&(a, b)| (b - a + 1).max(0)).sum()
}

fn max_range_end(ranges: &[(i64, i64)]) -> i64 {
    ranges.iter().map(|&(_, b)| b).max().unwrap_or(0)
}

fn merge_ranges(mut rs: Vec<(i64, i64)>) -> Vec<(i64, i64)> {
    rs.retain(|&(a, b)| a > 0 && b >= a);
    rs.sort_unstable();
    let mut out: Vec<(i64, i64)> = Vec::with_capacity(rs.len());
    for (a, b) in rs {
        if let Some(last) = out.last_mut() {
            if a <= last.1 + 1 {
                last.1 = last.1.max(b);
                continue;
            }
        }
        out.push((a, b));
    }
    out
}

fn complement_ranges(total: i64, filler: &[(i64, i64)], mixed: &[(i64, i64)]) -> Vec<(i64, i64)> {
    if total <= 0 {
        return vec![];
    }
    let mut excl = Vec::with_capacity(filler.len() + mixed.len());
    excl.extend_from_slice(filler);
    excl.extend_from_slice(mixed);
    let excl = merge_ranges(excl);
    let mut out = vec![];
    let mut cur = 1i64;
    for (a, b) in excl {
        if a > cur {
            out.push((cur, (a - 1).min(total)));
        }
        cur = cur.max(b + 1);
        if cur > total {
            break;
        }
    }
    if cur <= total {
        out.push((cur, total));
    }
    out
}

/// Tag for an absolute episode number: `"filler"` | `"mixed"` | `"canon"` | `""`.
pub fn episode_tag(info: &ShowFiller, abs_ep: i64) -> &'static str {
    if abs_ep <= 0 {
        return "";
    }
    if in_ranges(&info.filler, abs_ep) {
        return "filler";
    }
    if in_ranges(&info.mixed, abs_ep) {
        return "mixed";
    }
    if in_ranges(&info.canon, abs_ep) {
        return "canon";
    }
    if info.total > 0 && abs_ep <= info.total {
        return "canon";
    }
    ""
}

// ---------------------------------------------------------------- title normalization & built-in table

fn clean_title(title: &str) -> String {
    // Strip common provider prefixes ("EN - ", "ANIME | ") and trailing tags ("(2007)", "(DUB)", "[HD]").
    let s = title.replace('\u{00D7}', "x").replace('\u{2715}', "x");
    let s = crate::re!(r"(?i)^(?:(?:en|us|uk|jp|anime|dub|sub|multi)\s*[-|:]\s*)+").replace(s.trim(), "");
    let mut cur = s.trim().to_string();
    loop {
        let next = crate::re!(r"(?i)\s*[\(\[]\s*(?:(?:19|20)\d\d|dub|dubbed|sub|subbed|eng(?:lish)?(?:\s+dub)?|hd|fhd|4k|uhd|tv|uncensored|complete|dual[\s-]?audio|multi)\s*[\)\]]\s*$")
            .replace(&cur, "")
            .trim()
            .to_string();
        if next == cur {
            break;
        }
        cur = next;
    }
    norm(&cur)
}

struct BuiltinShow {
    title: &'static str,
    slug: &'static str,
    aliases: &'static [&'static str],
    total: i64,
    filler: &'static [(i64, i64)],
    mixed: &'static [(i64, i64)],
}

// Exact episode filler & mixed-canon ranges verified against AnimeFillerList.
const BUILTIN: &[BuiltinShow] = &[
    BuiltinShow {
        title: "Naruto",
        slug: "naruto",
        aliases: &["naruto", "naruto classic", "naruto part 1"],
        total: 220,
        filler: &[(26, 26), (97, 97), (101, 106), (136, 140), (143, 219)],
        mixed: &[
            (7, 7), (9, 9), (14, 16), (18, 21), (23, 24), (27, 30), (37, 41), (43, 47), (49, 49),
            (52, 60), (63, 63), (66, 66), (69, 72), (74, 74), (83, 83), (98, 100), (112, 114),
            (126, 127), (130, 131), (141, 142), (220, 220),
        ],
    },
    BuiltinShow {
        title: "Naruto Shippuden",
        slug: "naruto-shippuden",
        aliases: &["naruto shippuden", "naruto shippuuden", "naruto hurricane chronicles"],
        total: 500,
        filler: &[
            (57, 71), (91, 112), (144, 151), (170, 171), (176, 196), (223, 242), (257, 260),
            (271, 271), (279, 281), (284, 295), (303, 320), (347, 361), (376, 377), (388, 390),
            (394, 413), (416, 417), (422, 423), (427, 450), (464, 468), (480, 483),
        ],
        mixed: &[
            (1, 19), (24, 25), (45, 45), (49, 50), (54, 54), (56, 56), (89, 90), (113, 113),
            (115, 115), (127, 128), (213, 213), (254, 254), (296, 296), (324, 324), (327, 328),
            (330, 331), (338, 338), (346, 346), (362, 362), (385, 386), (415, 415), (419, 419),
            (426, 426), (451, 458), (460, 462), (469, 469), (471, 472), (478, 479),
        ],
    },
    BuiltinShow {
        title: "Bleach",
        slug: "bleach",
        aliases: &["bleach"],
        total: 366,
        filler: &[
            (33, 33), (50, 50), (64, 108), (128, 137), (147, 149), (168, 189), (204, 205),
            (213, 214), (228, 266), (287, 287), (298, 299), (303, 305), (311, 341), (355, 355),
        ],
        mixed: &[
            (8, 8), (27, 27), (32, 32), (46, 46), (109, 109), (111, 111), (116, 116), (119, 120),
            (124, 124), (141, 141), (143, 143), (146, 146), (156, 156), (160, 161), (190, 190),
            (193, 193), (206, 207), (209, 209), (222, 223), (267, 268), (274, 274), (276, 276),
            (284, 285), (288, 288), (290, 291), (295, 296), (310, 310), (342, 343), (345, 345),
            (347, 347), (351, 351), (357, 357),
        ],
    },
    BuiltinShow {
        title: "Bleach: Thousand-Year Blood War",
        slug: "bleach-thousand-year-blood-war",
        aliases: &["bleach thousand year blood war", "bleach tybw", "bleach sennen kessen hen"],
        total: 50,
        filler: &[],
        mixed: &[],
    },
    BuiltinShow {
        title: "One Piece",
        slug: "one-piece",
        aliases: &["one piece"],
        total: 1168,
        filler: &[
            (54, 60), (98, 99), (102, 102), (131, 143), (196, 206), (220, 225), (279, 283),
            (291, 292), (303, 303), (317, 319), (326, 336), (382, 384), (406, 407), (426, 429),
            (457, 458), (492, 492), (542, 542), (575, 578), (590, 590), (626, 627), (747, 750),
            (780, 782), (895, 896), (907, 907), (1029, 1030),
        ],
        mixed: &[
            (45, 47), (61, 61), (68, 69), (101, 101), (226, 226), (354, 354), (421, 421),
            (489, 489), (520, 520), (574, 574), (625, 625), (628, 628), (633, 633), (653, 653),
            (657, 657), (679, 679), (690, 690), (731, 731), (738, 738), (751, 751), (777, 778),
            (789, 789), (803, 803), (807, 807), (878, 879), (881, 885), (887, 890), (924, 924),
            (988, 989), (991, 991),
        ],
    },
    BuiltinShow {
        title: "Fairy Tail",
        slug: "fairy-tail",
        aliases: &["fairy tail"],
        total: 328,
        filler: &[(9, 9), (19, 19), (50, 50), (70, 75), (125, 150), (202, 226), (268, 268)],
        mixed: &[
            (20, 20), (49, 49), (69, 69), (151, 151), (182, 183), (201, 201), (255, 255),
            (260, 260), (270, 270), (312, 312),
        ],
    },
    BuiltinShow {
        title: "Black Clover",
        slug: "black-clover",
        aliases: &["black clover"],
        total: 170,
        filler: &[(29, 29), (66, 66), (68, 68), (82, 82), (123, 125), (131, 131), (134, 135), (142, 149)],
        mixed: &[(2, 2), (8, 9), (12, 12), (30, 30), (69, 69), (102, 102)],
    },
    BuiltinShow {
        title: "Boruto: Naruto Next Generations",
        slug: "boruto-naruto-next-generations",
        aliases: &["boruto", "boruto naruto next generations"],
        total: 293,
        filler: &[
            (16, 17), (40, 41), (48, 50), (67, 69), (96, 97), (112, 119), (138, 140), (152, 154),
            (156, 156), (256, 256),
        ],
        mixed: &[
            (1, 1), (18, 18), (24, 24), (59, 59), (93, 95), (127, 127), (157, 157), (192, 192),
            (210, 211),
        ],
    },
    BuiltinShow {
        title: "Dragon Ball Z",
        slug: "dragon-ball-z",
        aliases: &["dragon ball z", "dbz"],
        total: 291,
        filler: &[
            (9, 10), (12, 16), (39, 43), (100, 100), (102, 102), (108, 117), (124, 125),
            (170, 171), (174, 174), (195, 199), (202, 203), (274, 274), (288, 288),
        ],
        mixed: &[(11, 11), (17, 18), (20, 20), (44, 44), (204, 204), (229, 229), (251, 251), (287, 287)],
    },
    BuiltinShow {
        title: "Dragon Ball Z Kai",
        slug: "dragon-ball-z-kai",
        aliases: &["dragon ball z kai", "dragon ball kai", "dbz kai"],
        total: 167,
        filler: &[(101, 101), (165, 165)],
        mixed: &[
            (7, 8), (10, 11), (19, 19), (22, 23), (25, 27), (31, 34), (36, 38), (41, 47), (50, 51),
            (53, 53), (55, 55), (57, 61), (63, 65), (67, 74), (76, 78), (81, 98), (100, 100),
            (102, 107), (110, 115), (117, 123), (125, 138), (140, 160), (162, 164), (166, 167),
        ],
    },
    BuiltinShow {
        title: "Dragon Ball",
        slug: "dragon-ball",
        aliases: &["dragon ball"],
        total: 153,
        filler: &[(30, 33), (45, 45), (79, 83), (127, 132), (149, 153)],
        mixed: &[(29, 29), (42, 42), (44, 44)],
    },
    BuiltinShow {
        title: "Dragon Ball Super",
        slug: "dragon-ball-super",
        aliases: &["dragon ball super", "dbs"],
        total: 131,
        filler: &[(4, 4), (15, 15), (42, 46), (68, 70), (73, 76)],
        mixed: &[(1, 2), (89, 89)],
    },
    BuiltinShow {
        title: "Hunter x Hunter",
        slug: "hunter-x-hunter",
        aliases: &["hunter x hunter", "hunter x hunter 2011", "hunter hunter", "hxh"],
        total: 148,
        filler: &[(13, 13), (26, 26)],
        mixed: &[(12, 12)],
    },
    BuiltinShow {
        title: "My Hero Academia",
        slug: "my-hero-academia",
        aliases: &["my hero academia", "boku no hero academia", "mha"],
        total: 170,
        filler: &[(39, 39), (58, 58), (64, 64), (104, 104)],
        mixed: &[(54, 55), (89, 89), (105, 105), (127, 127)],
    },
    BuiltinShow {
        title: "Attack on Titan",
        slug: "attack-titan",
        aliases: &["attack on titan", "shingeki no kyojin", "aot"],
        total: 89,
        filler: &[],
        mixed: &[(22, 22), (33, 33), (38, 41)],
    },
    BuiltinShow {
        title: "Demon Slayer: Kimetsu no Yaiba",
        slug: "demon-slayer-kimetsu-no-yaiba",
        aliases: &["demon slayer", "demon slayer kimetsu no yaiba", "kimetsu no yaiba"],
        total: 63,
        filler: &[],
        mixed: &[(58, 59)],
    },
    BuiltinShow {
        title: "Jujutsu Kaisen",
        slug: "jujutsu-kaisen",
        aliases: &["jujutsu kaisen", "jjk"],
        total: 59,
        filler: &[],
        mixed: &[],
    },
    BuiltinShow {
        title: "Gintama",
        slug: "gintama",
        aliases: &["gintama", "gin tama"],
        total: 369,
        filler: &[
            (1, 2), (50, 50), (57, 57), (75, 75), (106, 106), (114, 114), (124, 125), (135, 135),
            (137, 137), (150, 150), (155, 155), (164, 164), (166, 166), (171, 171), (173, 174),
            (176, 176), (185, 185), (189, 189), (209, 209), (252, 252),
        ],
        mixed: &[(82, 82), (111, 111), (120, 120), (367, 367)],
    },
    BuiltinShow {
        title: "Fullmetal Alchemist",
        slug: "fullmetal-alchemist",
        aliases: &["fullmetal alchemist", "fullmetal alchemist 2003", "hagane no renkinjutsushi"],
        total: 51,
        filler: &[(4, 4), (10, 10), (37, 37)],
        mixed: &[],
    },
    BuiltinShow {
        title: "Fullmetal Alchemist: Brotherhood",
        slug: "fullmetal-alchemist-brotherhood",
        aliases: &["fullmetal alchemist brotherhood", "fmab", "hagane no renkinjutsushi fullmetal alchemist"],
        total: 64,
        filler: &[(27, 27)],
        mixed: &[(4, 4), (9, 9), (13, 14)],
    },
    BuiltinShow {
        title: "Inuyasha",
        slug: "inuyasha",
        aliases: &["inuyasha", "inu yasha"],
        total: 193,
        filler: &[
            (59, 59), (63, 65), (68, 68), (72, 72), (75, 79), (89, 101), (129, 130), (133, 134),
            (136, 140), (162, 163),
        ],
        mixed: &[
            (26, 26), (57, 58), (69, 69), (83, 84), (87, 87), (102, 102), (127, 128), (135, 135),
            (141, 142), (144, 144), (147, 148), (160, 160),
        ],
    },
    BuiltinShow {
        title: "Yu Yu Hakusho",
        slug: "yuyu-hakusho",
        aliases: &["yu yu hakusho", "yuyu hakusho", "yu yu hakusho ghost files"],
        total: 114,
        filler: &[],
        mixed: &[(11, 11), (13, 13), (109, 109), (111, 111)],
    },
    BuiltinShow {
        title: "Soul Eater",
        slug: "soul-eater",
        aliases: &["soul eater"],
        total: 51,
        filler: &[(17, 17)],
        mixed: &[(7, 7)],
    },
    BuiltinShow {
        title: "Blue Exorcist",
        slug: "blue-exorcist",
        aliases: &["blue exorcist", "ao no exorcist"],
        total: 37,
        filler: &[(6, 6), (11, 11), (18, 25)],
        mixed: &[(1, 3), (16, 17)],
    },
    BuiltinShow {
        title: "Fire Force",
        slug: "fire-force",
        aliases: &["fire force", "enen no shouboutai", "en en no shouboutai"],
        total: 48,
        filler: &[],
        mixed: &[(25, 25)],
    },
    BuiltinShow {
        title: "Tokyo Ghoul",
        slug: "tokyo-ghoul",
        aliases: &["tokyo ghoul"],
        total: 24,
        filler: &[],
        mixed: &[(13, 24)],
    },
    BuiltinShow {
        title: "Sword Art Online",
        slug: "sword-art-online",
        aliases: &["sword art online", "sao"],
        total: 50,
        filler: &[],
        mixed: &[],
    },
    BuiltinShow {
        title: "Detective Conan",
        slug: "detective-conan",
        aliases: &["detective conan", "case closed", "meitantei conan"],
        total: 1221,
        filler: &[
            (6, 6), (14, 14), (17, 17), (19, 19), (21, 21), (24, 26), (29, 30), (33, 33), (36, 37),
            (41, 41), (44, 45), (47, 47), (51, 51), (53, 53), (55, 56), (59, 59), (61, 62), (64, 67),
            (71, 71), (73, 74), (79, 80), (83, 83), (87, 90), (92, 95), (97, 97), (106, 111),
            (119, 120), (123, 127), (135, 135), (140, 140), (143, 143), (148, 152), (155, 155),
            (158, 161), (165, 165), (169, 169), (175, 175), (179, 187), (196, 198), (201, 204),
            (207, 211), (214, 216), (225, 225), (232, 232), (235, 237), (245, 245), (248, 248),
            (251, 252), (255, 257), (260, 262), (264, 265), (273, 273), (276, 276), (281, 283),
            (294, 300), (303, 303), (314, 315), (318, 322), (328, 328), (337, 337), (342, 342),
            (348, 349), (352, 353), (357, 357), (360, 360), (363, 365), (368, 370), (373, 373),
            (376, 380), (384, 384), (388, 389), (392, 393), (397, 397), (403, 405), (409, 410),
            (413, 414), (418, 420), (423, 424), (426, 426), (433, 434), (437, 437), (439, 442),
            (448, 448), (450, 452), (456, 456), (459, 459), (461, 461), (468, 468), (471, 471),
            (475, 475), (478, 478), (480, 480), (483, 483), (486, 486), (489, 489), (512, 512),
            (518, 520), (527, 527), (536, 536), (539, 541), (544, 544), (547, 548), (553, 556),
            (562, 562), (565, 567), (570, 570), (577, 577), (582, 582), (588, 588), (591, 591),
            (594, 596), (599, 599), (602, 607), (629, 631), (634, 641), (658, 658), (663, 666),
            (669, 670), (677, 680), (686, 689), (692, 698), (707, 709), (716, 721), (726, 726),
            (729, 730), (733, 733), (735, 737), (742, 743), (750, 750), (753, 753), (757, 758),
            (761, 762), (767, 769), (774, 778), (784, 784), (789, 791), (794, 807), (813, 813),
            (816, 821), (824, 826), (829, 829), (833, 835), (838, 842), (845, 846), (851, 852),
            (855, 860), (865, 865), (868, 871), (875, 877), (880, 880), (883, 884), (891, 893),
            (898, 900), (903, 908), (911, 915), (918, 918), (921, 924), (929, 940), (943, 951),
            (955, 970), (975, 982), (985, 992), (996, 999), (1002, 1002), (1006, 1010),
            (1013, 1017), (1021, 1023), (1026, 1028), (1030, 1032), (1036, 1037), (1039, 1041),
            (1043, 1044), (1047, 1052), (1055, 1058), (1062, 1070), (1073, 1076), (1080, 1084),
            (1087, 1092), (1095, 1097), (1100, 1104), (1107, 1108), (1111, 1114), (1117, 1122),
            (1125, 1129), (1132, 1134), (1137, 1143), (1146, 1147), (1152, 1163), (1168, 1168),
            (1173, 1177), (1180, 1183), (1186, 1186), (1188, 1192), (1195, 1203), (1206, 1213),
            (1217, 1221),
        ],
        mixed: &[],
    },
];

/// Synchronous lookup against the built-in curated shonen anime table.
pub fn builtin_lookup(title: &str) -> Option<ShowFiller> {
    let key = clean_title(title);
    if key.is_empty() {
        return None;
    }
    let hit = BUILTIN.iter().find(|b| b.aliases.iter().any(|&a| a == key))?;
    let filler = hit.filler.to_vec();
    let mixed = hit.mixed.to_vec();
    let canon = complement_ranges(hit.total, &filler, &mixed);
    Some(ShowFiller {
        title: hit.title.to_string(),
        slug: hit.slug.to_string(),
        total: hit.total,
        filler,
        mixed,
        canon,
        source: "builtin".into(),
    })
}

// ---------------------------------------------------------------- AnimeFillerList live scraper

fn parse_ranges_fragment(fragment: &str) -> Vec<(i64, i64)> {
    let text = crate::re!(r"<[^>]+>").replace_all(fragment, " ");
    let mut out = vec![];
    for cap in crate::re!(r"(\d+)(?:\s*-\s*(\d+))?").captures_iter(&text) {
        let Ok(a) = cap[1].parse::<i64>() else { continue };
        let b = cap.get(2).and_then(|m| m.as_str().parse::<i64>().ok()).unwrap_or(a);
        if a > 0 && b >= a {
            out.push((a, b));
        }
    }
    merge_ranges(out)
}

/// Parse an AnimeFillerList show HTML page (`https://www.animefillerlist.com/shows/<slug>`).
pub fn parse_afl_html(html: &str, slug: &str, fallback_title: &str) -> Option<ShowFiller> {
    let mut filler = vec![];
    let mut mixed = vec![];
    let mut canon = vec![];

    // Condensed summary divs on animefillerlist.com contain only <span> and <a> elements (no nested <div>).
    let div_re = crate::re!(r#"(?is)<div[^>]*class="([^"]*)"[^>]*>(.*?)</div>"#);
    let eps_re = crate::re!(r#"(?is)<span[^>]*class="[^"]*Episodes[^"]*"[^>]*>(.*?)</span>"#);
    for cap in div_re.captures_iter(html) {
        let cls = cap[1].to_ascii_lowercase();
        let has = |tok: &str| cls.split_whitespace().any(|w| w == tok);
        let target = if has("mixed_canon/filler") {
            Some(&mut mixed)
        } else if has("filler") {
            Some(&mut filler)
        } else if has("manga_canon") || has("anime_canon") {
            Some(&mut canon)
        } else {
            None
        };
        if let Some(vec_out) = target {
            if let Some(ep_cap) = eps_re.captures(&cap[2]) {
                vec_out.extend(parse_ranges_fragment(&ep_cap[1]));
            }
        }
    }
    filler = merge_ranges(filler);
    mixed = merge_ranges(mixed);
    canon = merge_ranges(canon);

    // Also inspect table rows for exact episode total (and fallback if Condensed block wasn't rendered).
    let mut max_num = 0i64;
    for cap in crate::re!(r#"(?i)<td[^>]*class="[^"]*Number[^"]*"[^>]*>\s*(\d+)\s*</td>"#).captures_iter(html) {
        if let Ok(n) = cap[1].parse::<i64>() {
            max_num = max_num.max(n);
        }
    }

    if filler.is_empty() && mixed.is_empty() && canon.is_empty() {
        let row_re = crate::re!(r#"(?is)<tr[^>]*class="([^"]*)"[^>]*>.*?<td[^>]*class="[^"]*Number[^"]*"[^>]*>\s*(\d+)\s*</td>"#);
        let mut f_nums = vec![];
        let mut m_nums = vec![];
        let mut c_nums = vec![];
        for cap in row_re.captures_iter(html) {
            let cls = cap[1].to_ascii_lowercase();
            let has = |tok: &str| cls.split_whitespace().any(|w| w == tok);
            let Ok(n) = cap[2].parse::<i64>() else { continue };
            max_num = max_num.max(n);
            if has("mixed_canon/filler") {
                m_nums.push((n, n));
            } else if has("filler") {
                f_nums.push((n, n));
            } else if has("manga_canon") || has("anime_canon") {
                c_nums.push((n, n));
            }
        }
        filler = merge_ranges(f_nums);
        mixed = merge_ranges(m_nums);
        canon = merge_ranges(c_nums);
    }

    let total = max_num
        .max(max_range_end(&filler))
        .max(max_range_end(&mixed))
        .max(max_range_end(&canon));
    if total <= 0 {
        return None;
    }

    let title = crate::re!(r"(?is)<h1[^>]*>(.*?)</h1>")
        .captures(html)
        .map(|c| {
            let plain = crate::re!(r"<[^>]+>").replace_all(&c[1], "");
            let plain = plain.trim();
            plain.strip_suffix(" Filler List").unwrap_or(plain).trim().to_string()
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback_title.trim().to_string());

    Some(ShowFiller {
        title,
        slug: slug.to_string(),
        total,
        filler,
        mixed,
        canon,
        source: "animefillerlist".into(),
    })
}

fn candidate_slugs(norm_title: &str) -> Vec<String> {
    let mut slugs = vec![];
    let full = norm_title.split_whitespace().collect::<Vec<_>>().join("-");
    if !full.is_empty() {
        slugs.push(full);
    }
    // AnimeFillerList (Drupal) strips common English stopwords from generated URL slugs (e.g. "attack-titan").
    const STOP: &[&str] = &["the", "of", "on", "in", "a", "an", "to", "for", "and", "at", "by", "into"];
    let short: Vec<&str> = norm_title.split_whitespace().filter(|w| !STOP.contains(w)).collect();
    if !short.is_empty() {
        let s = short.join("-");
        if !slugs.contains(&s) {
            slugs.push(s);
        }
    }
    slugs
}

enum FetchOutcome {
    Hit(ShowFiller),
    NotFound,
    Error,
}

async fn fetch_afl_slug(app: &App, slug: &str, title: &str) -> FetchOutcome {
    let url = format!("https://www.animefillerlist.com/shows/{slug}");
    let resp = match app.http.get(&url).header("User-Agent", UA).timeout(Duration::from_secs(10)).send().await {
        Ok(r) => r,
        Err(_) => return FetchOutcome::Error,
    };
    if resp.status().as_u16() == 404 {
        return FetchOutcome::NotFound;
    }
    if !resp.status().is_success() {
        return FetchOutcome::Error;
    }
    let Ok(html) = resp.text().await else { return FetchOutcome::Error };
    match parse_afl_html(&html, slug, title) {
        Some(info) => FetchOutcome::Hit(info),
        None => FetchOutcome::NotFound,
    }
}

/// Fetch & cache the AnimeFillerList `/shows` index (~380 shows) to match alternate slugs.
async fn lookup_index_slug(app: &App, norm_key: &str) -> Option<String> {
    let idx = AFL_INDEX.get_or_init(|| Mutex::new((0, vec![])));
    let entries = {
        let g = idx.lock().unwrap();
        if !g.1.is_empty() && now_ms() - g.0 < INDEX_TTL_MS {
            Some(g.1.clone())
        } else {
            None
        }
    };
    let list = match entries {
        Some(v) => v,
        None => {
            let resp = app
                .http
                .get("https://www.animefillerlist.com/shows")
                .header("User-Agent", UA)
                .timeout(Duration::from_secs(12))
                .send()
                .await
                .ok()?;
            if !resp.status().is_success() {
                return None;
            }
            let html = resp.text().await.ok()?;
            let mut parsed = vec![];
            for cap in crate::re!(r#"(?i)href="/shows/([^"/?#]+)"[^>]*>([^<]+)</a>"#).captures_iter(&html) {
                let slug = cap[1].trim().to_string();
                let name = cap[2].trim().to_string();
                let lower = name.to_ascii_lowercase();
                if lower.contains("films") || lower.contains("film") || lower.contains("ovas") || lower.contains("oads") || lower.contains("specials") || lower.contains("movie") {
                    continue;
                }
                parsed.push((slug, name));
            }
            if !parsed.is_empty() {
                *idx.lock().unwrap() = (now_ms(), parsed.clone());
            }
            parsed
        }
    };
    for (slug, name) in &list {
        // AFL index entries often look like "Attack on Titan (Shingeki no Kyojin)" -> match either whole or parenthesized alias.
        if clean_title(name) == norm_key {
            return Some(slug.clone());
        }
        if let Some((main, rest)) = name.split_once('(') {
            if clean_title(main) == norm_key {
                return Some(slug.clone());
            }
            if let Some(alt) = rest.strip_suffix(')') {
                if clean_title(alt) == norm_key {
                    return Some(slug.clone());
                }
            }
        }
    }
    None
}

/// Look up filler info for an anime title:
/// 1. Built-in curated shonen table (instant, zero network)
/// 2. Persistent cache (`data/filler-cache.json`, 30-day TTL)
/// 3. Live fetch from `https://www.animefillerlist.com/shows/<slug>` (plus index slug fallback)
pub async fn lookup_show(app: &Arc<App>, title: &str) -> Option<ShowFiller> {
    if let Some(hit) = builtin_lookup(title) {
        return Some(hit);
    }
    let key = clean_title(title);
    if key.len() < 2 {
        return None;
    }
    let fc = cache(app);
    if let Some(cached) = fc.get_fresh(&key) {
        return cached;
    }

    let mut had_network_error = false;
    let tried = candidate_slugs(&key);
    for slug in &tried {
        match fetch_afl_slug(app, slug, title).await {
            FetchOutcome::Hit(info) => {
                fc.set_hit(key, &info);
                return Some(info);
            }
            FetchOutcome::NotFound => {}
            FetchOutcome::Error => {
                had_network_error = true;
                break;
            }
        }
    }

    if !had_network_error {
        if let Some(idx_slug) = lookup_index_slug(app, &key).await {
            if !tried.contains(&idx_slug) {
                match fetch_afl_slug(app, &idx_slug, title).await {
                    FetchOutcome::Hit(info) => {
                        fc.set_hit(key, &info);
                        return Some(info);
                    }
                    FetchOutcome::NotFound => {}
                    FetchOutcome::Error => {
                        had_network_error = true;
                    }
                }
            }
        }
    }

    if had_network_error {
        fc.get_stale(&key)
    } else {
        fc.set_miss(key);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_lookup_and_tags() {
        let naruto = builtin_lookup("EN - Naruto (2002) [DUB]").expect("Naruto builtin");
        assert_eq!(naruto.slug, "naruto");
        assert_eq!(naruto.total, 220);
        assert_eq!(episode_tag(&naruto, 0), "");
        assert_eq!(episode_tag(&naruto, 1), "canon");
        assert_eq!(episode_tag(&naruto, 7), "mixed");
        assert_eq!(episode_tag(&naruto, 26), "filler");
        assert_eq!(episode_tag(&naruto, 143), "filler");
        assert_eq!(episode_tag(&naruto, 220), "mixed");
        assert_eq!(episode_tag(&naruto, 221), "");
        assert_eq!(naruto.next_canon(142), Some(220));

        let ship = builtin_lookup("Naruto: Shippuden").expect("Shippuden builtin");
        assert_eq!(ship.slug, "naruto-shippuden");
        assert_eq!(episode_tag(&ship, 57), "filler");
        assert_eq!(episode_tag(&ship, 1), "mixed");
        assert_eq!(episode_tag(&ship, 20), "canon");

        let hxh = builtin_lookup("Hunter × Hunter (2011)").expect("HxH builtin");
        assert_eq!(hxh.slug, "hunter-x-hunter");
        assert_eq!(episode_tag(&hxh, 13), "filler");
        assert_eq!(episode_tag(&hxh, 12), "mixed");
        assert_eq!(episode_tag(&hxh, 14), "canon");

        let fmab = builtin_lookup("Fullmetal Alchemist: Brotherhood").expect("FMAB builtin");
        let fma = builtin_lookup("Fullmetal Alchemist").expect("FMA builtin");
        assert_eq!(fmab.total, 64);
        assert_eq!(fma.total, 51);
        assert_eq!(episode_tag(&fmab, 27), "filler");
        assert_eq!(episode_tag(&fma, 4), "filler");

        let op = builtin_lookup("One Piece").expect("One Piece builtin");
        assert_eq!(episode_tag(&op, 54), "filler");
        assert_eq!(episode_tag(&op, 45), "mixed");
        assert_eq!(episode_tag(&op, 50), "canon");
        assert_eq!(op.next_canon(53), Some(61));
    }

    #[test]
    fn parse_afl_condensed_and_table() {
        let sample = r#"
        <h1>Naruto Filler List</h1>
        <div id="Condensed">
          <div class="manga_canon"><span class="Label">Manga Canon Episodes:</span><span class="Episodes"><a href="javascript://">1-6</a>, <a href="javascript://">8</a></span></div>
          <div class="mixed_canon/filler"><span class="Label">Mixed Canon/Filler Episodes:</span><span class="Episodes"><a href="javascript://">7</a></span></div>
          <div class="filler"><span class="Label">Filler Episodes:</span><span class="Episodes"><a href="javascript://">9-10</a></span></div>
          <div class="anime_canon"><span class="Label">Anime Canon Episodes:</span><span class="Episodes"><a href="javascript://">11</a></span></div>
        </div>
        <table><tr class="anime_canon odd"><td class="Number">11</td></tr></table>
        "#;
        let info = parse_afl_html(sample, "naruto", "Naruto").expect("parse condensed");
        assert_eq!(info.title, "Naruto");
        assert_eq!(info.total, 11);
        assert_eq!(episode_tag(&info, 1), "canon");
        assert_eq!(episode_tag(&info, 7), "mixed");
        assert_eq!(episode_tag(&info, 9), "filler");
        assert_eq!(episode_tag(&info, 11), "canon");
        assert_eq!(episode_tag(&info, 12), "");
    }

    #[test]
    fn candidate_slugs_handles_stopwords() {
        assert_eq!(candidate_slugs("attack on titan"), vec!["attack-on-titan", "attack-titan"]);
        assert_eq!(candidate_slugs("bleach"), vec!["bleach"]);
    }
}
