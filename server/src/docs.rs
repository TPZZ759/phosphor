// Dedicated Documentaries hub: accurate documentary classification, word-boundary-safe
// topic tagging (9 curated topics), TMDB + local curated rows, hero selection, and a
// full-featured faceted documentary explorer.
use crate::app::App;
use crate::catalog::Item;
use crate::discover::watched_set;
use crate::meta::Meta;
use crate::rec::{hidden_ratings, sp};
use crate::tmdb::TTL_H;
use crate::util::{s, tmdb_img};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const DOC_TOPICS: &[(&str, &str)] = &[
    ("crime", "True Crime"),
    ("nature", "Nature & Wildlife"),
    ("history", "History & War"),
    ("science", "Science & Space"),
    ("sports", "Sports"),
    ("music", "Music & Arts"),
    ("bio", "Biography"),
    ("society", "Society & Culture"),
    ("food", "Food & Travel"),
];

pub fn topic_label(slug: &str) -> Option<&'static str> {
    DOC_TOPICS.iter().find(|(id, _)| *id == slug).map(|(_, label)| *label)
}

fn norm_country(c: &str) -> String {
    let up = c.trim().to_uppercase();
    if up == "UK" { "GB".to_string() } else { up }
}

fn country_aliases(code: &str) -> &'static str {
    match code {
        "US" => "usa united states american",
        "GB" => "uk united kingdom british england",
        "AU" => "australia australian",
        "CA" => "canada canadian",
        "NZ" => "new zealand",
        "FR" => "france french",
        "DE" => "germany german",
        "JP" => "japan japanese",
        "IE" => "ireland irish",
        "IT" => "italy italian",
        "ES" => "spain spanish",
        "KR" => "korea korean",
        "SE" => "sweden swedish",
        "NO" => "norway norwegian",
        "DK" => "denmark danish",
        _ => "",
    }
}

fn is_doc_with(it: &Item, cat_upper: &str, meta: Option<&Meta>) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") {
        return false;
    }
    if let Some(m) = meta {
        if m.g.contains(&99) {
            return true;
        }
    }
    if cat_upper.contains("DOCU") || cat_upper.contains("DOCUMENTAR") {
        // Explicit documentary categories are always docs unless TMDB says pure Animation (16) without 99.
        if let Some(m) = meta {
            if !m.g.contains(&16) {
                return true;
            }
        } else {
            return true;
        }
    } else if cat_upper.contains("DISCOVERY+") {
        // Discovery+ mixes reality shows with docs: require TMDB genre 99 once metadata is indexed.
        if meta.is_none() {
            return true;
        }
    }
    it.genre.to_lowercase().contains("documentary")
}

/// True for English non-sport documentaries (movies & series).
pub fn is_doc_item(app: &App, it: &Item) -> bool {
    if !it.en || it.sport || (it.k != "m" && it.k != "s") {
        return false;
    }
    let cat_upper = app
        .catalog()
        .cat_name(if it.k == "s" { "series" } else { "movie" }, &it.cat)
        .to_uppercase();
    let meta = app.meta_of(it);
    is_doc_with(it, &cat_upper, meta.as_ref())
}

fn topic_regex(slug: &str) -> &'static regex::Regex {
    match slug {
        "crime" => crate::re!(
            r"(?i)\b(true crime|true-crime|murder|murderer|murders|homicide|serial killer|killer|killers|killing|killings|manslaughter|assassination|assassin|slaying|strangler|ripper|zodiac|dahmer|bundy|gacy|manson|night stalker|unabomber|kidnap|kidnapping|abduction|abducted|missing person|disappearance|cold case|unsolved|forensic|forensics|autopsy|crime|crimes|criminal|criminals|felony|heist|robbery|burglary|cartel|mafia|mobster|gangster|gangsters|organized crime|syndicate|yakuza|narco|narcos|cocaine|drug lord|trafficking|smuggling|kingpin|hitman|contract killer|cult|cults|con artist|conman|scammer|scam|fraud|ponzi|embezzlement|swindler|catfish|stalker|stalking|arson|poisoning|fugitive|manhunt|prison|prisoner|inmate|inmates|jail|penitentiary|death row|convict|convicted|wrongful conviction|innocence project|detective|investigator|investigation|fbi|cia|dea|interpol|scotland yard|police|courtroom|prosecutor|indictment|verdict|trial of|suspect|confession|confessions|whistleblower|espionage|spy|spies)\b"
        ),
        "nature" => crate::re!(
            r"(?i)\b(nature|natural world|natural history|wildlife|wilderness|animal|animals|mammal|mammals|predator|predators|prey|safari|rainforest|rainforests|jungle|jungles|savanna|savannah|serengeti|amazon basin|amazon rainforest|antarctica|arctic|tundra|coral reef|coral reefs|ocean|oceans|underwater|marine life|deep sea|sea creatures|whale|whales|orca|orcas|shark|sharks|dolphin|dolphins|octopus|penguin|penguins|lion|lions|tiger|tigers|leopard|cheetah|jaguar|elephant|elephants|rhino|gorilla|gorillas|chimpanzee|chimpanzees|orangutan|primate|primates|bear|bears|grizzly|polar bear|wolf|wolves|fox|foxes|bird|birds|eagle|owl|falcon|parrot|reptile|reptiles|snake|snakes|crocodile|alligator|turtle|turtles|insect|insects|bee|bees|butterfly|butterflies|spider|spiders|fungi|mushroom|mushrooms|forest|forests|woodland|trees|flora|fauna|ecosystem|ecosystems|biodiversity|habitat|habitats|migration|endangered|extinction|conservation|conservationist|national park|national parks|yellowstone|galapagos|everglades|planet earth|our planet|blue planet|frozen planet|green planet|living planet|life on earth|attenborough|david attenborough|steve irwin|jane goodall|bbc earth|nat geo|national geographic|animal planet|dinosaur|dinosaurs|prehistoric|glacier|glaciers|volcano|volcanoes|volcanic|earthquakes?|tsunami|hurricane|tornado|wildfires?)\b"
        ),
        "history" => crate::re!(
            r"(?i)\b(history|historical|historian|historians|ancient|antiquity|archaeology|archaeologist|archaeological|civilization|civilizations|civilisation|civilisations|empire|empires|dynasty|dynasties|kingdom|kingdoms|pharaoh|pharaohs|egyptian|mesopotamia|babylon|roman empire|ancient rome|ancient greece|sparta|viking|vikings|medieval|middle ages|renaissance|tudor|tudors|victorian|colonial|colonialism|revolution|revolutionary|independence|founding fathers|civil war|world war|wwi|wwii|ww1|ww2|war|wars|warfare|wartime|battle|battles|battlefield|d-day|normandy|pearl harbor|pearl harbour|stalingrad|vietnam war|korean war|gulf war|iraq war|afghanistan war|cold war|holocaust|auschwitz|nazi|nazis|third reich|hitler|stalin|churchill|napoleon|genghis khan|alexander the great|julius caesar|cleopatra|pompeii|titanic|chernobyl|9/11|september 11|watergate|cuban missile|berlin wall|soviet|ussr|iron curtain|ottoman|samurai|aztec|mayan|inca|slavery|abolition|suffragette|prohibition|great depression|military|soldier|soldiers|troops|veteran|veterans|navy|air force|submarine|tanks|blitzkrieg|armistice|treaty|monarchy|monarch|monarchs|royal family|british royals?|emperor|empress|tsar|czar|sultan|shogun|centuries|archival|archive footage|forgotten history|secret history|untold history)\b"
        ),
        "science" => crate::re!(
            r"(?i)\b(science|scientific|scientist|scientists|physics|physicist|quantum|relativity|einstein|hawking|stephen hawking|carl sagan|neil degrasse|brian cox|chemistry|chemist|biology|biologist|microbiology|genetics|genetic|genome|dna|crispr|evolution|evolutionary|neuroscience|neuroscientist|brain|consciousness|astronomy|astronomer|astrophysics|astrophysicist|cosmology|cosmos|cosmic|universe|multiverse|space|spacecraft|spaceship|spaceflight|astronaut|astronauts|cosmonaut|nasa|spacex|esa|apollo|moon landing|lunar|mars|martian|jupiter|saturn|pluto|voyager|solar system|milky way|galaxy|galaxies|nebula|black hole|black holes|supernova|exoplanet|asteroid|comet|big bang|telescope|hubble|james webb|rocket|rockets|satellite|satellites|orbit|orbital|iss|space station|alien|aliens|ufo|ufos|uap|extraterrestrial|roswell|area 51|technology|technological|tech|silicon valley|artificial intelligence|ai|machine learning|robot|robots|robotics|computer|computers|computing|internet|cyber|cybersecurity|hacker|hackers|hacking|algorithm|algorithms|smartphone|engineering|engineer|engineers|megastructure|megastructures|skyscraper|aviation|aircraft|invention|inventions|inventor|inventors|innovation|nuclear|atomic|fusion|fission|particle|cern|hadron|climate change|global warming|renewable energy|solar power|medicine|medical|vaccine|vaccines|pandemic|epidemic|virus|bacteria|pathogen|disease|cancer research|surgeon|surgery|biotech|math|mathematics|mathematician|geometry|infinity|geology|geologist|paleontology|fossil|fossils)\b"
        ),
        "sports" => crate::re!(
            r"(?i)\b(sport|sports|athlete|athletes|athletic|athletics|olympic|olympics|olympian|paralympic|world cup|championship|championships|world champion|olympic champion|heavyweight champion|tournament|playoffs|nba finals|cup final|grand final|super bowl|nfl|nba|wnba|mlb|nhl|mls|ufc|mma|wwe|fifa|uefa|premier league|champions league|la liga|serie a|bundesliga|football|footballer|soccer|basketball|baseball|hockey|ice hockey|cricket|rugby|tennis|wimbledon|golf|pga|masters tournament|boxing|boxer|heavyweight|muay thai|jiu-jitsu|wrestling|wrestler|martial arts|karate|judo|fencing|formula\s*1|f1|grand prix|motorsport|motorsports|nascar|indycar|indy\s*500|motogp|rally|le mans|racing|racecar|tour de france|cycling|cyclist|marathon|triathlon|swimming|swimmer|freediving|cliff diving|gymnastics|gymnast|figure skating|skiing|snowboarding|surfing|surfer|skateboarding|skateboarder|skater|climbing|rock climbing|free solo|mountaineering|mountaineer|everest|alpinist|kayaking|rowing|sailing|yachting|poker|chess|grandmaster|esports|head coach|football coach|basketball coach|quarterback|striker|goalkeeper|pitcher|slam dunk|knockout|doping|stadium|drive to survive|all or nothing|the last dance|welcome to wrexham|sunderland|beckham|michael jordan|senna|schumacher|maradona|pele|messi|ronaldo|muhammad ali|mike tyson|mcgregor|federer|nadal|serena williams|tiger woods|simone biles|usain bolt|kobe|lance armstrong|tony hawk)\b"
        ),
        "music" => crate::re!(
            r"(?i)\b(music|musical|musician|musicians|singer|singers|songwriter|composer|composers|orchestra|symphony|opera|concert|concerts|live performance|world tour|concert tour|music tour|album|record label|recording studio|rock\s*and\s*roll|rock\s*n\s*roll|rock band|punk rock|heavy metal|hip-hop|hip\s*hop|rapper|rappers|jazz|blues|soul music|funk|reggae|folk music|country music|classical music|electronic music|techno|disco|pop star|pop icon|boy band|girl group|guitarist|drummer|pianist|violinist|dj|beatles|rolling stones|pink floyd|led zeppelin|david bowie|elvis|bob dylan|bob marley|johnny cash|amy winehouse|whitney houston|taylor swift|beyonce|billie eilish|nirvana|kurt cobain|metallica|freddie mercury|elton john|michael jackson|madonna|tupac|biggie|woodstock|coachella|glastonbury|motown|abbey road|grammy|grammys|art|arts|artist|artists|artistic|artwork|painter|painters|painting|paintings|sculpture|sculptor|museum|museums|gallery|galleries|banksy|picasso|van gogh|da vinci|warhol|basquiat|frida kahlo|graffiti|street art|photography|photographer|photographers|cinema|filmmaker|filmmakers|filmmaking|film director|screenwriter|cinematography|cinematographer|hollywood|movie magic|behind the scenes|making of|special effects|visual effects|animation studio|broadway|theater|theatre|playwright|ballet|ballerina|dancer|dancers|choreographer|choreography|fashion|fashion designer|haute couture|runway|vogue|architecture|architect|architects|industrial design|graphic design|literature|author|novelist|poet|poetry|comedy|comedian|comedians|stand-up|satire|cartoonist|comic book|manga)\b"
        ),
        "bio" => crate::re!(
            r"(?i)\b(biography|biographical|biopic|portrait|portraits|memoir|autobiography|life story|my story|his life|her life|personal life|early life|life and times|life and career|life and legacy|life and death|rise and fall|untold story|personal journey|intimate portrait|intimate look|in his own words|in her own words|in their own words|profile of|tribute to|legacy of|chronicles the life|traces the life|follows the life|extraordinary life|remarkable life|inspiring story|icon|legendary|pioneer|visionary|activist|activists|philanthropist|billionaire|mogul|tycoon|entrepreneur|founder|celebrity|superstar|presidential|president|prime minister|politician|politicians|statesman|first lady|pope|dalai lama|nelson mandela|mahatma gandhi|martin luther king|malcolm x|barack obama|michelle obama|donald trump|steve jobs|elon musk|bill gates|jeff bezos|mark zuckerberg|oprah|princess diana|queen elizabeth|albert einstein|marie curie|nikola tesla|leonardo da vinci|arnold schwarzenegger|sylvester stallone|val kilmer|michael j\. fox|robin williams|anthony bourdain|stan lee|fred rogers|ruth bader ginsburg|malala)\b"
        ),
        "society" => crate::re!(
            r"(?i)\b(society|societal|social|culture|cultural|subculture|community|communities|politics|political|democracy|election|elections|government|capitalism|socialism|economy|economic|economics|finance|financial|wall street|stock market|banking|banker|bankers|bankruptcy|recession|inflation|housing crisis|poverty|inequality|wealth|billionaires|corporate|corporation|corporations|monopoly|big tech|big pharma|startup|consumerism|advertising|marketing|media|journalism|journalist|journalists|newsroom|newspaper|press|propaganda|misinformation|disinformation|conspiracy|conspiracies|social media|influencer|influencers|internet culture|meme|memes|addiction|opioid|mental health|psychology|psychologist|therapy|depression|trauma|education|public school|high school|school system|university|college|healthcare|immigration|immigrant|immigrants|refugee|refugees|border|human rights|civil rights|feminism|feminist|gender|lgbtq|queer|transgender|racism|racist|racial|apartheid|segregation|discrimination|protest|protests|protesters|activism|social movement|grassroots|justice|climate crisis|environment|environmental|pollution|plastic|sustainability|vegan|factory farming|religion|religious|church|evangelical|scientology|mormon|faith|spirituality|philosophy|ethics|modern life|millennials|gen z|dating|marriage|family life|parenting|aging|longevity|cryptocurrency|crypto|bitcoin|nft|gambling|casino|las vegas|sex industry|pornography|taboo)\b"
        ),
        _ => crate::re!(
            r"(?i)\b(food|foods|foodie|culinary|cuisine|cuisines|chef|chefs|cooking|cookery|cookbook|kitchen|kitchens|restaurant|restaurants|michelin|gastronomy|dining|street food|fast food|comfort food|baking|baker|bakery|pastry|patisserie|barbecue|bbq|grill|grilling|sushi|ramen|noodles|pizza|pasta|tacos|curry|chocolate|coffee|espresso|tea ceremony|wine|winery|wineries|vineyard|sommelier|beer|brewery|brewing|whisky|whiskey|bourbon|tequila|distillery|cocktail|cocktails|cheese|bread|sourdough|butcher|seafood|foraging|farm-to-table|ingredients|flavor|flavour|spices|tasting menu|feast|recipe|recipes|travel|traveler|traveller|traveling|travelling|journey across|road trip|backpacking|wanderlust|nomad|nomadic|globetrotting|globe-trotting|around the world|train journey|railway journey|great railway|scenic journey|culinary journey|destinations?|uncharted|bourdain|anthony bourdain|parts unknown|no reservations|gordon ramsay|jamie oliver|stanley tucci|phil rosenthal|somebody feed phil|chef'?s table|ugly delicious|salt fat acid heat|rick steves|michael palin|joanna lumley|samin nosrat|david chang|nigella)\b"
        ),
    }
}

/// Returns (1..=3 topic slugs, explicit_match).
fn classify_topics(
    title: &str,
    overview: &str,
    plot: &str,
    genre: &str,
    cat_name: &str,
    tmdb_genres: &[i64],
) -> (Vec<&'static str>, bool) {
    let head = format!("{title} {genre} {cat_name}");
    let body = if !overview.is_empty() && !plot.is_empty() && overview != plot {
        format!("{overview} {plot}")
    } else if !overview.is_empty() {
        overview.to_string()
    } else {
        plot.to_string()
    };
    // Avoid tagging Star Wars / metaphorical "empire" documentaries as History & War.
    let sw = crate::re!(r"(?i)\b(star wars|empire of dreams|empire state|media empire|fashion empire|business empire|music empire|drug empire|tech empire)\b");
    let head_hist = if sw.is_match(&head) { sw.replace_all(&head, " ").into_owned() } else { head.clone() };
    let body_hist = if sw.is_match(&body) { sw.replace_all(&body, " ").into_owned() } else { body.clone() };

    let mut scored: Vec<(usize, &'static str, i32)> = Vec::with_capacity(DOC_TOPICS.len());
    for (idx, (slug, _)) in DOC_TOPICS.iter().enumerate() {
        let mut sc = 0i32;
        match *slug {
            "crime" if tmdb_genres.contains(&80) => sc += 4,
            "history" if tmdb_genres.iter().any(|g| matches!(g, 36 | 10752 | 10768)) => sc += 4,
            "music" if tmdb_genres.contains(&10402) => sc += 4,
            _ => {}
        }
        let re = topic_regex(slug);
        let (h, b) = if *slug == "history" { (head_hist.as_str(), body_hist.as_str()) } else { (head.as_str(), body.as_str()) };
        let head_hits = re.find_iter(h).count().min(4) as i32;
        let body_hits = re.find_iter(b).count().min(6) as i32;
        sc += head_hits * 3 + body_hits;
        if sc > 0 {
            scored.push((idx, *slug, sc));
        }
    }
    if scored.is_empty() {
        return (vec!["society"], false);
    }
    scored.sort_by(|a, b| b.2.cmp(&a.2).then(a.0.cmp(&b.0)));
    (scored.into_iter().take(3).map(|x| x.1).collect(), true)
}

/// Classify a documentary into 1–3 topic slugs from `DOC_TOPICS`.
pub fn doc_topics(app: &App, it: &Item) -> Vec<&'static str> {
    let cat_name = app.catalog().cat_name(if it.k == "s" { "series" } else { "movie" }, &it.cat);
    let meta = app.meta_of(it);
    classify_topics(
        &it.title,
        meta.as_ref().map(|m| m.ov.as_str()).unwrap_or(""),
        &it.plot,
        &it.genre,
        &cat_name,
        meta.as_ref().map(|m| m.g.as_slice()).unwrap_or(&[]),
    )
    .0
}

/// Human-readable labels corresponding to `doc_topics(app, it)`.
pub fn doc_topic_labels(app: &App, it: &Item) -> Vec<&'static str> {
    doc_topics(app, it).into_iter().filter_map(topic_label).collect()
}

/// Bayesian rating tuned for documentaries (smoothing = 80 so acclaimed docs shine).
fn doc_bayes(m: &Meta) -> f64 {
    let v = m.vc.max(0) as f64;
    (v / (v + 80.0)) * m.va + (80.0 / (v + 80.0)) * 6.6
}

fn item_countries(it: &Item, meta: Option<&Meta>) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    if let Some(m) = meta {
        for c in &m.oc {
            let code = norm_country(c);
            if !code.is_empty() && !out.contains(&code) {
                out.push(code);
            }
        }
    }
    if out.is_empty() && !it.origin.is_empty() {
        let code = norm_country(&it.origin);
        if !code.is_empty() {
            out.push(code);
        }
    }
    out
}

struct DocEntry {
    item: Item,
    meta: Option<Meta>,
    topics: Vec<&'static str>,
    explicit_topic: bool,
    platforms: Vec<&'static str>,
    countries: Vec<String>,
    is_uhd: bool,
}

impl DocEntry {
    fn bayes(&self) -> f64 {
        match &self.meta {
            Some(m) if m.va > 0.0 => doc_bayes(m),
            _ => (self.item.rating / 10.0).clamp(0.0, 1.0) * 6.5,
        }
    }
    fn pop(&self) -> f64 {
        self.meta.as_ref().map(|m| m.p).unwrap_or(self.item.rating).max(0.0)
    }
    fn votes(&self) -> i64 {
        self.meta.as_ref().map(|m| m.vc).unwrap_or(0)
    }
    fn year(&self) -> i32 {
        let my = self.meta.as_ref().map(|m| m.year()).unwrap_or(0);
        if my > 0 { my } else { self.item.year.unwrap_or(0) }
    }
    fn sort_date(&self) -> String {
        if let Some(m) = &self.meta {
            if !m.d.is_empty() {
                return m.d.clone();
            }
        }
        let y = self.year();
        if y > 0 { format!("{y:04}-01-01") } else { String::new() }
    }
    fn runtime(&self) -> i64 {
        self.meta.as_ref().map(|m| m.rt).unwrap_or(0)
    }
    fn has_poster(&self) -> bool {
        !self.item.poster.is_empty() || self.meta.as_ref().map_or(false, |m| !m.pp.is_empty())
    }
    fn has_backdrop(&self, tm: Option<&Value>) -> bool {
        !self.item.backdrop.is_empty()
            || self.meta.as_ref().map_or(false, |m| !m.bd.is_empty())
            || tm.and_then(|t| t["backdrop_path"].as_str()).map_or(false, |b| !b.is_empty())
    }
    fn has_overview(&self, tm: Option<&Value>) -> bool {
        self.meta.as_ref().map_or(false, |m| m.ov.len() >= 20)
            || self.item.plot.len() >= 20
            || tm.and_then(|t| t["overview"].as_str()).map_or(false, |o| o.len() >= 20)
    }
    fn hidden(&self, hide: &HashSet<String>) -> bool {
        if let Some(t) = &self.item.tmdb {
            let mt = if self.item.k == "s" { "tv" } else { "movie" };
            return hide.contains(&format!("{mt}:{t}"));
        }
        false
    }
}

fn doc_pool(app: &App) -> Vec<DocEntry> {
    let cat = app.catalog();
    let db = app.meta.read().unwrap();
    let mut out: Vec<DocEntry> = Vec::new();
    for (kind, mt, list) in [("movie", "movie", &cat.movies), ("series", "tv", &cat.series)] {
        for it in list {
            if !it.en || it.sport {
                continue;
            }
            let cat_name = cat.cat_name(kind, &it.cat);
            let cat_upper = cat_name.to_uppercase();
            let meta = it
                .tmdb
                .as_ref()
                .and_then(|t| db.get(&format!("{mt}:{t}")))
                .filter(|m| !m.missing);
            if !is_doc_with(it, &cat_upper, meta) {
                continue;
            }
            let (topics, explicit_topic) = classify_topics(
                &it.title,
                meta.map(|m| m.ov.as_str()).unwrap_or(""),
                &it.plot,
                &it.genre,
                &cat_name,
                meta.map(|m| m.g.as_slice()).unwrap_or(&[]),
            );
            let platforms = cat.item_platforms(it);
            let countries = item_countries(it, meta);
            let is_uhd = it.uhd || it.variants.iter().any(|v| v.uhd);
            out.push(DocEntry {
                item: it.clone(),
                meta: meta.cloned(),
                topics,
                explicit_topic,
                platforms,
                countries,
                is_uhd,
            });
        }
    }
    out
}

/// Enriched documentary card JSON (adds `isDoc`, `docTopics`, `docTopicSlugs`).
pub fn doc_card(app: &App, it: &Item, tm: Option<&Value>) -> Value {
    let slugs = doc_topics(app, it);
    doc_card_with(app, it, tm, &slugs)
}

fn doc_card_with(app: &App, it: &Item, tm: Option<&Value>, slugs: &[&'static str]) -> Value {
    let mut c = app.card_meta(it);
    if let Some(t) = tm {
        if s(&c["backdrop"]).is_empty() {
            if let Some(b) = t["backdrop_path"].as_str().filter(|x| !x.is_empty()) {
                c["backdrop"] = json!(tmdb_img(b, "w1280"));
            }
        }
        if s(&c["poster"]).is_empty() {
            if let Some(p) = t["poster_path"].as_str().filter(|x| !x.is_empty()) {
                c["poster"] = json!(tmdb_img(p, "w342"));
            }
        }
        if s(&c["overview"]).is_empty() {
            if let Some(ov) = t["overview"].as_str().filter(|x| !x.is_empty()) {
                c["overview"] = json!(ov);
            }
        }
        if c["rating"].as_f64().unwrap_or(0.0) <= 0.0 {
            if let Some(v) = t["vote_average"].as_f64().filter(|&v| v > 0.0) {
                c["rating"] = json!((v * 10.0).round() / 10.0);
            }
        }
        if c["year"].is_null() {
            let d = t["release_date"]
                .as_str()
                .filter(|x| !x.is_empty())
                .or(t["first_air_date"].as_str())
                .unwrap_or("");
            if let Some(y) = d.get(0..4).and_then(|y| y.parse::<i32>().ok()) {
                c["year"] = json!(y);
            }
        }
    }
    let labels: Vec<&'static str> = slugs.iter().filter_map(|sl| topic_label(sl)).collect();
    c["isDoc"] = json!(true);
    c["docTopics"] = json!(labels);
    c["docTopicSlugs"] = json!(slugs);
    c
}

fn compute_facets(entries: &[&DocEntry]) -> (Vec<Value>, Vec<Value>, Vec<Value>, usize, usize, usize) {
    let mut topic_counts: HashMap<&str, usize> = HashMap::new();
    let mut plat_counts: HashMap<&str, usize> = HashMap::new();
    let mut country_counts: HashMap<&str, usize> = HashMap::new();
    let (mut movies, mut series, mut uhd) = (0usize, 0usize, 0usize);

    for e in entries {
        if e.item.k == "s" { series += 1; } else { movies += 1; }
        if e.is_uhd { uhd += 1; }
        for t in &e.topics {
            *topic_counts.entry(*t).or_default() += 1;
        }
        for p in &e.platforms {
            *plat_counts.entry(*p).or_default() += 1;
        }
        for c in &e.countries {
            *country_counts.entry(c.as_str()).or_default() += 1;
        }
    }

    let topics: Vec<Value> = DOC_TOPICS
        .iter()
        .map(|(id, label)| json!({ "id": id, "label": label, "count": topic_counts.get(*id).copied().unwrap_or(0) }))
        .collect();

    let mut plats: Vec<(&str, usize)> = plat_counts.into_iter().collect();
    plats.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let platforms: Vec<Value> = plats
        .into_iter()
        .take(12)
        .map(|(name, count)| json!({ "name": name, "count": count }))
        .collect();

    let mut ctrs: Vec<(&str, usize)> = country_counts.into_iter().collect();
    ctrs.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    let countries: Vec<Value> = ctrs
        .into_iter()
        .take(12)
        .map(|(code, count)| json!({ "code": code, "count": count }))
        .collect();

    (topics, platforms, countries, movies, series, uhd)
}

fn push_row(rows: &mut Vec<Value>, id: &str, title: &str, sub: Option<&str>, items: Vec<Value>) {
    if items.len() < 3 {
        return;
    }
    let mut r = json!({ "id": id, "title": title, "items": items });
    if let Some(sub) = sub {
        r["sub"] = json!(sub);
    }
    rows.push(r);
}

/// GET /api/docs: Hero + curated documentary rows + facet summaries & counts.
pub async fn page(app: Arc<App>) -> Value {
    let pool = doc_pool(&app);
    let hide = hidden_ratings(&app);
    let (topics_facet, platforms_facet, countries_facet, movies_cnt, series_cnt, uhd_cnt) = {
        let all_refs: Vec<&DocEntry> = pool.iter().collect();
        compute_facets(&all_refs)
    };

    let mut by_key: HashMap<(&'static str, i64), usize> = HashMap::with_capacity(pool.len());
    for (idx, e) in pool.iter().enumerate() {
        by_key.insert((e.item.k, e.item.id), idx);
    }

    // TMDB trending & popular documentaries intersected with our library.
    let mut tmdb_interleaved: Vec<(usize, Value)> = vec![];
    if app.tmdb.enabled() {
        let (tr_mv, tr_tv) = tokio::join!(
            app.tmdb.pages(
                "/discover/movie",
                3,
                [sp("with_genres", "99"), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "30")],
                6 * TTL_H,
            ),
            app.tmdb.pages(
                "/discover/tv",
                3,
                [sp("with_genres", "99"), sp("sort_by", "popularity.desc"), sp("vote_count.gte", "20")],
                6 * TTL_H,
            )
        );
        let cat = app.catalog();
        let match_tmdb = |results: &[Value], mt: &str| -> Vec<(usize, Value)> {
            let mut hits = vec![];
            let mut seen = HashSet::new();
            for r in results {
                let tid = r["id"].as_i64().map(|x| x.to_string()).unwrap_or_else(|| s(&r["id"]));
                if tid.is_empty() || hide.contains(&format!("{mt}:{tid}")) {
                    continue;
                }
                let Some(it) = cat.by_tmdb(mt, &tid) else { continue };
                let Some(&idx) = by_key.get(&(it.k, it.id)) else { continue };
                if !seen.insert(idx) {
                    continue;
                }
                hits.push((idx, r.clone()));
            }
            hits
        };
        let tv_hits = match_tmdb(&tr_tv, "tv");
        let mv_hits = match_tmdb(&tr_mv, "movie");
        let mut seen = HashSet::new();
        let max_len = tv_hits.len().max(mv_hits.len());
        for i in 0..max_len {
            if let Some((idx, r)) = tv_hits.get(i) {
                if seen.insert(*idx) {
                    tmdb_interleaved.push((*idx, r.clone()));
                }
            }
            if let Some((idx, r)) = mv_hits.get(i) {
                if seen.insert(*idx) {
                    tmdb_interleaved.push((*idx, r.clone()));
                }
            }
        }
    }

    let visible: Vec<usize> = pool
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.hidden(&hide))
        .map(|(i, _)| i)
        .collect();

    // Hero: top 6 acclaimed documentaries with crisp backdrop & overview,
    // prioritizing TMDB trending ∩ library, favoring 4K & flagship topics.
    let flagship = |slug: &str| matches!(slug, "nature" | "crime" | "science" | "history");
    let mut hero_cands: Vec<(f64, usize, Option<Value>)> = vec![];
    let mut hero_seen: HashSet<usize> = HashSet::new();
    for (rank, (idx, r)) in tmdb_interleaved.iter().enumerate() {
        let e = &pool[*idx];
        if !e.has_backdrop(Some(r)) || !e.has_overview(Some(r)) {
            continue;
        }
        hero_seen.insert(*idx);
        let mut sc = e.bayes() + (3.2 - (rank as f64 * 0.06)).max(0.4) + (e.pop() + 1.0).log10() * 0.5;
        if e.is_uhd { sc += 1.1; }
        if e.topics.first().map_or(false, |t| flagship(t)) { sc += 0.8; }
        else if e.topics.iter().any(|t| flagship(t)) { sc += 0.4; }
        hero_cands.push((sc, *idx, Some(r.clone())));
    }
    for &idx in &visible {
        if hero_seen.contains(&idx) {
            continue;
        }
        let e = &pool[idx];
        if !e.has_backdrop(None) || !e.has_overview(None) {
            continue;
        }
        let mut sc = e.bayes() + (e.pop() + 1.0).log10() * 0.55;
        if e.is_uhd { sc += 1.1; }
        if e.topics.first().map_or(false, |t| flagship(t)) { sc += 0.8; }
        else if e.topics.iter().any(|t| flagship(t)) { sc += 0.4; }
        if e.votes() < 20 { sc -= 1.4; }
        hero_cands.push((sc, idx, None));
    }
    hero_cands.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut hero_picks: Vec<(usize, Option<Value>)> = vec![];
    let mut topic_used: HashMap<&str, usize> = HashMap::new();
    let mut picked_idx: HashSet<usize> = HashSet::new();
    for (_, idx, tm) in &hero_cands {
        if hero_picks.len() >= 6 { break; }
        let primary = pool[*idx].topics.first().copied().unwrap_or("society");
        let used = topic_used.get(primary).copied().unwrap_or(0);
        if used >= 2 { continue; }
        *topic_used.entry(primary).or_default() += 1;
        picked_idx.insert(*idx);
        hero_picks.push((*idx, tm.clone()));
    }
    if hero_picks.len() < 6 {
        for (_, idx, tm) in &hero_cands {
            if hero_picks.len() >= 6 { break; }
            if picked_idx.insert(*idx) {
                hero_picks.push((*idx, tm.clone()));
            }
        }
    }
    if hero_picks.len() < 6 {
        let mut fallback: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|i| !picked_idx.contains(i) && pool[*i].has_backdrop(None))
            .collect();
        fallback.sort_by(|a, b| pool[*b].bayes().total_cmp(&pool[*a].bayes()));
        for idx in fallback.into_iter().take(6 - hero_picks.len()) {
            hero_picks.push((idx, None));
        }
    }
    let hero: Vec<Value> = hero_picks
        .into_iter()
        .map(|(idx, tm)| doc_card_with(&app, &pool[idx].item, tm.as_ref(), &pool[idx].topics))
        .collect();

    // Curated rows (each emitted only if >= 3 items).
    let mut rows: Vec<Value> = vec![];

    // 1. Trending & Acclaimed Documentaries
    let mut trend_items: Vec<Value> = Vec::with_capacity(40);
    let mut trend_seen: HashSet<usize> = HashSet::new();
    for (idx, r) in tmdb_interleaved.iter().take(40) {
        if trend_seen.insert(*idx) {
            trend_items.push(doc_card_with(&app, &pool[*idx].item, Some(r), &pool[*idx].topics));
        }
    }
    if trend_items.len() < 40 {
        let mut local_top: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|i| !trend_seen.contains(i) && pool[*i].has_poster())
            .collect();
        local_top.sort_by(|a, b| {
            let sa = pool[*a].bayes() * 1.4 + (pool[*a].pop() + 1.0).log10() * 0.8;
            let sb = pool[*b].bayes() * 1.4 + (pool[*b].pop() + 1.0).log10() * 0.8;
            sb.total_cmp(&sa)
        });
        for idx in local_top.into_iter().take(40 - trend_items.len()) {
            trend_items.push(doc_card_with(&app, &pool[idx].item, None, &pool[idx].topics));
        }
    }
    push_row(&mut rows, "doc-trend", "Trending & Acclaimed Documentaries", Some("Popular & critically acclaimed"), trend_items);

    // 2. 4K & Dolby Vision Documentaries
    let mut uhd_ids: Vec<usize> = visible.iter().copied().filter(|i| pool[*i].is_uhd).collect();
    uhd_ids.sort_by(|a, b| {
        let sa = pool[*a].bayes() * 1.2 + (pool[*a].pop() + 1.0).log10() * 0.6;
        let sb = pool[*b].bayes() * 1.2 + (pool[*b].pop() + 1.0).log10() * 0.6;
        sb.total_cmp(&sa)
    });
    let uhd_items: Vec<Value> = uhd_ids
        .into_iter()
        .take(40)
        .map(|i| doc_card_with(&app, &pool[i].item, None, &pool[i].topics))
        .collect();
    push_row(&mut rows, "doc-4k", "4K & Dolby Vision Documentaries", Some("Ultra HD showcase"), uhd_items);

    // Helper for topic rows (3..11).
    let topic_row = |slug: &'static str, favor_uhd: bool| -> Vec<Value> {
        let mut ids: Vec<usize> = visible
            .iter()
            .copied()
            .filter(|i| pool[*i].topics.iter().any(|t| *t == slug))
            .collect();
        ids.sort_by(|a, b| {
            let score = |i: usize| {
                let e = &pool[i];
                let mut sc = e.bayes() * 1.15 + (e.pop() + 1.0).log10() * 0.7;
                if e.topics.first().copied() == Some(slug) { sc += 0.6; }
                if slug == "society" && !e.explicit_topic { sc -= 2.0; }
                if favor_uhd && e.is_uhd { sc += 1.2; }
                sc
            };
            score(*b).total_cmp(&score(*a))
        });
        ids.into_iter()
            .take(40)
            .map(|i| doc_card_with(&app, &pool[i].item, None, &pool[i].topics))
            .collect()
    };

    // 3..11. Curated Topic Rows
    push_row(&mut rows, "doc-crime", "True Crime & Investigations", None, topic_row("crime", false));
    push_row(&mut rows, "doc-nature", "Nature, Wildlife & Planet Earth", None, topic_row("nature", true));
    push_row(&mut rows, "doc-history", "History, War & Civilisations", None, topic_row("history", false));
    push_row(&mut rows, "doc-science", "Science, Space & Tech", None, topic_row("science", false));
    push_row(&mut rows, "doc-sports", "Sports Documentaries", None, topic_row("sports", false));
    push_row(&mut rows, "doc-music", "Music, Film & Arts", None, topic_row("music", false));
    push_row(&mut rows, "doc-bio", "Biographies & Portraits", None, topic_row("bio", false));
    push_row(&mut rows, "doc-society", "Society, Money & Culture", None, topic_row("society", false));
    push_row(&mut rows, "doc-food", "Food, Travel & Exploration", None, topic_row("food", false));

    // 12. Limited Docuseries (1 Season)
    let mut lim_ids: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|i| pool[*i].item.k == "s" && pool[*i].meta.as_ref().map_or(false, |m| m.ns == 1))
        .collect();
    lim_ids.sort_by(|a, b| {
        let sa = pool[*a].bayes() * 1.2 + (pool[*a].pop() + 1.0).log10() * 0.7;
        let sb = pool[*b].bayes() * 1.2 + (pool[*b].pop() + 1.0).log10() * 0.7;
        sb.total_cmp(&sa)
    });
    let lim_items: Vec<Value> = lim_ids
        .into_iter()
        .take(40)
        .map(|i| doc_card_with(&app, &pool[i].item, None, &pool[i].topics))
        .collect();
    push_row(&mut rows, "doc-limited", "Limited Docuseries (1 Season)", Some("Complete in one season"), lim_items);

    // 13. Short Documentaries (Under 90 min)
    let mut short_ids: Vec<usize> = visible
        .iter()
        .copied()
        .filter(|i| pool[*i].item.k == "m" && { let rt = pool[*i].runtime(); rt > 0 && rt < 90 })
        .collect();
    short_ids.sort_by(|a, b| {
        let sa = pool[*a].bayes() * 1.2 + (pool[*a].pop() + 1.0).log10() * 0.6;
        let sb = pool[*b].bayes() * 1.2 + (pool[*b].pop() + 1.0).log10() * 0.6;
        sb.total_cmp(&sa)
    });
    let short_items: Vec<Value> = short_ids
        .into_iter()
        .take(40)
        .map(|i| doc_card_with(&app, &pool[i].item, None, &pool[i].topics))
        .collect();
    push_row(&mut rows, "doc-short", "Short Documentaries (Under 90 min)", None, short_items);

    // 14. Recently Added Documentaries
    let mut added_ids: Vec<usize> = visible.iter().copied().filter(|i| pool[*i].has_poster()).collect();
    if added_ids.len() < 3 {
        added_ids = visible.clone();
    }
    added_ids.sort_by(|a, b| pool[*b].item.added.cmp(&pool[*a].item.added));
    let added_items: Vec<Value> = added_ids
        .into_iter()
        .take(40)
        .map(|i| doc_card_with(&app, &pool[i].item, None, &pool[i].topics))
        .collect();
    push_row(&mut rows, "doc-added", "Recently Added Documentaries", None, added_items);

    json!({
        "hero": hero,
        "rows": rows,
        "topics": topics_facet,
        "platforms": platforms_facet,
        "countries": countries_facet,
        "counts": {
            "total": pool.len(),
            "movies": movies_cnt,
            "series": series_cnt,
            "uhd": uhd_cnt
        }
    })
}

/// GET /api/docs/browse: filterable, searchable documentary explorer with live facets.
pub fn browse(app: &App, q: &HashMap<String, String>) -> Value {
    let get = |k: &str| q.get(k).cloned().unwrap_or_default();
    let terms: Vec<String> = get("q")
        .to_lowercase()
        .split_whitespace()
        .map(String::from)
        .collect();
    let kind = get("kind").trim().to_lowercase();
    let req_topics: Vec<String> = get("topic")
        .split(',')
        .map(|x| x.trim().to_lowercase())
        .filter(|x| !x.is_empty())
        .collect();
    let topic_all = get("tmode").eq_ignore_ascii_case("all");
    let req_platforms: Vec<String> = get("platform")
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect();
    let req_countries: Vec<String> = get("country")
        .split(',')
        .map(|x| norm_country(x))
        .filter(|x| !x.is_empty())
        .collect();
    let lang = get("lang").trim().to_lowercase();
    let y1 = get("y1").parse::<i32>().unwrap_or(0);
    let y2 = {
        let v = get("y2").parse::<i32>().unwrap_or(0);
        if v == 0 { 9999 } else { v }
    };
    let rmin = get("rmin").parse::<f64>().unwrap_or(0.0);
    let rtmin = get("rtmin").parse::<i64>().unwrap_or(0);
    let rtmax = get("rtmax").parse::<i64>().unwrap_or(0);
    let format = get("format").trim().to_lowercase();
    let uhd = get("uhd") == "1";
    let unwatched = get("unwatched") == "1";
    let sort = get("sort").trim().to_lowercase();
    let page = get("page").parse::<usize>().unwrap_or(0);
    let per = get("per").parse::<usize>().unwrap_or(60).clamp(1, 120);

    let seen = if unwatched { Some(watched_set(app)) } else { None };
    let pool = doc_pool(app);
    let cat = app.catalog();

    let mut matched: Vec<&DocEntry> = pool
        .iter()
        .filter(|e| {
            // kind
            match kind.as_str() {
                "movie" if e.item.k != "m" => return false,
                "tv" | "series" if e.item.k != "s" => return false,
                _ => {}
            }
            // format
            match format.as_str() {
                "limited" => {
                    if !(e.item.k == "s" && e.meta.as_ref().map_or(false, |m| m.ns == 1)) {
                        return false;
                    }
                }
                "multi" => {
                    if !(e.item.k == "s" && e.meta.as_ref().map_or(false, |m| m.ns >= 2)) {
                        return false;
                    }
                }
                "feature" => {
                    if e.item.k != "m" {
                        return false;
                    }
                }
                _ => {}
            }
            // uhd
            if uhd && !e.is_uhd {
                return false;
            }
            // topic (OR by default, AND when tmode=all)
            if !req_topics.is_empty() {
                let ok = if topic_all {
                    req_topics.iter().all(|t| e.topics.iter().any(|x| *x == t.as_str()))
                } else {
                    req_topics.iter().any(|t| e.topics.iter().any(|x| *x == t.as_str()))
                };
                if !ok {
                    return false;
                }
            }
            // platform
            if !req_platforms.is_empty() && !req_platforms.iter().any(|p| cat.item_has_platform(&e.item, p)) {
                return false;
            }
            // country
            if !req_countries.is_empty() && !req_countries.iter().any(|c| e.countries.contains(c)) {
                return false;
            }
            // lang
            if !lang.is_empty() {
                let ml = e.meta.as_ref().map(|m| m.l.as_str()).unwrap_or("");
                match lang.as_str() {
                    "en" => if ml != "en" { return false; }
                    "nonen" => if ml.is_empty() || ml == "en" { return false; }
                    "nordic" => if !matches!(ml, "sv" | "da" | "no" | "nb" | "fi" | "is") { return false; }
                    other => if ml != other { return false; }
                }
            }
            // year range
            if y1 > 0 || y2 < 9999 {
                let yr = e.year();
                if yr == 0 || yr < y1 || yr > y2 {
                    return false;
                }
            }
            // minimum TMDB vote_average (vc >= 15 for docs)
            if rmin > 0.0 {
                let ok = e.meta.as_ref().map_or(false, |m| m.va >= rmin && m.vc >= 15);
                if !ok {
                    return false;
                }
            }
            // runtime range
            if rtmin > 0 || rtmax > 0 {
                let rt = e.runtime();
                if rtmin > 0 && rt < rtmin {
                    return false;
                }
                if rtmax > 0 && (rt == 0 || rt > rtmax) {
                    return false;
                }
            }
            // unwatched
            if let Some(seen) = &seen {
                if let Some(t) = &e.item.tmdb {
                    let mt = if e.item.k == "s" { "tv" } else { "movie" };
                    if seen.contains(&format!("{mt}:{t}")) {
                        return false;
                    }
                }
            }
            // multi-term search across title, overview/plot, genre, platforms, topic labels, country
            if !terms.is_empty() {
                let mut hay = String::with_capacity(256);
                hay.push_str(&e.item.title);
                hay.push(' ');
                if let Some(m) = &e.meta {
                    hay.push_str(&m.ov);
                    hay.push(' ');
                    for g in &m.g {
                        if let Some(gn) = crate::meta::genre_name(e.item.k == "s", *g) {
                            hay.push_str(gn);
                            hay.push(' ');
                        }
                    }
                }
                if !e.item.plot.is_empty() {
                    hay.push_str(&e.item.plot);
                    hay.push(' ');
                }
                if !e.item.genre.is_empty() {
                    hay.push_str(&e.item.genre);
                    hay.push(' ');
                }
                for p in &e.platforms {
                    hay.push_str(p);
                    hay.push(' ');
                }
                for t in &e.topics {
                    hay.push_str(t);
                    hay.push(' ');
                    if let Some(lbl) = topic_label(t) {
                        hay.push_str(lbl);
                        hay.push(' ');
                    }
                }
                for c in &e.countries {
                    hay.push_str(c);
                    hay.push(' ');
                    let al = country_aliases(c);
                    if !al.is_empty() {
                        hay.push_str(al);
                        hay.push(' ');
                    }
                }
                let hay_lo = hay.to_lowercase();
                if !terms.iter().all(|w| hay_lo.contains(w.as_str())) {
                    return false;
                }
            }
            true
        })
        .collect();

    let (topics_facet, platforms_facet, countries_facet, movies_cnt, series_cnt, uhd_cnt) = compute_facets(&matched);

    match sort.as_str() {
        "rating" => matched.sort_by(|a, b| b.bayes().total_cmp(&a.bayes()).then(b.votes().cmp(&a.votes()))),
        "votes" => matched.sort_by(|a, b| b.votes().cmp(&a.votes()).then(b.bayes().total_cmp(&a.bayes()))),
        "newest" => matched.sort_by(|a, b| b.sort_date().cmp(&a.sort_date()).then(b.item.added.cmp(&a.item.added))),
        "oldest" => matched.sort_by(|a, b| {
            let da = a.sort_date();
            let db = b.sort_date();
            (da.is_empty(), da).cmp(&(db.is_empty(), db)).then(b.bayes().total_cmp(&a.bayes()))
        }),
        "runtime_asc" => matched.sort_by(|a, b| {
            let ra = a.runtime();
            let rb = b.runtime();
            (ra == 0, ra).cmp(&(rb == 0, rb)).then(b.bayes().total_cmp(&a.bayes()))
        }),
        "runtime_desc" => matched.sort_by(|a, b| b.runtime().cmp(&a.runtime()).then(b.bayes().total_cmp(&a.bayes()))),
        "added" => matched.sort_by(|a, b| b.item.added.cmp(&a.item.added).then(b.bayes().total_cmp(&a.bayes()))),
        "az" => matched.sort_by(|a, b| a.item.title.to_lowercase().cmp(&b.item.title.to_lowercase())),
        _ => matched.sort_by(|a, b| b.pop().total_cmp(&a.pop()).then(b.bayes().total_cmp(&a.bayes()))),
    }

    let total = matched.len();
    let items: Vec<Value> = matched
        .into_iter()
        .skip(page * per)
        .take(per)
        .map(|e| doc_card_with(app, &e.item, None, &e.topics))
        .collect();

    json!({
        "total": total,
        "page": page,
        "per": per,
        "facets": {
            "topics": topics_facet,
            "platforms": platforms_facet,
            "countries": countries_facet,
            "movies": movies_cnt,
            "series": series_cnt,
            "uhd": uhd_cnt
        },
        "items": items
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_item(k: &'static str, title: &str, genre: &str) -> Item {
        Item {
            k,
            id: 1,
            title: title.into(),
            en: true,
            sport: false,
            genre: genre.into(),
            ..Default::default()
        }
    }

    #[test]
    fn word_boundary_no_false_art_or_war() {
        let (t1, _) = classify_topics("Planet Earth II", "A remarkable look at wildlife across habitats.", "", "", "EN - DOCUMENTARIES", &[99]);
        assert!(t1.contains(&"nature"));
        assert!(!t1.contains(&"music"), "earth must not trigger art/music: {t1:?}");

        let (t2, _) = classify_topics("Part of the Heart", "A smart start to modern life in cities.", "", "", "", &[99]);
        assert!(!t2.contains(&"music"), "part/heart/smart/start must not trigger art/music: {t2:?}");

        let (t3, _) = classify_topics("Award-Winning Warning", "Looking forward toward a warm climate.", "", "", "", &[99]);
        assert!(!t3.contains(&"history"), "award/warning/forward/toward/warm must not trigger war/history: {t3:?}");

        let (t4, _) = classify_topics("Empire of Dreams: The Story of the Star Wars Trilogy", "Behind the scenes filmmaking in Hollywood.", "", "", "", &[99]);
        assert!(t4.contains(&"music"), "film doc should classify as music/arts: {t4:?}");
        assert!(!t4.contains(&"history"), "Star Wars making-of must not trigger history/war: {t4:?}");
    }

    #[test]
    fn no_cross_topic_false_positives() {
        let (t_cult, _) = classify_topics("Wild Wild Country", "A controversial cult builds a utopian city in Oregon.", "", "", "NETFLIX SERIES", &[99]);
        assert!(t_cult.contains(&"crime"));
        assert!(!t_cult.contains(&"nature"), "Wild Wild Country must not match nature: {t_cult:?}");

        let (t_cos, _) = classify_topics("Cosmos: A Spacetime Odyssey", "Astrophysicist Neil deGrasse Tyson explores the universe and space exploration.", "", "", "", &[99]);
        assert_eq!(t_cos[0], "science");
        assert!(!t_cos.contains(&"food"), "Odyssey/space exploration must not match food/travel: {t_cos:?}");
    }

    #[test]
    fn classifies_all_nine_topics() {
        assert_eq!(classify_topics("Making a Murderer", "A wrongful conviction and homicide investigation.", "", "", "", &[99, 80]).0[0], "crime");
        assert_eq!(classify_topics("Blue Planet II", "David Attenborough explores coral reefs and deep sea whales.", "", "", "", &[99]).0[0], "nature");
        assert_eq!(classify_topics("WWII in Colour", "Archival footage of World War battles.", "", "", "", &[99, 36, 10752]).0[0], "history");
        assert_eq!(classify_topics("Apollo 11", "NASA astronauts and the historic moon landing rocket mission.", "", "", "", &[99]).0[0], "science");
        assert_eq!(classify_topics("Free Solo", "Rock climbing El Capitan without ropes.", "", "", "", &[99]).0[0], "sports");
        assert_eq!(classify_topics("The Beatles: Get Back", "Recording studio sessions for an iconic rock band.", "", "", "", &[99, 10402]).0[0], "music");
        assert_eq!(classify_topics("Becoming", "An intimate portrait and memoir of Michelle Obama.", "", "", "", &[99]).0[0], "bio");
        assert_eq!(classify_topics("Inside Job", "The 2008 financial crisis on Wall Street and banking corruption.", "", "", "", &[99]).0[0], "society");
        assert_eq!(classify_topics("Jiro Dreams of Sushi", "A Michelin-starred sushi chef in Tokyo.", "", "", "", &[99]).0[0], "food");
    }

    #[test]
    fn topic_count_between_one_and_three() {
        let (empty_t, explicit) = classify_topics("Untitled", "", "", "", "", &[99]);
        assert_eq!(empty_t, vec!["society"]);
        assert!(!explicit);

        let (many_t, explicit_many) = classify_topics(
            "Murder of the Nazi Space Chef",
            "A serial killer scientist during WWII who played NBA basketball, cooked sushi, and played jazz.",
            "",
            "",
            "",
            &[99, 80, 36, 10402],
        );
        assert!(explicit_many);
        assert!((1..=3).contains(&many_t.len()));
    }

    #[test]
    fn topic_label_lookup() {
        assert_eq!(topic_label("crime"), Some("True Crime"));
        assert_eq!(topic_label("food"), Some("Food & Travel"));
        assert_eq!(topic_label("unknown"), None);
    }

    #[test]
    fn is_doc_classification_rules() {
        let mut it = mk_item("m", "Test", "");
        let doc_meta = Meta { g: vec![99], ..Default::default() };
        let anim_doc_meta = Meta { g: vec![99, 16], ..Default::default() };
        let cartoon_meta = Meta { g: vec![16], ..Default::default() };
        let reality_meta = Meta { g: vec![10764], ..Default::default() };
        let drama_meta = Meta { g: vec![18, 36], ..Default::default() };

        // Genre 99 always counts (including animated documentaries).
        assert!(is_doc_with(&it, "NETFLIX MOVIES", Some(&doc_meta)));
        assert!(is_doc_with(&it, "NETFLIX MOVIES", Some(&anim_doc_meta)));

        // Non-English or sport items are excluded even with genre 99.
        it.en = false;
        assert!(!is_doc_with(&it, "EN - DOCUMENTARIES", Some(&doc_meta)));
        it.en = true;
        it.sport = true;
        assert!(!is_doc_with(&it, "EN - DOCUMENTARIES", Some(&doc_meta)));
        it.sport = false;

        // DOCU categories allow missing meta or non-cartoon meta, reject pure cartoon (16 without 99).
        assert!(is_doc_with(&it, "EN - DOCUMENTARIES", None));
        assert!(is_doc_with(&it, "DOCU-SERIES", Some(&drama_meta)));
        assert!(!is_doc_with(&it, "DOCU-MOVIES", Some(&cartoon_meta)));

        // DISCOVERY+ allows missing meta, requires 99 once meta is present (excludes pure reality shows).
        assert!(is_doc_with(&it, "DISCOVERY+ SERIES", None));
        assert!(is_doc_with(&it, "DISCOVERY+ SERIES", Some(&doc_meta)));
        assert!(!is_doc_with(&it, "DISCOVERY+ SERIES", Some(&reality_meta)));

        // Genre field fallback.
        let it_g = mk_item("s", "Unindexed Doc", "Documentary, History");
        assert!(is_doc_with(&it_g, "EN - SERIES", None));
        assert!(!is_doc_with(&it, "EN - ACTION", Some(&drama_meta)));
    }

    #[test]
    fn doc_bayes_smoothing_80() {
        let acclaimed = Meta { va: 8.3, vc: 200, ..Default::default() };
        let tiny_votes = Meta { va: 10.0, vc: 2, ..Default::default() };
        let average_pop = Meta { va: 7.4, vc: 1000, ..Default::default() };
        assert!(doc_bayes(&acclaimed) > doc_bayes(&tiny_votes));
        assert!(doc_bayes(&acclaimed) > doc_bayes(&average_pop));
    }
}
