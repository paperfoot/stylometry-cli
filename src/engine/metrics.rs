//! Named, plain-language measurements of a passage's texture: rhythm,
//! paragraphing, dialogue, word difficulty, punctuation, narration habits and
//! sentence constructions. Each is one number an agent can aim at.
//!
//! A voice is judged by ranges at a length: every span of the author's text
//! as long as the draft is measured, and the draft must sit inside the
//! author's 15th–85th percentile band. Rates over a 400-word scene swing far
//! more than over a chapter, so ranges are always built at the draft's own
//! length (`ranges`), never from whole-book averages.

use std::collections::HashMap;

use serde::Serialize;

use crate::engine::lexicon;
use crate::engine::profile::Profile;
use crate::engine::style::{self, StyleCounts};
use crate::engine::text;

/// What a metric counts, which decides its rounding and its minimum
/// tolerance (one event in the window for rates).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Occurrences per 1,000 words.
    Per1k,
    /// Sentences per 1,000 sentences.
    Per1kSentences,
    /// A fraction in [0, 1].
    Share,
    /// Words (lengths and their spread).
    Words,
    /// Any other score (grade, diversity, correlation, ratio).
    Score,
}

/// One metric's identity and the plain advice for moving it.
#[derive(Debug, Clone)]
pub struct Def {
    pub key: String,
    pub label: String,
    pub unit: Unit,
    pub raise: &'static str,
    pub lower: &'static str,
}

/// key, label, unit, advice to raise, advice to lower.
type Core = (&'static str, &'static str, Unit, &'static str, &'static str);

const CORE: &[Core] = &[
    ("sentence_words_mean", "mean sentence length (words)", Unit::Words,
     "join fragments to their neighbours and let a few sentences run on with clauses",
     "split long sentences; end more of them early"),
    ("sentence_words_median", "median sentence length (words)", Unit::Words,
     "make the typical sentence longer: fewer fragments, more full clauses",
     "make the typical sentence shorter and plainer"),
    ("sentence_words_p90", "90th-percentile sentence length (words)", Unit::Words,
     "let the longest sentences run longer",
     "cut the longest sentences down"),
    ("sentence_words_sd", "sentence length spread (sd, words)", Unit::Words,
     "vary sentence length more: mix very short and long",
     "keep sentence lengths closer to each other"),
    ("sentence_words_cv", "sentence length variation (cv)", Unit::Score,
     "vary sentence length more relative to the average",
     "even out sentence lengths"),
    ("short_sentence_share", "sentences of 5 words or fewer", Unit::Share,
     "use more short punches and fragments",
     "merge fragments into the sentences around them"),
    ("long_sentence_share", "sentences of 25 words or more", Unit::Share,
     "let more sentences run long",
     "break up the longest sentences"),
    ("sentence_length_autocorr", "sentence-length autocorrelation (lag 1)", Unit::Score,
     "let sentence lengths come in runs (long after long, short after short)",
     "alternate sentence lengths more"),
    ("paragraph_words_mean", "mean paragraph length (words)", Unit::Words,
     "write fuller paragraphs; fold one-liners into the paragraph before",
     "break paragraphs up more often"),
    ("paragraph_words_cv", "paragraph length variation (cv)", Unit::Score,
     "mix very short and long paragraphs more",
     "keep paragraph lengths more even"),
    ("one_sentence_paragraph_share", "one-sentence paragraphs", Unit::Share,
     "give more lines a paragraph of their own",
     "fold one-line paragraphs into their neighbours"),
    ("dialogue_paragraph_share", "paragraphs with dialogue", Unit::Share,
     "put more of the scene into speech",
     "move more of the scene out of dialogue into narration"),
    ("dialogue_word_share", "words inside dialogue", Unit::Share,
     "let characters talk more",
     "trim the dialogue; narrate more"),
    ("word_letters_mean", "mean word length (letters)", Unit::Score,
     "use a few more longer, specific words",
     "prefer short plain words"),
    ("syllables_per_word", "syllables per word", Unit::Score,
     "allow more multi-syllable words",
     "prefer one-syllable words"),
    ("fk_grade", "Flesch-Kincaid grade", Unit::Score,
     "use longer sentences and words in places",
     "use shorter sentences and plainer words"),
    ("mattr_500", "vocabulary variety (MATTR, 500-word window)", Unit::Score,
     "vary the vocabulary more; repeat words less",
     "repeat the core words more; stop reaching for variants"),
    ("mtld", "vocabulary variety (MTLD)", Unit::Score,
     "vary the vocabulary more",
     "lean on a smaller, repeated vocabulary"),
    ("long_word_share", "words of 7+ letters", Unit::Share,
     "allow a few more long or abstract words",
     "swap long words for short plain ones"),
    ("contractions_per_1k", "contractions per 1k words", Unit::Per1k,
     "contract more (don't, it's, I'm)",
     "spell out more contractions"),
    ("contraction_share", "contracted share of contractable forms", Unit::Share,
     "contract more of the forms that can be contracted",
     "spell out more of them (do not, it is)"),
    ("ly_adverbs_per_1k", "-ly adverbs per 1k words", Unit::Per1k,
     "allow plain -ly adverbs (really, slowly, finally)",
     "cut -ly adverbs"),
    ("questions_per_1k", "question marks per 1k words", Unit::Per1k,
     "ask more questions, in speech and in the narrator's head",
     "turn some questions into statements"),
    ("narration_questions_per_1k", "questions in narration per 1k words", Unit::Per1k,
     "let the narrator ask herself questions outside dialogue",
     "cut rhetorical questions in narration"),
    ("exclamations_per_1k", "exclamation marks per 1k words", Unit::Per1k,
     "allow an exclamation now and then",
     "cut exclamation marks"),
    ("em_dashes_per_1k", "em dashes per 1k words", Unit::Per1k,
     "use dashes for interruptions and asides",
     "replace dashes with commas or full stops"),
    ("ellipses_per_1k", "ellipses per 1k words", Unit::Per1k,
     "let speech trail off now and then (...)",
     "cut ellipses"),
    ("semicolons_per_1k", "semicolons per 1k words", Unit::Per1k,
     "join a few related clauses with semicolons",
     "replace semicolons with full stops"),
    ("colons_per_1k", "colons per 1k words", Unit::Per1k,
     "use a colon to set up a payoff now and then",
     "cut colons"),
    ("parentheses_per_1k", "parentheses per 1k words", Unit::Per1k,
     "allow parenthetical asides",
     "cut parenthetical asides"),
    ("commas_per_1k", "commas per 1k words", Unit::Per1k,
     "build more sentences from clauses and lists",
     "write fewer clause-heavy sentences"),
    ("first_person_per_1k", "first-person pronouns per 1k words", Unit::Per1k,
     "keep the narrator at the centre: more I, me, my",
     "pull the narrator back a little"),
    ("present_tense_share", "present-tense share of narration verbs", Unit::Share,
     "narrate in the present tense",
     "narrate in the past tense"),
    ("similes_per_1k", "similes per 1k words (like a, as if)", Unit::Per1k,
     "allow an occasional simile",
     "cut similes; say the thing plainly"),
    ("which_clauses_per_1k", "', which' clauses per 1k words", Unit::Per1k,
     "allow a few ', which' clauses",
     "split ', which' clauses into their own sentences"),
    ("the_way_per_1k", "'the way he/she...' per 1k words", Unit::Per1k,
     "allow 'the way she...' descriptions",
     "cut 'the way he...' constructions"),
    ("and_chains_per_1k_sentences", "sentences with 3+ 'and' per 1k sentences", Unit::Per1kSentences,
     "let a few sentences chain with 'and'",
     "break and-chained sentences; build long ones from but/because/when"),
    ("trailing_ing_per_1k", "trailing ', -ing' clauses per 1k words", Unit::Per1k,
     "allow a trailing participle now and then",
     "end sentences on the main verb, not on ', doing X'"),
    ("not_but_per_1k", "'not X, but Y' contrasts per 1k words", Unit::Per1k,
     "allow the odd 'not this but that' contrast",
     "state what it is; drop the 'not X, but Y' frame"),
    ("echo_phrases_per_1k", "repeated three-word phrases per 1k words", Unit::Per1k,
     "repeat key phrases more",
     "vary repeated phrases (same words, same order)"),
    ("colour_words_per_1k", "colour words per 1k words", Unit::Per1k,
     "add a little colour to the setting",
     "cut colour description; sketch the setting"),
    ("emotion_words_per_1k", "named emotions per 1k words", Unit::Per1k,
     "let the narrator name her feelings",
     "show feelings instead of naming them"),
    ("opener_i_share", "narration sentences starting 'I'", Unit::Share,
     "start more narration sentences with I",
     "vary sentence openings away from I"),
    ("opener_the_share", "narration sentences starting 'The'", Unit::Share,
     "start more sentences with a scene noun (The ...)",
     "start fewer sentences with 'The ...' staging"),
    ("opener_he_she_share", "narration sentences starting 'He'/'She'", Unit::Share,
     "start more sentences with he or she",
     "vary openings away from he/she"),
    ("opener_and_but_share", "narration sentences starting 'And'/'But'", Unit::Share,
     "start a few sentences with And or But",
     "start fewer sentences with And or But"),
];

/// Function-word rates are added per profile: its most frequent function
/// words, each as "word_<w>_per_1k".
pub const FUNCTION_WORDS_TRACKED: usize = 40;

/// The metric definitions, core first, then one rate per tracked function word.
pub fn defs(function_words: &[String]) -> Vec<Def> {
    let mut out: Vec<Def> = CORE
        .iter()
        .map(|(k, l, u, r, lo)| Def {
            key: k.to_string(),
            label: l.to_string(),
            unit: *u,
            raise: r,
            lower: lo,
        })
        .collect();
    for w in function_words {
        out.push(Def {
            key: format!("word_{w}_per_1k"),
            label: format!("'{w}' per 1k words"),
            unit: Unit::Per1k,
            raise: "use this word more where it fits naturally",
            lower: "use this word less",
        });
    }
    out
}

/// The profile's most frequent function words: their rates carry much of a
/// voice's feel ("but", "as", "would" separate confessional narrators from
/// observational ones).
pub fn top_function_words(p: &Profile, n: usize) -> Vec<String> {
    let mut v: Vec<(&String, u64)> = p
        .word_counts
        .iter()
        .filter(|(w, _)| lexicon::is_function_word(w))
        .map(|(w, c)| (w, *c))
        .collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    v.into_iter().take(n).map(|(w, _)| w.clone()).collect()
}

const FIRST_PERSON: [&str; 9] = ["i", "me", "my", "mine", "myself", "i'm", "i'd", "i've", "i'll"];
const PRESENT: [&str; 12] = [
    "is", "am", "are", "has", "says", "does", "isn't", "aren't", "doesn't", "goes", "looks", "walks",
];
const PAST: [&str; 12] = [
    "was", "were", "had", "said", "did", "wasn't", "weren't", "didn't", "went", "looked", "walked",
    "hadn't",
];
const COLOURS: [&str; 16] = [
    "red", "blue", "green", "white", "black", "gray", "grey", "pink", "brown", "gold", "golden",
    "silver", "amber", "copper", "crimson", "purple",
];
const EMOTIONS: [&str; 30] = [
    "angry", "mad", "pissed", "furious", "upset", "sad", "happy", "scared", "terrified", "afraid",
    "nervous", "anxious", "embarrassed", "guilty", "jealous", "hurt", "devastated", "relieved",
    "confused", "shocked", "panic", "love", "hate", "worried", "disappointed", "ashamed", "excited",
    "lonely", "miserable", "humiliated",
];
/// Words ending in -ly that are not manner adverbs.
const NOT_LY_ADVERBS: [&str; 34] = [
    "family", "only", "early", "reply", "supply", "apply", "rely", "belly", "jelly", "bully",
    "holy", "ugly", "lovely", "lonely", "silly", "friendly", "likely", "daily", "july", "italy",
    "lily", "emily", "kelly", "molly", "sally", "holly", "ally", "rally", "assembly", "butterfly",
    "elderly", "curly", "chilly", "unlikely",
];
/// Words ending in -ing that are not participles.
const NOT_PARTICIPLES: [&str; 18] = [
    "thing", "things", "nothing", "something", "anything", "everything", "morning", "evening",
    "during", "king", "ring", "sing", "bring", "spring", "string", "wing", "ceiling", "building",
];
/// Words after "but" that make an ordinary clause, not a "not X but Y"
/// contrast (from slop-score's stage-1 patterns, MIT, Sam Paech).
const BUT_EXCLUDE: [&str; 21] = [
    "when", "while", "which", "who", "where", "if", "that", "as", "because", "although",
    "though", "until", "unless", "here", "there", "then", "my", "we", "i", "you", "anything",
];

fn per_1k(n: usize, words: usize) -> f64 {
    if words == 0 { 0.0 } else { n as f64 * 1000.0 / words as f64 }
}

fn share(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

fn mean_sd(xs: &[f64]) -> (f64, f64) {
    if xs.is_empty() {
        return (0.0, 0.0);
    }
    let m = xs.iter().sum::<f64>() / xs.len() as f64;
    let v = xs.iter().map(|x| (x - m).powi(2)).sum::<f64>() / xs.len() as f64;
    (m, v.sqrt())
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i.min(sorted.len() - 1)]
}

/// Vowel-group syllable estimate (silent final e dropped).
pub fn syllables(w: &str) -> usize {
    let w = w.trim_matches('\'');
    let mut groups = 0;
    let mut prev = false;
    for c in w.chars() {
        let v = "aeiouy".contains(c);
        if v && !prev {
            groups += 1;
        }
        prev = v;
    }
    if w.ends_with('e') && groups > 1 && !w.ends_with("le") {
        groups -= 1;
    }
    groups.max(1)
}

/// Moving-average type-token ratio (Covington & McFall 2010): mean TTR over
/// every `win`-token window. Texts shorter than the window get plain TTR.
fn mattr(tokens: &[String], win: usize) -> f64 {
    if tokens.is_empty() {
        return 0.0;
    }
    if tokens.len() <= win {
        let set: std::collections::HashSet<&String> = tokens.iter().collect();
        return set.len() as f64 / tokens.len() as f64;
    }
    let mut counts: HashMap<&str, u32> = HashMap::new();
    let mut distinct = 0usize;
    for t in &tokens[..win] {
        let c = counts.entry(t.as_str()).or_insert(0);
        if *c == 0 {
            distinct += 1;
        }
        *c += 1;
    }
    let mut total = distinct as f64;
    let mut n = 1usize;
    for i in win..tokens.len() {
        let out = counts.get_mut(tokens[i - win].as_str()).unwrap();
        *out -= 1;
        if *out == 0 {
            distinct -= 1;
        }
        let c = counts.entry(tokens[i].as_str()).or_insert(0);
        if *c == 0 {
            distinct += 1;
        }
        *c += 1;
        total += distinct as f64;
        n += 1;
    }
    total / n as f64 / win as f64
}

/// Measure of textual lexical diversity (McCarthy & Jarvis 2010), the mean
/// of a forward and a backward pass at the standard 0.72 TTR threshold.
fn mtld(tokens: &[String]) -> f64 {
    fn pass<'a>(it: impl Iterator<Item = &'a String>) -> f64 {
        let mut factors = 0.0;
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut n = 0usize;
        let mut ttr = 1.0;
        for t in it {
            n += 1;
            seen.insert(t.as_str());
            ttr = seen.len() as f64 / n as f64;
            if ttr <= 0.72 {
                factors += 1.0;
                seen.clear();
                n = 0;
                ttr = 1.0;
            }
        }
        if n > 0 {
            factors += (1.0 - ttr) / (1.0 - 0.72);
        }
        factors
    }
    if tokens.is_empty() {
        return 0.0;
    }
    let f = pass(tokens.iter());
    let b = pass(tokens.iter().rev());
    let len = tokens.len() as f64;
    let fwd = if f > 0.0 { len / f } else { len };
    let bwd = if b > 0.0 { len / b } else { len };
    (fwd + bwd) / 2.0
}

/// A paragraph with its dialogue removed: what the narrator says.
pub fn narration(paragraph: &str) -> String {
    let mut out = paragraph.to_string();
    for span in text::quoted_spans(paragraph) {
        if !span.is_empty() {
            out = out.replacen(&span, " ", 1);
        }
    }
    out
}

/// Similes: "like a/an/the/someone/something", "as if", "as though".
pub fn is_simile_at(tokens: &[String], i: usize) -> bool {
    let next = tokens.get(i + 1).map(String::as_str);
    match tokens[i].as_str() {
        "like" => matches!(next, Some("a" | "an" | "the" | "someone" | "something")),
        "as" => matches!(next, Some("if" | "though")),
        _ => false,
    }
}

/// "the way he/she/..." as a description frame.
pub fn is_the_way_at(tokens: &[String], i: usize) -> bool {
    tokens[i] == "the"
        && tokens.get(i + 1).is_some_and(|t| t == "way")
        && tokens.get(i + 2).is_some_and(|t| {
            matches!(
                t.as_str(),
                "he" | "she" | "i" | "it" | "they" | "you" | "his" | "her" | "we" | "my" | "your" | "their"
            )
        })
}

pub fn is_ly_adverb(w: &str) -> bool {
    w.len() > 4 && w.ends_with("ly") && !NOT_LY_ADVERBS.contains(&w)
}

/// Sentences chaining three or more "and"s.
pub fn is_and_chain(sentence_tokens: &[String]) -> bool {
    sentence_tokens.iter().filter(|t| *t == "and").count() >= 3
}

/// Trailing participial clauses: ", <verb>ing ..." after at least three
/// words of main clause ("She left the room, slamming the door").
pub fn trailing_ing_count(sentence: &str) -> usize {
    let mut n = 0;
    let lower = sentence.to_lowercase();
    for (pos, _) in lower.match_indices(", ") {
        let before = text::count_words(&lower[..pos]);
        let word: String = lower[pos + 2..]
            .chars()
            .take_while(|c| c.is_alphabetic())
            .collect();
        if before >= 3
            && word.len() >= 5
            && word.ends_with("ing")
            && !NOT_PARTICIPLES.contains(&word.as_str())
        {
            n += 1;
        }
    }
    n
}

/// "Not X, but Y" contrast frames, ported from the stage-1 patterns of Sam
/// Paech's slop-score (MIT): "not X, but Y" within a sentence, the dash
/// form "not X — it's Y", and the two-sentence form "It's not X. It's Y."
/// Returns the number of frames in a paragraph's sentence sequence.
pub fn not_but_count(sentences: &[String]) -> usize {
    let mut n = 0;
    let lowered: Vec<String> = sentences.iter().map(|s| s.to_lowercase().replace('\u{2019}', "'")).collect();
    for s in &lowered {
        n += not_but_in_sentence(s);
    }
    for pair in lowered.windows(2) {
        if negated_pronoun_start(&pair[0]) && pronoun_be_start(&pair[1]) {
            n += 1;
        }
    }
    n
}

fn is_negation(t: &str) -> bool {
    t == "not" || t.ends_with("n't")
}

fn not_but_in_sentence(s: &str) -> usize {
    let toks = text::word_tokens(s);
    let Some(neg) = toks.iter().position(|t| {
        is_negation(t)
    }) else {
        return 0;
    };
    if toks.get(neg + 1).is_some_and(|t| t == "that" || t == "only") {
        return 0;
    }
    let mut n = 0;
    // ", but Y" / "; but Y" / ": but Y" after the negation.
    for sep in [", but ", "; but ", ": but "] {
        for (pos, _) in s.match_indices(sep) {
            let head = text::word_tokens(&s[..pos]);
            if !head.iter().any(|t| is_negation(t)) {
                continue;
            }
            let next: String = s[pos + sep.len()..]
                .chars()
                .take_while(|c| c.is_alphabetic() || *c == '\'')
                .collect();
            if !BUT_EXCLUDE.contains(&next.as_str()) {
                n += 1;
            }
        }
    }
    // Dash form: "... not X — it's/they're Y".
    for dash in ['\u{2014}', '\u{2013}'] {
        for (pos, _) in s.match_indices(dash) {
            let head = text::word_tokens(&s[..pos]);
            if !head.iter().any(|t| is_negation(t)) {
                continue;
            }
            let tail = text::word_tokens(&s[pos..]);
            if tail.first().is_some_and(|t| {
                matches!(t.as_str(), "it" | "it's" | "they" | "they're" | "this" | "that" | "that's")
            }) {
                n += 1;
            }
        }
    }
    n
}

fn negated_pronoun_start(s: &str) -> bool {
    let t = text::word_tokens(s);
    let first = t.first().map(String::as_str);
    let pron = matches!(first, Some("it" | "they" | "this" | "that" | "it's" | "they're" | "that's"));
    pron && t.iter().take(4).any(|w| is_negation(w))
}

fn pronoun_be_start(s: &str) -> bool {
    let t = text::word_tokens(s);
    match (t.first().map(String::as_str), t.get(1).map(String::as_str)) {
        (Some("it's" | "they're" | "that's"), next) => next != Some("not"),
        (Some("it" | "they" | "this" | "that"), Some("is" | "are" | "was" | "were")) => {
            t.get(2).map(String::as_str) != Some("not")
        }
        _ => false,
    }
}

/// Three-word phrases with at least two content words that repeat inside
/// the passage: unintended echoes ("her dark eyes ... her dark eyes").
pub fn echo_phrases(tokens: &[String]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for w in tokens.windows(3) {
        let content = w.iter().filter(|t| !lexicon::is_function_word(t)).count();
        if content >= 2 {
            *counts.entry(w.join(" ")).or_insert(0) += 1;
        }
    }
    let mut v: Vec<(String, usize)> = counts.into_iter().filter(|(_, c)| *c >= 2).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v
}

/// Measure a passage (prose paragraphs). Values are in `defs` order: the
/// core metrics, then the rate of each of `function_words`.
pub fn measure(paragraphs: &[String], function_words: &[String]) -> Vec<f64> {
    let style = StyleCounts::of_paragraphs(paragraphs);
    let mut tokens: Vec<String> = Vec::new();
    let mut sent_lens: Vec<f64> = Vec::new();
    let mut para_lens: Vec<f64> = Vec::new();
    let (mut one_sentence, mut dialogue_paras) = (0usize, 0usize);
    let (mut and_chains, mut trailing, mut not_but) = (0usize, 0usize, 0usize);
    let (mut narr_q, mut narr_sents) = (0usize, 0usize);
    let (mut op_i, mut op_the, mut op_hs, mut op_ab) = (0usize, 0usize, 0usize, 0usize);
    let (mut present, mut past) = (0usize, 0usize);
    let mut which = 0usize;

    for p in paragraphs {
        let sents = text::sentences(p);
        let mut n_sent = 0;
        for s in &sents {
            let st = text::word_tokens(s);
            if st.is_empty() {
                continue;
            }
            n_sent += 1;
            sent_lens.push(st.len() as f64);
            if is_and_chain(&st) {
                and_chains += 1;
            }
            trailing += trailing_ing_count(s);
        }
        not_but += not_but_count(&sents);
        if n_sent == 1 {
            one_sentence += 1;
        }
        let lower = p.to_lowercase();
        which += lower
            .match_indices(", which")
            .filter(|(i, m)| {
                !lower[i + m.len()..]
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic())
            })
            .count();
        if !text::quoted_spans(p).is_empty() {
            dialogue_paras += 1;
        }
        let narr = narration(p);
        narr_q += narr.matches('?').count();
        for s in text::sentences(&narr) {
            let st = text::word_tokens(&s);
            let Some(first) = st.first() else { continue };
            narr_sents += 1;
            match first.as_str() {
                "i" | "i'm" | "i'd" | "i've" | "i'll" => op_i += 1,
                "the" => op_the += 1,
                "he" | "she" | "he's" | "she's" | "he'd" | "she'd" => op_hs += 1,
                "and" | "but" => op_ab += 1,
                _ => {}
            }
            for w in &st {
                if PRESENT.contains(&w.as_str()) {
                    present += 1;
                } else if PAST.contains(&w.as_str()) {
                    past += 1;
                }
            }
        }
        let pt = text::word_tokens(p);
        para_lens.push(pt.len() as f64);
        tokens.extend(pt);
    }

    let n = tokens.len();
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for t in &tokens {
        *counts.entry(t.as_str()).or_insert(0) += 1;
    }
    let count = |w: &str| counts.get(w).copied().unwrap_or(0);
    let sum_of = |ws: &[&str]| ws.iter().map(|w| count(w)).sum::<usize>();

    let (sl_mean, sl_sd) = mean_sd(&sent_lens);
    let mut sorted = sent_lens.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let autocorr = {
        let denom: f64 = sent_lens.iter().map(|x| (x - sl_mean).powi(2)).sum();
        if denom <= 0.0 || sent_lens.len() < 3 {
            0.0
        } else {
            sent_lens
                .windows(2)
                .map(|w| (w[0] - sl_mean) * (w[1] - sl_mean))
                .sum::<f64>()
                / denom
        }
    };
    let (pl_mean, pl_sd) = mean_sd(&para_lens);
    let n_sents = sent_lens.len();
    let syll: usize = tokens.iter().map(|t| syllables(t)).sum();
    let letters: usize = tokens
        .iter()
        .map(|t| t.chars().filter(|c| c.is_alphabetic()).count())
        .sum();
    let long_words = tokens
        .iter()
        .filter(|t| t.chars().filter(|c| c.is_alphabetic()).count() >= 7)
        .count();
    let contractions = tokens.iter().filter(|t| style::is_contraction(t)).count();
    let ly = tokens.iter().filter(|t| is_ly_adverb(t)).count();
    let (mut similes, mut the_way) = (0usize, 0usize);
    for i in 0..n {
        if is_simile_at(&tokens, i) {
            similes += 1;
        }
        if is_the_way_at(&tokens, i) {
            the_way += 1;
        }
    }
    let wps = share(n, n_sents);
    let spw = share(syll, n);
    let contractable = style.contractions + style.expanded_forms;

    let mut v = vec![
        sl_mean,
        quantile(&sorted, 0.5),
        quantile(&sorted, 0.9),
        sl_sd,
        if sl_mean > 0.0 { sl_sd / sl_mean } else { 0.0 },
        share(sent_lens.iter().filter(|x| **x <= 5.0).count(), n_sents),
        share(sent_lens.iter().filter(|x| **x >= 25.0).count(), n_sents),
        autocorr,
        pl_mean,
        if pl_mean > 0.0 { pl_sd / pl_mean } else { 0.0 },
        share(one_sentence, paragraphs.len()),
        share(dialogue_paras, paragraphs.len()),
        share(style.dialogue_words as usize, n),
        share(letters, n),
        spw,
        if n_sents == 0 { 0.0 } else { 0.39 * wps + 11.8 * spw - 15.59 },
        mattr(&tokens, 500),
        mtld(&tokens),
        share(long_words, n),
        per_1k(contractions, n),
        if contractable == 0 { 0.0 } else { style.contractions as f64 / contractable as f64 },
        per_1k(ly, n),
        per_1k(style.questions as usize, n),
        per_1k(narr_q, n),
        per_1k(style.exclamations as usize, n),
        per_1k(style.em_dashes as usize, n),
        per_1k(style.ellipses as usize, n),
        per_1k(style.semicolons as usize, n),
        per_1k(style.colons as usize, n),
        per_1k(style.parentheses as usize, n),
        per_1k(style.commas as usize, n),
        per_1k(sum_of(&FIRST_PERSON), n),
        share(present, present + past),
        per_1k(similes, n),
        per_1k(which, n),
        per_1k(the_way, n),
        per_1k(and_chains, n_sents) ,
        per_1k(trailing, n),
        per_1k(not_but, n),
        per_1k(echo_phrases(&tokens).len(), n),
        per_1k(sum_of(&COLOURS), n),
        per_1k(sum_of(&EMOTIONS), n),
        share(op_i, narr_sents),
        share(op_the, narr_sents),
        share(op_hs, narr_sents),
        share(op_ab, narr_sents),
    ];
    debug_assert_eq!(v.len(), CORE.len());
    for w in function_words {
        v.push(per_1k(count(w), n));
    }
    v
}

/// A metric's band in the author's own text at one length.
#[derive(Debug, Clone, Serialize)]
pub struct Band {
    pub key: String,
    pub label: String,
    pub unit: Unit,
    pub low: f64,
    pub median: f64,
    pub high: f64,
}

/// The author's metric bands at one window length.
#[derive(Debug, Clone, Serialize)]
pub struct Ranges {
    /// Requested window length (words).
    pub window_words: usize,
    /// Base passages (~250 words) summed per window.
    pub passages_per_window: usize,
    /// Windows of the author's text measured.
    pub windows: usize,
    /// Fewer than 8 windows: the bands are rough.
    pub directional: bool,
    pub bands: Vec<Band>,
    #[serde(skip)]
    pub defs: Vec<Def>,
}

/// Weighted percentile: the smallest value whose cumulative weight reaches
/// `q` of the total.
fn weighted_quantile(pairs: &[(f64, f64)], q: f64) -> f64 {
    let total: f64 = pairs.iter().map(|p| p.1).sum();
    if pairs.is_empty() || total <= 0.0 {
        return 0.0;
    }
    let mut acc = 0.0;
    for (v, w) in pairs {
        acc += w;
        if acc >= q * total - 1e-12 {
            return *v;
        }
    }
    pairs[pairs.len() - 1].0
}

/// Windows of `window` words over the profile: spans of k = round(window /
/// 250) consecutive passages within one work, each measured, with the
/// passages' weights (so a 60/40 blend weighs its components 60/40).
pub fn window_measures(p: &Profile, window: usize, function_words: &[String]) -> (usize, Vec<(Vec<f64>, f64)>) {
    let k = ((window as f64 / crate::engine::profile::BASE_CHUNK_WORDS as f64).round() as usize).max(1);
    let rows = p
        .spans(k)
        .iter()
        .map(|span| {
            let paras: Vec<String> = span
                .iter()
                .flat_map(|&i| p.chunks[i].paragraphs.iter().cloned())
                .collect();
            let w: f64 = span.iter().map(|&i| p.chunks[i].weight as f64).sum();
            (measure(&paras, function_words), w)
        })
        .collect();
    (k, rows)
}

/// The author's p15 / p50 / p85 band per metric at `window` words.
pub fn ranges(p: &Profile, window: usize, function_words: &[String]) -> Ranges {
    let defs = defs(function_words);
    let (k, rows) = window_measures(p, window, function_words);
    let bands = defs
        .iter()
        .enumerate()
        .map(|(j, d)| {
            let mut pairs: Vec<(f64, f64)> = rows.iter().map(|(v, w)| (v[j], *w)).collect();
            pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
            Band {
                key: d.key.clone(),
                label: d.label.clone(),
                unit: d.unit,
                low: round_unit(weighted_quantile(&pairs, 0.15), d.unit),
                median: round_unit(weighted_quantile(&pairs, 0.5), d.unit),
                high: round_unit(weighted_quantile(&pairs, 0.85), d.unit),
            }
        })
        .collect();
    Ranges {
        window_words: window,
        passages_per_window: k,
        windows: rows.len(),
        directional: rows.len() < 8,
        bands,
        defs,
    }
}

/// Where a value sits against a band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    InRange,
    /// Outside the p15–p85 band but within tolerance: a nudge, not a fail.
    Near,
    /// Clearly outside: beyond the band by more than half its width.
    OutOfRange,
}

/// Judge a value against a band. A p15–p85 band holds only 70% of the
/// author's own windows, so a hard edge there would fail the author on a
/// third of their metrics. The tolerance is half the band width (about the
/// 2nd–98th percentile for a normal spread), never less than one event in
/// the window for rates, so the author's own text passes and real drift
/// does not.
pub fn status(value: f64, band: &Band, window_words: usize) -> Status {
    if value >= band.low && value <= band.high {
        return Status::InRange;
    }
    let floor = match band.unit {
        Unit::Per1k => 1000.0 / window_words.max(1) as f64,
        Unit::Per1kSentences => 1000.0 / (window_words.max(1) as f64 / 12.0).max(1.0),
        Unit::Share => 0.02,
        Unit::Words | Unit::Score => 0.05 * band.median.abs().max(0.1),
    };
    let tol = (0.5 * (band.high - band.low)).max(floor);
    if value < band.low - tol || value > band.high + tol {
        Status::OutOfRange
    } else {
        Status::Near
    }
}

/// Round for display: shares and scores to 3 decimals, rates and lengths to 1–2.
pub fn round_unit(v: f64, unit: Unit) -> f64 {
    let f = match unit {
        Unit::Share | Unit::Score => 1000.0,
        Unit::Per1k | Unit::Words => 100.0,
        Unit::Per1kSentences => 10.0,
    };
    (v * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paras(t: &str) -> Vec<String> {
        text::paragraphs(t)
    }

    fn get(v: &[f64], key: &str) -> f64 {
        let d = defs(&[]);
        v[d.iter().position(|d| d.key == key).unwrap()]
    }

    #[test]
    fn measures_rhythm_and_constructions() {
        let t = "I wait. The rain falls and the wind blows and the door bangs and nobody comes.\n\n\
                 \"Why?\" she asks. It's not fear, but something else. I leave the room, slamming the door.\n\n\
                 Why am I here? She looks at me like a stranger.";
        let v = measure(&paras(t), &[]);
        assert_eq!(v.len(), CORE.len());
        assert!(get(&v, "and_chains_per_1k_sentences") > 0.0);
        assert!(get(&v, "not_but_per_1k") > 0.0);
        assert!(get(&v, "trailing_ing_per_1k") > 0.0);
        assert!(get(&v, "similes_per_1k") > 0.0);
        assert!(get(&v, "narration_questions_per_1k") > 0.0);
        assert!(get(&v, "dialogue_paragraph_share") > 0.3);
        assert!(get(&v, "present_tense_share") > 0.5);
    }

    #[test]
    fn not_but_skips_ordinary_clauses() {
        let s = vec!["I did not go, but when I did, it rained.".to_string()];
        assert_eq!(not_but_count(&s), 0);
        let s = vec!["It's not the money. It's the principle.".to_string()];
        let s2: Vec<String> = text::sentences(&s[0]);
        assert_eq!(not_but_count(&s2), 1);
    }

    #[test]
    fn mtld_and_mattr_are_sane() {
        let toks: Vec<String> = "a b c d e f g h a b c d".split(' ').map(String::from).collect();
        assert!(mattr(&toks, 500) > 0.6);
        let m = mattr(&toks, 4);
        assert!(m > 0.9 && m <= 1.0, "{m}");
        assert!(mtld(&toks) > 1.0);
        assert_eq!(syllables("cake"), 1);
        assert_eq!(syllables("table"), 2);
    }

    #[test]
    fn status_tolerates_near_misses() {
        let b = Band { key: "x".into(), label: "x".into(), unit: Unit::Share, low: 0.2, median: 0.25, high: 0.3 };
        assert_eq!(status(0.25, &b, 1000), Status::InRange);
        assert_eq!(status(0.33, &b, 1000), Status::Near);
        assert_eq!(status(0.5, &b, 1000), Status::OutOfRange);
    }
}
