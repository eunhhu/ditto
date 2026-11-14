//! Task 046 Phase 0: the history-card selector measured offline against
//! `eval/history-cards/probes-v1.json`, under the rules registered in
//! `docs/agent/tasks/046-history-cards.md` before this code existed.
//!
//! `cargo test --release -p ditto-context --test history_cards_phase0 --
//! --ignored --nocapture` prints the report and writes it to
//! `target/task046/phase0-report.json`. The quick test always runs and only
//! checks that the probe labels are what they claim.

use std::collections::{BTreeSet, HashSet};
use std::time::Instant;

use chrono::{DateTime, Duration, Utc};
use ditto_context::history::{CardRules, HistoryExchange, HistoryIndex, Weighting};
use ditto_context::search_form_set;
use serde::Deserialize;
use serde_json::{Value, json};

const BACKGROUND: usize = 2_000;
const LATENCY_SIZES: [usize; 2] = [1_000, 10_000];
const WINDOW: usize = 16;
const FALSE_FIRE_LIMIT: f64 = 0.10;

#[derive(Debug, Deserialize)]
struct Probes {
    now: DateTime<Utc>,
    targets: Vec<Target>,
    unrelated: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Target {
    id: String,
    exchange: Planted,
    hard_distractors: Vec<Planted>,
    probes: Vec<Probe>,
}

#[derive(Debug, Deserialize)]
struct Planted {
    age_days: i64,
    user: String,
    answer: String,
}

#[derive(Debug, Deserialize)]
struct Probe {
    class: String,
    #[serde(default)]
    kind: Option<String>,
    text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Lexical,
    Paraphrase,
    Related,
    Unrelated,
}

#[derive(Debug, Clone)]
struct Case {
    class: Class,
    text: String,
    target: Option<String>,
    held_out: bool,
}

fn load() -> Probes {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../eval/history-cards/probes-v1.json"
    );
    serde_json::from_slice(&std::fs::read(path).expect("probe file")).expect("probe json")
}

/// Every probe in file order, alternating between the tuning half and the
/// held-out half (the registered split).
fn cases(probes: &Probes) -> Vec<Case> {
    let mut cases = Vec::new();
    for target in &probes.targets {
        for probe in &target.probes {
            let class = match (probe.class.as_str(), probe.kind.as_deref()) {
                ("needs", Some("lexical")) => Class::Lexical,
                ("needs", Some("paraphrase")) => Class::Paraphrase,
                ("related", _) => Class::Related,
                other => panic!("unknown probe class {other:?}"),
            };
            cases.push(Case {
                class,
                text: probe.text.clone(),
                target: Some(format!("target-{}", target.id)),
                held_out: false,
            });
        }
    }
    for text in &probes.unrelated {
        cases.push(Case {
            class: Class::Unrelated,
            text: text.clone(),
            target: None,
            held_out: false,
        });
    }
    for (ordinal, case) in cases.iter_mut().enumerate() {
        case.held_out = ordinal % 2 == 1;
    }
    cases
}

/// A deterministic generator, so every run sees the same background.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[(self.next() as usize) % items.len()]
    }
}

const PERSON_KO: [&str; 10] = [
    "친구 민지",
    "동료 준호",
    "팀장님",
    "언니",
    "사촌",
    "이웃 아저씨",
    "대학 동기",
    "남편",
    "아내",
    "아들",
];
const PLACE_KO: [&str; 10] = [
    "카페",
    "공원",
    "헬스장",
    "도서관",
    "마트",
    "영화관",
    "한강",
    "시장",
    "백화점",
    "회사 근처 식당",
];
const ACTIVITY_KO: [&str; 10] = [
    "산책",
    "운동",
    "독서",
    "수다",
    "쇼핑",
    "요가",
    "자전거",
    "등산",
    "공부",
    "사진 촬영",
];
const FOOD_KO: [&str; 10] = [
    "김치찌개",
    "파스타",
    "샐러드",
    "비빔밥",
    "라멘",
    "떡볶이",
    "초밥",
    "햄버거",
    "쌀국수",
    "된장찌개",
];
const WORK_KO: [&str; 8] = [
    "보고서",
    "발표 자료",
    "코드 리뷰",
    "예산안",
    "기획서",
    "견적서",
    "회의록",
    "계약서",
];
const DAY_KO: [&str; 5] = ["월요일", "화요일", "수요일", "목요일", "금요일"];
const WEATHER_KO: [&str; 5] = ["맑은", "흐린", "쌀쌀한", "따뜻한", "바람 부는"];
const THING_KO: [&str; 8] = [
    "우산",
    "텀블러",
    "운동화",
    "노트",
    "이어폰",
    "머그컵",
    "양말",
    "충전기",
];
const HOBBY_KO: [&str; 7] = [
    "뜨개질",
    "보드게임",
    "베이킹",
    "캠핑",
    "클라이밍",
    "드로잉",
    "기타 연주",
];
const PERSON_EN: [&str; 8] = [
    "Sam",
    "Priya",
    "my manager",
    "my brother",
    "Jordan",
    "Mia",
    "the neighbors",
    "my cousin",
];
const PLACE_EN: [&str; 8] = [
    "cafe",
    "park",
    "gym",
    "library",
    "mall",
    "cinema",
    "farmers market",
    "museum",
];
const ACTIVITY_EN: [&str; 8] = [
    "a walk", "a run", "reading", "yoga", "cycling", "hiking", "painting", "swimming",
];
const FOOD_EN: [&str; 8] = [
    "ramen",
    "tacos",
    "salad",
    "pizza",
    "curry",
    "sushi",
    "a sandwich",
    "pho",
];
const WORK_EN: [&str; 6] = [
    "quarterly report",
    "client deck",
    "code review",
    "budget",
    "roadmap",
    "launch plan",
];
const DAY_EN: [&str; 5] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday"];
const WEATHER_EN: [&str; 5] = ["sunny", "cloudy", "chilly", "warm", "windy"];
const THING_EN: [&str; 7] = [
    "umbrella",
    "water bottle",
    "pair of sneakers",
    "notebook",
    "headphones",
    "mug",
    "charger",
];
const HOBBY_EN: [&str; 7] = [
    "knitting",
    "board games",
    "baking",
    "camping",
    "climbing",
    "drawing",
    "guitar",
];

/// One everyday exchange, Korean or English.
fn background(rng: &mut Lcg) -> (String, String) {
    let korean = rng.next() % 2 == 0;
    let template = rng.next() % 10;
    if korean {
        let (p, l, a, f) = (
            rng.pick(&PERSON_KO),
            rng.pick(&PLACE_KO),
            rng.pick(&ACTIVITY_KO),
            rng.pick(&FOOD_KO),
        );
        let (w, d, we, t, h) = (
            rng.pick(&WORK_KO),
            rng.pick(&DAY_KO),
            rng.pick(&WEATHER_KO),
            rng.pick(&THING_KO),
            rng.pick(&HOBBY_KO),
        );
        match template {
            0 => (
                format!("{p}랑 {l}에서 {a} 했어"),
                format!("{a} 좋았겠네요."),
            ),
            1 => (
                format!("오늘 점심은 {f} 먹었어"),
                format!("{f} 맛있었겠어요."),
            ),
            2 => (
                format!("{w} 마감이 {d}이라 바빠"),
                format!("{d}까지 힘내세요."),
            ),
            3 => (
                format!("{we} 날씨라 {a} 하기 좋네"),
                "좋은 하루 보내세요.".into(),
            ),
            4 => (
                format!("{p} 생일이 다음 주야"),
                "선물 준비 도와드릴까요?".into(),
            ),
            5 => (format!("{l} 가는 길에 {t} 샀어"), "잘 사셨네요.".into()),
            6 => (format!("요즘 {h}에 빠졌어"), format!("{h} 재미있죠.")),
            7 => (
                format!("회사에서 {w} 때문에 회의만 세 번 했어"),
                "피곤하셨겠어요.".into(),
            ),
            8 => (
                format!("{f} 레시피 알려줘"),
                format!("{f}는 재료를 먼저 손질하세요."),
            ),
            _ => (
                format!("주말에 {p}랑 {l} 갈까 해"),
                "즐거운 주말 되세요.".into(),
            ),
        }
    } else {
        let (p, l, a, f) = (
            rng.pick(&PERSON_EN),
            rng.pick(&PLACE_EN),
            rng.pick(&ACTIVITY_EN),
            rng.pick(&FOOD_EN),
        );
        let (w, d, we, t, h) = (
            rng.pick(&WORK_EN),
            rng.pick(&DAY_EN),
            rng.pick(&WEATHER_EN),
            rng.pick(&THING_EN),
            rng.pick(&HOBBY_EN),
        );
        match template {
            0 => (format!("Had {f} for lunch with {p}"), "Sounds nice.".into()),
            1 => (
                format!("Went to the {l} after work"),
                "Good way to unwind.".into(),
            ),
            2 => (
                format!("The {w} deadline moved to {d}"),
                "Noted the new deadline.".into(),
            ),
            3 => (
                format!("It's {we} today, perfect for {a}"),
                "Enjoy it.".into(),
            ),
            4 => (
                format!("{p}'s birthday is next week"),
                "Want help picking a gift?".into(),
            ),
            5 => (
                format!("Bought a new {t} at the mall"),
                "Nice purchase.".into(),
            ),
            6 => (
                format!("I've been into {h} lately"),
                "That's a fun hobby.".into(),
            ),
            7 => (
                format!("Three meetings about the {w} today"),
                "That sounds tiring.".into(),
            ),
            8 => (
                format!("Can you give me a {f} recipe?"),
                "Start by prepping the ingredients.".into(),
            ),
            _ => (
                format!("Thinking of visiting the {l} with {p} this weekend"),
                "Have a great weekend.".into(),
            ),
        }
    }
}

/// The planted targets and distractors plus `size` background exchanges,
/// oldest first, and the turn ids of the most recent `WINDOW` (never carded).
fn corpus(probes: &Probes, size: usize) -> (Vec<HistoryExchange>, HashSet<String>) {
    let mut rng = Lcg(0x5eed_046);
    let mut exchanges = Vec::with_capacity(size + 64);
    for target in &probes.targets {
        exchanges.push(HistoryExchange {
            turn_id: format!("target-{}", target.id),
            at: probes.now - Duration::days(target.exchange.age_days),
            user: target.exchange.user.clone(),
            answer: target.exchange.answer.clone(),
        });
        for (index, planted) in target.hard_distractors.iter().enumerate() {
            exchanges.push(HistoryExchange {
                turn_id: format!("distractor-{}-{index}", target.id),
                at: probes.now - Duration::days(planted.age_days),
                user: planted.user.clone(),
                answer: planted.answer.clone(),
            });
        }
    }
    for index in 0..size {
        let minutes = (rng.next() % (90 * 24 * 60)) as i64;
        let (user, answer) = background(&mut rng);
        exchanges.push(HistoryExchange {
            turn_id: format!("background-{index:05}"),
            at: probes.now - Duration::minutes(minutes),
            user,
            answer,
        });
    }
    exchanges.sort_by(|left, right| {
        left.at
            .cmp(&right.at)
            .then(left.turn_id.cmp(&right.turn_id))
    });
    let window = exchanges
        .iter()
        .rev()
        .take(WINDOW)
        .map(|exchange| exchange.turn_id.clone())
        .collect();
    (exchanges, window)
}

fn index_of(exchanges: &[HistoryExchange]) -> HistoryIndex {
    let mut index = HistoryIndex::new();
    for exchange in exchanges {
        index.push(exchange.clone());
    }
    index
}

#[derive(Debug, Default, Clone)]
struct Metrics {
    lexical: (usize, usize, usize),
    paraphrase: (usize, usize),
    reciprocal: f64,
    related: (usize, usize),
    unrelated: (usize, usize),
    tokens: Vec<usize>,
}

impl Metrics {
    fn lexical_recall5(&self) -> f64 {
        ratio(self.lexical.1, self.lexical.0)
    }
    fn lexical_recall3(&self) -> f64 {
        ratio(self.lexical.2, self.lexical.0)
    }
    fn all_recall5(&self) -> f64 {
        ratio(
            self.lexical.1 + self.paraphrase.1,
            self.lexical.0 + self.paraphrase.0,
        )
    }
    fn false_fire(&self) -> f64 {
        ratio(self.unrelated.1, self.unrelated.0)
    }
    fn related_fire(&self) -> f64 {
        ratio(self.related.1, self.related.0)
    }
    fn mean_tokens(&self) -> f64 {
        if self.tokens.is_empty() {
            0.0
        } else {
            self.tokens.iter().sum::<usize>() as f64 / self.tokens.len() as f64
        }
    }
    fn json(&self) -> Value {
        json!({
            "lexical_recall_at_5": self.lexical_recall5(),
            "lexical_recall_at_3": self.lexical_recall3(),
            "lexical_probes": self.lexical.0,
            "lexical_mrr": if self.lexical.0 == 0 { 0.0 } else { self.reciprocal / self.lexical.0 as f64 },
            "paraphrase_recall_at_5": ratio(self.paraphrase.1, self.paraphrase.0),
            "paraphrase_probes": self.paraphrase.0,
            "all_needs_recall_at_5": self.all_recall5(),
            "related_fire_rate": self.related_fire(),
            "related_probes": self.related.0,
            "unrelated_false_fire_rate": self.false_fire(),
            "unrelated_probes": self.unrelated.0,
            "mean_tokens_per_firing": self.mean_tokens(),
        })
    }
}

fn ratio(numerator: usize, denominator: usize) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

/// Characters over four, the token estimate the context compiler uses.
fn tokens(cards: &[ditto_context::history::Card]) -> usize {
    cards
        .iter()
        .enumerate()
        .map(|(position, card)| card.render(&format!("h{}", position + 1)).len().div_ceil(4))
        .sum()
}

fn evaluate(
    index: &HistoryIndex,
    window: &HashSet<String>,
    cases: &[Case],
    held_out: bool,
    rules: &CardRules,
    now: DateTime<Utc>,
) -> Metrics {
    let mut metrics = Metrics::default();
    for case in cases.iter().filter(|case| case.held_out == held_out) {
        let cards = index.cards(&case.text, now, window, rules);
        let position = case
            .target
            .as_ref()
            .and_then(|target| cards.iter().position(|card| &card.turn_id == target));
        if !cards.is_empty() {
            metrics.tokens.push(tokens(&cards));
        }
        match case.class {
            Class::Lexical => {
                metrics.lexical.0 += 1;
                if let Some(rank) = position {
                    metrics.lexical.1 += usize::from(rank < 5);
                    metrics.lexical.2 += usize::from(rank < 3);
                    metrics.reciprocal += 1.0 / (rank + 1) as f64;
                }
            }
            Class::Paraphrase => {
                metrics.paraphrase.0 += 1;
                metrics.paraphrase.1 += usize::from(position.is_some_and(|rank| rank < 5));
            }
            Class::Related => {
                metrics.related.0 += 1;
                metrics.related.1 += usize::from(!cards.is_empty());
            }
            Class::Unrelated => {
                metrics.unrelated.0 += 1;
                metrics.unrelated.1 += usize::from(!cards.is_empty());
            }
        }
    }
    metrics
}

fn grid() -> Vec<CardRules> {
    let mut rules = Vec::new();
    for weighting in [Weighting::Idf, Weighting::Inverse, Weighting::Flat] {
        let scores: &[f64] = match weighting {
            Weighting::Idf => &[0.0, 2.0, 4.0, 6.0, 8.0, 10.0],
            Weighting::Inverse => &[0.0, 0.05, 0.1, 0.25, 0.5, 1.0],
            Weighting::Flat => &[0.0, 1.0, 2.0, 3.0],
        };
        for recency in [true, false] {
            for answer_words in [true, false] {
                for min_shared in [1, 2, 3] {
                    for rare_df in [0, 1, 2, 3, 5, 10] {
                        for min_score in scores {
                            rules.push(CardRules {
                                max_cards: 5,
                                extract_chars: 80,
                                weighting,
                                recency,
                                answer_words,
                                min_shared,
                                rare_df,
                                min_score: *min_score,
                            });
                        }
                    }
                }
            }
        }
    }
    rules
}

fn rules_json(rules: &CardRules) -> Value {
    json!({
        "max_cards": rules.max_cards,
        "extract_chars": rules.extract_chars,
        "weighting": format!("{:?}", rules.weighting),
        "recency": rules.recency,
        "answer_words": rules.answer_words,
        "min_shared": rules.min_shared,
        "rare_df": rules.rare_df,
        "min_score": rules.min_score,
    })
}

/// The registered tuning rule: highest lexical Recall@5 with at most 10%
/// false fire; then the lower related fire rate; then fewer tokens.
fn tune(
    index: &HistoryIndex,
    window: &HashSet<String>,
    cases: &[Case],
    now: DateTime<Utc>,
) -> (CardRules, Metrics) {
    let mut best: Option<(CardRules, Metrics)> = None;
    for rules in grid() {
        let metrics = evaluate(index, window, cases, false, &rules, now);
        if metrics.false_fire() > FALSE_FIRE_LIMIT {
            continue;
        }
        let better = match &best {
            None => true,
            Some((_, current)) => {
                let key = |m: &Metrics| (m.lexical_recall5(), -m.related_fire(), -m.mean_tokens());
                let (a, b) = (key(&metrics), key(current));
                a.0 > b.0 || (a.0 == b.0 && (a.1 > b.1 || (a.1 == b.1 && a.2 > b.2)))
            }
        };
        if better {
            best = Some((rules, metrics));
        }
    }
    best.expect("some rules meet the false-fire limit")
}

fn percentile(sorted: &[f64], fraction: f64) -> f64 {
    let rank = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted[rank]
}

fn rss_kib() -> Option<u64> {
    std::fs::read_to_string("/proc/self/status")
        .ok()?
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next()?.parse().ok())
}

fn latency(probes: &Probes, cases: &[Case], rules: &CardRules) -> Value {
    let mut rows = Vec::new();
    for size in LATENCY_SIZES {
        let (exchanges, window) = corpus(probes, size);
        let before = rss_kib();
        let started = Instant::now();
        let index = index_of(&exchanges);
        let build_ms = started.elapsed().as_secs_f64() * 1_000.0;
        let after = rss_kib();
        let mut samples = Vec::new();
        for _ in 0..20 {
            for case in cases {
                let started = Instant::now();
                let cards = index.cards(&case.text, probes.now, &window, rules);
                samples.push(started.elapsed().as_secs_f64() * 1_000.0);
                std::hint::black_box(cards);
            }
        }
        samples.sort_by(f64::total_cmp);
        rows.push(json!({
            "exchanges": index.len(),
            "build_ms": build_ms,
            "rss_growth_kib": match (before, after) { (Some(b), Some(a)) => json!(a.saturating_sub(b)), _ => Value::Null },
            "select_p50_ms": percentile(&samples, 0.50),
            "select_p95_ms": percentile(&samples, 0.95),
            "select_max_ms": samples.last().copied(),
            "samples": samples.len(),
        }));
        drop(index);
    }
    json!(rows)
}

/// The same probes through SQLite FTS5, unicode61 and trigram tokenizers,
/// top five by bm25, firing on any match.
fn fts5(probes: &Probes, cases: &[Case]) -> Value {
    let (exchanges, window) = corpus(probes, BACKGROUND);
    let mut out = serde_json::Map::new();
    for tokenizer in ["unicode61", "trigram"] {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        db.execute_batch(&format!(
            "CREATE VIRTUAL TABLE h USING fts5(turn_id UNINDEXED, body, tokenize='{tokenizer}');"
        ))
        .unwrap();
        {
            let mut insert = db
                .prepare("INSERT INTO h (turn_id, body) VALUES (?1, ?2)")
                .unwrap();
            for exchange in &exchanges {
                insert
                    .execute((
                        &exchange.turn_id,
                        format!("{} {}", exchange.user, exchange.answer),
                    ))
                    .unwrap();
            }
        }
        let minimum = if tokenizer == "trigram" { 3 } else { 1 };
        let mut metrics = Metrics::default();
        let mut samples = Vec::new();
        for case in cases.iter().filter(|case| case.held_out) {
            let terms = case
                .text
                .split(|character: char| !character.is_alphanumeric())
                .filter(|term| term.chars().count() >= minimum)
                .map(|term| format!("\"{}\"", term.replace('"', "\"\"")))
                .collect::<Vec<_>>();
            let started = Instant::now();
            let found = if terms.is_empty() {
                Vec::new()
            } else {
                let mut query = db
                    .prepare("SELECT turn_id FROM h WHERE h MATCH ?1 ORDER BY bm25(h) LIMIT 21")
                    .unwrap();
                query
                    .query_map([terms.join(" OR ")], |row| row.get::<_, String>(0))
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|id| !window.contains(id))
                    .take(5)
                    .collect::<Vec<_>>()
            };
            samples.push(started.elapsed().as_secs_f64() * 1_000.0);
            let hit = case
                .target
                .as_ref()
                .is_some_and(|target| found.iter().any(|id| id == target));
            match case.class {
                Class::Lexical => {
                    metrics.lexical.0 += 1;
                    metrics.lexical.1 += usize::from(hit);
                }
                Class::Paraphrase => {
                    metrics.paraphrase.0 += 1;
                    metrics.paraphrase.1 += usize::from(hit);
                }
                Class::Related => {
                    metrics.related.0 += 1;
                    metrics.related.1 += usize::from(!found.is_empty());
                }
                Class::Unrelated => {
                    metrics.unrelated.0 += 1;
                    metrics.unrelated.1 += usize::from(!found.is_empty());
                }
            }
        }
        samples.sort_by(f64::total_cmp);
        out.insert(
            tokenizer.into(),
            json!({
                "held_out_lexical_recall_at_5": metrics.lexical_recall5(),
                "held_out_all_needs_recall_at_5": metrics.all_recall5(),
                "held_out_related_fire_rate": metrics.related_fire(),
                "held_out_unrelated_fire_rate": metrics.false_fire(),
                "select_p95_ms_at_2000": percentile(&samples, 0.95),
            }),
        );
    }
    Value::Object(out)
}

#[test]
fn probe_labels_match_their_words() {
    let probes = load();
    for target in &probes.targets {
        let target_forms = search_form_set(&format!(
            "{} {}",
            target.exchange.user, target.exchange.answer
        ));
        for probe in &target.probes {
            let shared = search_form_set(&probe.text)
                .intersection(&target_forms)
                .cloned()
                .collect::<BTreeSet<_>>();
            match probe.kind.as_deref() {
                Some("lexical") => assert!(
                    !shared.is_empty(),
                    "{}: lexical probe shares nothing: {}",
                    target.id,
                    probe.text
                ),
                Some("paraphrase") => assert!(
                    shared.is_empty(),
                    "{}: paraphrase probe shares {shared:?}: {}",
                    target.id,
                    probe.text
                ),
                _ => {}
            }
        }
    }
    let cases = cases(&probes);
    assert_eq!(cases.len(), 104);
    assert_eq!(cases.iter().filter(|case| case.held_out).count(), 52);
}

#[test]
#[ignore = "Phase 0 report: run in release with --ignored --nocapture"]
fn phase0_report() {
    let probes = load();
    let cases = cases(&probes);
    let (exchanges, window) = corpus(&probes, BACKGROUND);
    let index = index_of(&exchanges);

    let (frozen, tuning) = tune(&index, &window, &cases, probes.now);
    let held_out = evaluate(&index, &window, &cases, true, &frozen, probes.now);
    let latency = latency(&probes, &cases, &frozen);
    let p95_at_10k = latency[1]["select_p95_ms"].as_f64().unwrap();
    let gates = json!({
        "lexical_recall_at_5_at_least_0_90": held_out.lexical_recall5() >= 0.90,
        "unrelated_false_fire_at_most_0_10": held_out.false_fire() <= FALSE_FIRE_LIMIT,
        "select_p95_at_10000_at_most_5ms": p95_at_10k <= 5.0,
    });
    let misses = cases
        .iter()
        .filter(|case| case.held_out && matches!(case.class, Class::Lexical))
        .filter(|case| {
            let cards = index.cards(&case.text, probes.now, &window, &frozen);
            !cards
                .iter()
                .take(5)
                .any(|card| Some(&card.turn_id) == case.target.as_ref())
        })
        .map(|case| json!({"target": case.target, "probe": case.text}))
        .collect::<Vec<_>>();
    let fired_unrelated = cases
        .iter()
        .filter(|case| case.held_out && case.class == Class::Unrelated)
        .filter_map(|case| {
            let cards = index.cards(&case.text, probes.now, &window, &frozen);
            (!cards.is_empty()).then(|| {
                json!({
                    "probe": case.text,
                    "cards": cards
                        .iter()
                        .map(|card| json!({
                            "turn_id": card.turn_id,
                            "matched": card.matched,
                            "score": card.score,
                        }))
                        .collect::<Vec<_>>(),
                })
            })
        })
        .collect::<Vec<_>>();
    let report = json!({
        "probes": "eval/history-cards/probes-v1.json",
        "background_exchanges": BACKGROUND,
        "corpus_exchanges": index.len(),
        "window": WINDOW,
        "grid_size": grid().len(),
        "frozen_rules": rules_json(&frozen),
        "tuning_half": tuning.json(),
        "held_out_half": held_out.json(),
        "held_out_lexical_misses": misses,
        "held_out_unrelated_fired": fired_unrelated,
        "gates": gates,
        "latency": latency,
        "fts5": fts5(&probes, &cases),
        "build": if cfg!(debug_assertions) { "debug" } else { "release" },
    });
    let text = serde_json::to_string_pretty(&report).unwrap();
    println!("{text}");
    let out = concat!(env!("CARGO_MANIFEST_DIR"), "/../../target/task046");
    std::fs::create_dir_all(out).unwrap();
    std::fs::write(format!("{out}/phase0-report.json"), text + "\n").unwrap();
}
