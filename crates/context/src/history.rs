//! History cards (Task 046): earlier exchanges related to a message, chosen
//! from the memory search's words (ADR 0029) with no model call.
//!
//! Phase 0 measures this selector offline against
//! `eval/history-cards/probes-v1.json`; no turn uses it yet. It is a pure
//! function of the exchanges it holds, the message, the time and the rules, so
//! a turn that uses it can be replayed from the journal.

use std::collections::{BTreeMap, HashMap, HashSet};

use chrono::{DateTime, Utc};

use crate::search_vocabulary;

/// One finished exchange: what the user said and what Ditto answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryExchange {
    pub turn_id: String,
    pub at: DateTime<Utc>,
    pub user: String,
    pub answer: String,
}

/// How much a matched word counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weighting {
    /// Every matched word counts one.
    Flat,
    /// `1 / n`, where `n` exchanges hold the word.
    Inverse,
    /// `ln(1 + N / n)` over `N` exchanges.
    Idf,
}

/// The selector's knobs. Phase 0 tunes them on half its probes and freezes
/// them before reading the other half.
#[derive(Debug, Clone, PartialEq)]
pub struct CardRules {
    pub max_cards: usize,
    /// Characters of the user's text and of the answer each card shows.
    pub extract_chars: usize,
    pub weighting: Weighting,
    /// Older exchanges count less, by age bucket rather than a continuous
    /// decay, so every platform orders them the same way.
    pub recency: bool,
    /// Match Ditto's answer too, not only what the user said.
    pub answer_words: bool,
    /// Matched words a card needs, unless one of them is rare.
    pub min_shared: usize,
    /// A card with fewer matched words still fires when one of them is held
    /// by at most this many exchanges.
    pub rare_df: usize,
    pub min_score: f64,
}

/// One card: an earlier exchange, why it matched, and short extracts.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub turn_id: String,
    pub at: DateTime<Utc>,
    pub score: f64,
    pub matched: Vec<String>,
    pub user: String,
    pub answer: String,
}

impl Card {
    /// The line a turn would show: label, date, speaker and extracts.
    pub fn render(&self, label: &str) -> String {
        format!(
            "- {label} ({}) you: \"{}\" / Ditto: \"{}\"",
            self.at.format("%Y-%m-%d"),
            self.user,
            self.answer
        )
    }
}

#[derive(Debug)]
struct Entry {
    exchange: HistoryExchange,
}

/// An inverted index over exchanges, by search form.
#[derive(Debug, Default)]
pub struct HistoryIndex {
    entries: Vec<Entry>,
    user_postings: HashMap<String, Vec<u32>>,
    any_postings: HashMap<String, Vec<u32>>,
}

impl HistoryIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn push(&mut self, exchange: HistoryExchange) {
        let id = u32::try_from(self.entries.len()).expect("fewer than 2^32 exchanges");
        let user_forms = forms(&exchange.user);
        let any_forms = user_forms
            .iter()
            .cloned()
            .chain(forms(&exchange.answer))
            .collect::<HashSet<_>>();
        for form in &user_forms {
            self.user_postings.entry(form.clone()).or_default().push(id);
        }
        for form in any_forms {
            self.any_postings.entry(form).or_default().push(id);
        }
        self.entries.push(Entry { exchange });
    }

    /// The cards for `message`: exchanges not in `exclude` that pass the
    /// rules' gate, best first, then newer, then by turn id.
    pub fn cards(
        &self,
        message: &str,
        now: DateTime<Utc>,
        exclude: &HashSet<String>,
        rules: &CardRules,
    ) -> Vec<Card> {
        let postings = if rules.answer_words {
            &self.any_postings
        } else {
            &self.user_postings
        };
        let total = self.entries.len().max(1) as f64;
        // Per exchange: score summed in word order, and the matched words.
        let mut hits: BTreeMap<u32, (f64, Vec<String>, usize)> = BTreeMap::new();
        for (word, word_forms) in search_vocabulary(message) {
            let mut docs = word_forms
                .iter()
                .filter_map(|form| postings.get(form))
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            docs.sort_unstable();
            docs.dedup();
            if docs.is_empty() {
                continue;
            }
            let df = docs.len();
            let weight = match rules.weighting {
                Weighting::Flat => 1.0,
                Weighting::Inverse => 1.0 / df as f64,
                Weighting::Idf => (1.0 + total / df as f64).ln(),
            };
            for doc in docs {
                let hit = hits.entry(doc).or_insert((0.0, Vec::new(), usize::MAX));
                hit.0 += weight;
                hit.1.push(word.clone());
                hit.2 = hit.2.min(df);
            }
        }
        let mut cards = hits
            .into_iter()
            .filter_map(|(doc, (score, matched, rarest))| {
                let entry = &self.entries[doc as usize];
                if exclude.contains(&entry.exchange.turn_id) {
                    return None;
                }
                let enough = matched.len() >= rules.min_shared || rarest <= rules.rare_df;
                let score = score * age_factor(entry.exchange.at, now, rules.recency);
                (enough && score >= rules.min_score).then(|| {
                    let user = extract(&entry.exchange.user, &matched, rules.extract_chars);
                    let answer = extract(&entry.exchange.answer, &matched, rules.extract_chars);
                    Card {
                        turn_id: entry.exchange.turn_id.clone(),
                        at: entry.exchange.at,
                        score,
                        matched,
                        user,
                        answer,
                    }
                })
            })
            .collect::<Vec<_>>();
        cards.sort_by(|left, right| {
            right
                .score
                .total_cmp(&left.score)
                .then_with(|| right.at.cmp(&left.at))
                .then_with(|| left.turn_id.cmp(&right.turn_id))
        });
        cards.truncate(rules.max_cards);
        cards
    }
}

fn forms(text: &str) -> HashSet<String> {
    search_vocabulary(text)
        .into_iter()
        .flat_map(|(_, forms)| forms)
        .collect()
}

/// Recent exchanges count fully, older ones less, in four steps.
fn age_factor(at: DateTime<Utc>, now: DateTime<Utc>, recency: bool) -> f64 {
    if !recency {
        return 1.0;
    }
    match (now - at).num_days() {
        ..=7 => 1.0,
        8..=30 => 0.85,
        31..=90 => 0.7,
        _ => 0.55,
    }
}

/// At most `limit` characters of `text`, around the first matched word.
fn extract(text: &str, matched: &[String], limit: usize) -> String {
    let characters = text.chars().collect::<Vec<_>>();
    if characters.len() <= limit {
        return text.to_owned();
    }
    let lower = text.to_lowercase();
    let first = matched
        .iter()
        .filter_map(|word| lower.find(word.as_str()))
        .min()
        .map(|byte| lower[..byte].chars().count())
        .unwrap_or(0);
    let start = first
        .saturating_sub(limit / 3)
        .min(characters.len() - limit);
    let mut shown = characters[start..start + limit].iter().collect::<String>();
    if start > 0 {
        shown.insert(0, '…');
    }
    if start + limit < characters.len() {
        shown.push('…');
    }
    shown
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn rules() -> CardRules {
        CardRules {
            max_cards: 5,
            extract_chars: 80,
            weighting: Weighting::Idf,
            recency: true,
            answer_words: true,
            min_shared: 2,
            rare_df: 2,
            min_score: 0.0,
        }
    }

    fn exchange(id: &str, days: i64, user: &str, answer: &str) -> HistoryExchange {
        HistoryExchange {
            turn_id: id.into(),
            at: Utc::now() - Duration::days(days),
            user: user.into(),
            answer: answer.into(),
        }
    }

    #[test]
    fn a_rare_word_finds_its_exchange_through_a_particle_and_small_talk_finds_nothing() {
        let mut index = HistoryIndex::new();
        index.push(exchange(
            "gukbap",
            12,
            "회사 앞 할매순대국에서 점심 먹었는데 깍두기가 진짜 맛있더라",
            "기억해 둘게요.",
        ));
        index.push(exchange(
            "lunch-1",
            3,
            "오늘 점심은 파스타 먹었어",
            "맛있었겠어요.",
        ));
        index.push(exchange(
            "lunch-2",
            4,
            "오늘 점심은 샐러드 먹었어",
            "맛있었겠어요.",
        ));
        let none = HashSet::new();
        let cards = index.cards(
            "깍두기 맛있다던 국밥집 어디였지?",
            Utc::now(),
            &none,
            &rules(),
        );
        assert_eq!(cards[0].turn_id, "gukbap");
        assert!(
            index
                .cards("ㅇㅋ 고마워", Utc::now(), &none, &rules())
                .is_empty()
        );
        // An exchange in the window is never carded.
        let window = HashSet::from(["gukbap".to_owned()]);
        assert!(
            index
                .cards("깍두기 맛있다던 곳", Utc::now(), &window, &rules())
                .iter()
                .all(|card| card.turn_id != "gukbap")
        );
    }

    #[test]
    fn the_order_is_the_same_on_every_run() {
        let mut index = HistoryIndex::new();
        for day in 0..30 {
            index.push(exchange(
                &format!("t{day:02}"),
                day,
                "reading group venue notes",
                "noted",
            ));
        }
        let first = index.cards("reading group venue", Utc::now(), &HashSet::new(), &rules());
        for _ in 0..5 {
            assert_eq!(
                index.cards("reading group venue", Utc::now(), &HashSet::new(), &rules()),
                first
            );
        }
    }

    #[test]
    fn an_extract_keeps_the_matched_word_and_its_bound() {
        let text = "a ".repeat(100) + "Okafor will fix the heater" + &" b".repeat(100);
        let shown = extract(&text, &["okafor".to_owned()], 40);
        assert!(shown.contains("Okafor"), "{shown}");
        assert!(shown.chars().count() <= 42);
    }
}
