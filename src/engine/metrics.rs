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
    /// Occurrences in one item (emoji in a post, lines in a message).
    Count,
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
    ("dialogue_tags_per_1k", "speech tags (said, asked...) per 1k words", Unit::Per1k,
     "tag more lines of speech (she said, he asked)",
     "drop tags where the speaker is already clear"),
    ("pov_first_share", "first-person share of narration pronouns (I vs he/she/they)", Unit::Share,
     "narrate from inside the narrator: I, me, my",
     "narrate from outside: he, she, they"),
    ("you_per_1k", "you/your per 1k words", Unit::Per1k,
     "talk to the reader directly: you, your",
     "address the reader less often"),
    ("we_per_1k", "we/us/our per 1k words", Unit::Per1k,
     "speak as 'we' more often",
     "speak as 'we' less often"),
    ("digits_per_1k", "numbers written in digits per 1k words", Unit::Per1k,
     "use concrete figures, in digits",
     "use fewer figures"),
    ("headings_per_1k", "headings per 1k words", Unit::Per1k,
     "break the piece into sections with headings",
     "use fewer headings; let paragraphs carry the structure"),
    ("list_line_share", "list items among paragraphs", Unit::Share,
     "put enumerations into bullet lists",
     "turn lists back into prose"),
    ("numbered_list_share", "numbered-list items among paragraphs", Unit::Share,
     "number the steps or points",
     "drop the numbering; use prose or plain bullets"),
    ("bold_per_1k", "bold spans per 1k words", Unit::Per1k,
     "bold the key phrase now and then",
     "cut bold emphasis"),
    ("italic_per_1k", "italic spans per 1k words", Unit::Per1k,
     "italicise stressed words and titles",
     "cut italics"),
    ("links_per_1k", "web links per 1k words", Unit::Per1k,
     "link sources and examples",
     "link less"),
    ("blockquotes_per_1k", "block quotes per 1k words", Unit::Per1k,
     "quote sources in block quotes",
     "fold block quotes into the prose"),
    ("code_blocks_per_1k", "code blocks per 1k words", Unit::Per1k,
     "show code in fenced blocks",
     "cut code blocks"),
    ("opening_paragraph_words", "first paragraph length (words)", Unit::Words,
     "open with a fuller first paragraph",
     "open with a shorter first paragraph"),
    ("llm_tells_per_1k", "stock LLM words and phrases per 1k words", Unit::Per1k,
     "",
     "cut the stock LLM phrasing; say it the author's way"),
];

/// Keys that count markdown markup. A corpus of plain .txt books has none,
/// so these are `unmeasured` (left out of score, plan and brief) unless at
/// least `MARKUP_MIN_SOURCES` of the author's sources use that markup.
pub const MARKUP_KEYS: [&str; 8] = [
    "headings_per_1k", "list_line_share", "numbered_list_share", "bold_per_1k", "italic_per_1k",
    "links_per_1k", "blockquotes_per_1k", "code_blocks_per_1k",
];
pub const MARKUP_MIN_SOURCES: f64 = 0.3;

/// Measures computed from other measures, or too abstract to act on: they
/// count toward the score but never become a plan step or a finding.
/// (`llm_tells_per_1k` is acted on through the llm_tell findings, which
/// carry the words and lines.)
pub const DERIVED_KEYS: [&str; 7] = [
    "mattr_500", "mtld", "fk_grade", "sentence_length_autocorr", "sentence_words_cv", "syllables_per_word",
    "llm_tells_per_1k",
];

pub fn is_derived(key: &str) -> bool {
    DERIVED_KEYS.contains(&key) || is_function_word_key(key)
}

pub fn is_function_word_key(key: &str) -> bool {
    key.starts_with("word_") && key.ends_with("_per_1k")
}

/// The family a metric belongs to. The voice distance averages within each
/// family, then across families, so 40 function-word rates or 8 sentence
/// length variants can't drown out paragraphing or punctuation.
pub fn family(key: &str) -> &'static str {
    if is_function_word_key(key) {
        return "function_words";
    }
    // Its own family: one of ~40 lexis measures it would barely move the
    // score, yet stock LLM phrasing is the clearest sign a draft isn't theirs.
    if key == "llm_tells_per_1k" {
        return "tells";
    }
    if key.starts_with("item_")
        || key.ends_with("_per_item")
        || key.starts_with("has_")
        || matches!(key, "lowercase_start_share" | "ends_with_punct_share" | "first_line_words")
    {
        return "items";
    }
    match key {
        k if k.starts_with("sentence_") || k == "short_sentence_share" || k == "long_sentence_share" => "rhythm",
        "paragraph_words_mean" | "paragraph_words_cv" | "one_sentence_paragraph_share" => "paragraphing",
        "dialogue_paragraph_share" | "dialogue_word_share" | "dialogue_tags_per_1k" => "dialogue",
        "questions_per_1k" | "narration_questions_per_1k" | "exclamations_per_1k" | "em_dashes_per_1k"
        | "ellipses_per_1k" | "semicolons_per_1k" | "colons_per_1k" | "parentheses_per_1k"
        | "commas_per_1k" => "punctuation",
        "similes_per_1k" | "which_clauses_per_1k" | "the_way_per_1k" | "and_chains_per_1k_sentences"
        | "trailing_ing_per_1k" | "not_but_per_1k" | "echo_phrases_per_1k" | "opener_i_share"
        | "opener_the_share" | "opener_he_she_share" | "opener_and_but_share" => "constructions",
        k if MARKUP_KEYS.contains(&k) || k == "opening_paragraph_words" || k == "digits_per_1k" => "structure",
        _ => "lexis",
    }
}

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

/// How a paragraph-level block reads in markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    Heading,
    /// A ``` or ~~~ fence line (opening or closing a code block).
    Fence,
    Quote,
    /// A bullet-list block with this many items.
    Bullets(usize),
    /// A numbered-list block with this many items.
    Numbered(usize),
    Prose,
}

fn starts_numbered(t: &str) -> bool {
    let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
    (1..=3).contains(&digits) && {
        let rest = &t[digits..];
        rest.starts_with(". ") || rest.starts_with(") ")
    }
}

/// Classify a block. Lists are counted by their markers, because stored
/// paragraphs join a list's lines with spaces ("- a - b - c").
pub fn block_kind(p: &str) -> Block {
    let t = p.trim_start();
    if t.starts_with('#') && t.trim_start_matches('#').starts_with(' ') {
        return Block::Heading;
    }
    if t.starts_with("```") || t.starts_with("~~~") {
        return Block::Fence;
    }
    if t.starts_with('>') {
        return Block::Quote;
    }
    for m in ["- ", "* ", "+ ", "• "] {
        if t.starts_with(m) {
            let sep = format!(" {m}");
            return Block::Bullets(1 + t.matches(sep.as_str()).count());
        }
    }
    if starts_numbered(t) {
        let n = t
            .split(' ')
            .filter(|w| {
                let d = w.chars().take_while(|c| c.is_ascii_digit()).count();
                (1..=3).contains(&d) && matches!(&w[d..], "." | ")")
            })
            .count();
        return Block::Numbered(n.max(1));
    }
    Block::Prose
}

/// Inline markup counts in one paragraph: (bold spans, italic spans, links).
/// Links are only web links (`](http`, bare URLs): `](#anchor)` contents
/// links in converted books are not a linking habit.
pub fn inline_markup(p: &str) -> (usize, usize, usize) {
    let bold = p.matches("**").count() / 2 + p.matches("__").count() / 2;
    let chars: Vec<char> = p.chars().collect();
    let mut singles = 0usize;
    for (i, &c) in chars.iter().enumerate() {
        if c != '*' && c != '_' {
            continue;
        }
        let prev = if i == 0 { None } else { Some(chars[i - 1]) };
        let next = chars.get(i + 1).copied();
        if prev == Some(c) || next == Some(c) {
            continue; // part of ** / __
        }
        if c == '*' && (i == 0 || prev.is_some_and(char::is_whitespace)) && next.is_none_or(char::is_whitespace) {
            continue; // a bullet or a lone star
        }
        // `_` inside a word (snake_case, URLs) is not emphasis.
        let inner = prev.is_some_and(char::is_alphanumeric) && next.is_some_and(char::is_alphanumeric);
        if c == '_' && inner {
            continue;
        }
        singles += 1;
    }
    let mut links = 0usize;
    for tok in p.split_whitespace() {
        if tok.contains("](http") || tok.contains("](www.") {
            links += 1;
            continue;
        }
        let bare = tok.trim_start_matches(['(', '[', '<', '"', '\u{201C}']);
        if (bare.starts_with("http://") || bare.starts_with("https://") || bare.starts_with("www.")) && !tok.contains("](") {
            links += 1;
        }
    }
    (bold, singles / 2, links)
}

fn is_url_word(w: &str) -> bool {
    let b = w.trim_start_matches(['(', '[', '<', '"', '\u{201C}']);
    b.starts_with("http://") || b.starts_with("https://") || b.starts_with("www.")
}

/// Prose without web links, line by line: a markdown link keeps its text
/// and loses its target, a bare URL goes (its trailing full stop or comma
/// stays, so sentences still end). Otherwise "https:" counts as a colon,
/// "](" as a parenthesis, and URL path pieces as words, digits and echo
/// phrases. Link counts come from the raw text (`inline_markup`).
pub fn strip_links(p: &str) -> String {
    let mut lines = Vec::new();
    for line in p.split('\n') {
        let mut out = String::with_capacity(line.len());
        let mut rest = line;
        while let Some(i) = rest.find("](") {
            let before = &rest[..i];
            // "[text](target)" → "text"; a stray "](" just loses its target.
            match before.rfind('[') {
                Some(o) => {
                    out.push_str(&before[..o]);
                    out.push_str(&before[o + 1..]);
                }
                None => out.push_str(before),
            }
            // The target ends at its matching ')': Wikipedia targets hold
            // balanced parentheses of their own.
            let target = &rest[i + 2..];
            let mut depth = 0usize;
            let close = target.char_indices().find(|&(_, c)| match c {
                '(' => {
                    depth += 1;
                    false
                }
                ')' if depth == 0 => true,
                ')' => {
                    depth -= 1;
                    false
                }
                _ => false,
            });
            rest = close.map(|(j, _)| &target[j + 1..]).unwrap_or("");
        }
        out.push_str(rest);
        if !out.contains("http") && !out.contains("www.") {
            lines.push(out);
            continue;
        }
        let words: Vec<String> = out
            .split(' ')
            .filter_map(|w| {
                if !is_url_word(w) {
                    return Some(w.to_string());
                }
                let keep: String = w.chars().rev().take_while(|c| matches!(c, '.' | ',' | ';' | '!' | '?')).collect();
                (!keep.is_empty()).then(|| keep.chars().rev().collect())
            })
            .collect();
        // A URL's kept full stop belongs to the word before it.
        let mut joined = String::with_capacity(out.len());
        for w in words {
            if !joined.is_empty() && !w.chars().all(|c| matches!(c, '.' | ',' | ';' | '!' | '?')) {
                joined.push(' ');
            }
            joined.push_str(&w);
        }
        lines.push(joined);
    }
    lines.join("\n")
}

const YOU: [&str; 9] = ["you", "your", "yours", "yourself", "yourselves", "you're", "you've", "you'll", "you'd"];
const WE: [&str; 9] = ["we", "us", "our", "ours", "ourselves", "we're", "we've", "we'll", "we'd"];
const THIRD_PERSON: [&str; 21] = [
    "he", "she", "him", "her", "his", "hers", "himself", "herself", "they", "them", "their",
    "theirs", "themselves", "he's", "she's", "he'd", "she'd", "he'll", "she'll", "they're", "they'd",
];

/// Measure a passage (prose paragraphs). Values are in `defs` order: the
/// core metrics, then the rate of each of `function_words`.
pub fn measure(paragraphs: &[String], function_words: &[String]) -> Vec<f64> {
    measure_for(paragraphs, function_words, false)
}

/// `measure` with short-form sentence splitting (every line break and
/// every terminal mark ends a sentence, whatever the next letter's case).
/// Headings and code fences are counted as structure and kept out of the
/// prose measures; links are counted, then stripped (`strip_links`), so the
/// draft and the author's windows are measured on the same prose.
pub fn measure_for(all_paragraphs: &[String], function_words: &[String], short_form: bool) -> Vec<f64> {
    let (mut headings, mut fences, mut quotes, mut bullets, mut numbered, mut other_paras) =
        (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
    let (mut bold, mut italic, mut links) = (0usize, 0usize, 0usize);
    let mut prose_paras: Vec<String> = Vec::with_capacity(all_paragraphs.len());
    for p in all_paragraphs {
        let kind = block_kind(p);
        match kind {
            Block::Heading => {
                headings += 1;
                continue;
            }
            Block::Fence => {
                fences += 1;
                continue;
            }
            _ => {}
        }
        let (b, i, l) = inline_markup(p);
        bold += b;
        italic += i;
        links += l;
        let prose = strip_links(p);
        // A paragraph that was only a link is no paragraph of prose.
        if prose.trim().is_empty() {
            continue;
        }
        match kind {
            Block::Quote => {
                quotes += 1;
                other_paras += 1;
            }
            Block::Bullets(n) => bullets += n,
            Block::Numbered(n) => numbered += n,
            _ => other_paras += 1,
        }
        prose_paras.push(prose);
    }
    let paragraphs = &prose_paras[..];
    let style = StyleCounts::of_paragraphs(paragraphs);
    let (mut tags, mut narr_first, mut narr_third) = (0usize, 0usize, 0usize);
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
        let sents = text::sentences_for(p, short_form);
        tags += crate::engine::voice::tag_verbs_in(p).len();
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
        for s in text::sentences_for(&narr, short_form) {
            let st = text::word_tokens(&s);
            let Some(first) = st.first() else { continue };
            narr_sents += 1;
            for w in &st {
                if FIRST_PERSON.contains(&w.as_str()) {
                    narr_first += 1;
                } else if THIRD_PERSON.contains(&w.as_str()) {
                    narr_third += 1;
                }
            }
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
        per_1k(tags, n),
        share(narr_first, narr_first + narr_third),
        per_1k(sum_of(&YOU), n),
        per_1k(sum_of(&WE), n),
        per_1k(tokens.iter().filter(|t| t.chars().any(|c| c.is_ascii_digit())).count(), n),
        per_1k(headings, n),
        share(bullets + numbered, bullets + numbered + other_paras),
        share(numbered, bullets + numbered + other_paras),
        per_1k(bold, n),
        per_1k(italic, n),
        per_1k(links, n),
        per_1k(quotes, n),
        per_1k(fences.div_ceil(2), n),
        paragraphs.first().map(|p| text::count_words(p) as f64).unwrap_or(0.0),
        per_1k(crate::engine::voice::find_tells(&tokens).len(), n),
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
    /// Family (rhythm, paragraphing, dialogue, lexis, function_words,
    /// punctuation, constructions, structure, items).
    pub family: &'static str,
    /// Markup the author's sources barely use (under 30% of them): left out
    /// of the score, the plan and the brief.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub unmeasured: bool,
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

/// Legacy: spans of k = round(window / 250) consecutive passages within one
/// work, each measured, paired with `p.spans(k)`. Kept for callers that zip
/// rows with spans; new code uses `window_rows`, which follows
/// `Profile::windows` (pooling short works, lengths in words).
pub fn window_measures(p: &Profile, window: usize, function_words: &[String]) -> (usize, Vec<(Vec<f64>, f64)>) {
    let k = ((window as f64 / crate::engine::profile::BASE_CHUNK_WORDS as f64).round() as usize).max(1);
    let short = p.register.is_short_form();
    let rows = p
        .spans(k)
        .iter()
        .map(|span| {
            let paras: Vec<String> = span
                .iter()
                .flat_map(|&i| p.chunks[i].paragraphs.iter().cloned())
                .collect();
            let w: f64 = span.iter().map(|&i| p.chunks[i].weight as f64).sum();
            (measure_for(&paras, function_words, short), w)
        })
        .collect();
    (k, rows)
}

/// One measured window of the author's text.
#[derive(Debug, Clone)]
pub struct WindowRow {
    /// Chunk indices, in corpus order.
    pub chunks: Vec<usize>,
    pub values: Vec<f64>,
    /// Sum of the chunks' weights (a 60/40 blend weighs its parts 60/40).
    pub weight: f64,
}

impl WindowRow {
    pub fn source(&self, p: &Profile) -> u32 {
        p.chunks[self.chunks[0]].source
    }

    /// The window starts at its source's first passage: its first paragraph
    /// is an opening.
    pub fn is_opening(&self, p: &Profile) -> bool {
        let i = self.chunks[0];
        i == 0 || p.chunks[i - 1].source != p.chunks[i].source
    }
}

fn measure_chunks(p: &Profile, idx: &[usize], fw: &[String]) -> WindowRow {
    let paras: Vec<String> = idx.iter().flat_map(|&i| p.chunks[i].paragraphs.iter().cloned()).collect();
    WindowRow {
        chunks: idx.to_vec(),
        values: measure_for(&paras, fw, p.register.is_short_form()),
        weight: idx.iter().map(|&i| p.chunks[i].weight as f64).sum(),
    }
}

/// The author's windows of `words` words (`Profile::windows`), measured.
/// With `overlap`, a half-shifted window is added between each pair of
/// neighbours (second half of one + first half of the next), doubling the
/// resolution of a percentile when a novel only has ~30 windows.
pub fn window_rows(p: &Profile, words: usize, fw: &[String], overlap: bool) -> Vec<WindowRow> {
    let wins = p.windows(words);
    let mut sets: Vec<Vec<usize>> = Vec::with_capacity(wins.len() * 2);
    for (i, w) in wins.iter().enumerate() {
        sets.push(w.clone());
        let Some(next) = wins.get(i + 1).filter(|_| overlap) else { continue };
        let contiguous = w.last().is_some_and(|&l| l + 1 == next[0]);
        let pooled = p.register.is_short_form()
            || p.chunks[w[0]].source != p.chunks[*w.last().unwrap()].source
            || p.chunks[w.last().unwrap() + 1].source == p.chunks[w[0]].source;
        if contiguous && pooled && w.len() >= 2 && next.len() >= 2 {
            let mut mid: Vec<usize> = w[w.len() / 2..].to_vec();
            mid.extend_from_slice(&next[..next.len() / 2]);
            sets.push(mid);
        }
    }
    sets.iter().map(|s| measure_chunks(p, s, fw)).collect()
}

/// Share of the author's sources whose text contains each markup key.
pub fn markup_presence(p: &Profile) -> HashMap<&'static str, f64> {
    let n_src = p.sources.len().max(1);
    let mut has: HashMap<&'static str, std::collections::HashSet<u32>> = HashMap::new();
    for c in &p.chunks {
        for para in &c.paragraphs {
            let (b, i, l) = inline_markup(para);
            let kind = block_kind(para);
            let flags = [
                ("headings_per_1k", kind == Block::Heading),
                ("list_line_share", matches!(kind, Block::Bullets(_) | Block::Numbered(_))),
                ("numbered_list_share", matches!(kind, Block::Numbered(_))),
                ("bold_per_1k", b > 0),
                ("italic_per_1k", i > 0),
                ("links_per_1k", l > 0),
                ("blockquotes_per_1k", kind == Block::Quote),
                ("code_blocks_per_1k", kind == Block::Fence),
            ];
            for (k, on) in flags {
                if on {
                    has.entry(k).or_default().insert(c.source);
                }
            }
        }
    }
    MARKUP_KEYS
        .iter()
        .map(|k| (*k, has.get(k).map(|s| s.len()).unwrap_or(0) as f64 / n_src as f64))
        .collect()
}

/// p15 / p50 / p85 bands from measured windows. `opening_paragraph_words`
/// comes from source openings only (a mid-chapter window's first paragraph
/// is not an opening); with fewer than 5 openings it is unmeasured.
pub fn bands_from(p: &Profile, defs: &[Def], rows: &[&WindowRow], presence: &HashMap<&'static str, f64>) -> Vec<Band> {
    defs.iter()
        .enumerate()
        .map(|(j, d)| {
            let opening = d.key == "opening_paragraph_words";
            let mut pairs: Vec<(f64, f64)> = rows
                .iter()
                .filter(|r| !opening || r.is_opening(p))
                .map(|r| (r.values[j], r.weight))
                .collect();
            pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let unmeasured = presence.get(d.key.as_str()).is_some_and(|s| *s < MARKUP_MIN_SOURCES)
                || (opening && pairs.len() < 5);
            Band {
                key: d.key.clone(),
                label: d.label.clone(),
                unit: d.unit,
                low: round_unit(weighted_quantile(&pairs, 0.15), d.unit),
                median: round_unit(weighted_quantile(&pairs, 0.5), d.unit),
                high: round_unit(weighted_quantile(&pairs, 0.85), d.unit),
                family: family(&d.key),
                unmeasured,
            }
        })
        .collect()
}

/// The author's p15 / p50 / p85 band per metric at `window` words.
pub fn ranges(p: &Profile, window: usize, function_words: &[String]) -> Ranges {
    let rows = window_rows(p, window, function_words, false);
    let refs: Vec<&WindowRow> = rows.iter().collect();
    ranges_from(p, window, function_words, &refs)
}

/// Bands over a chosen set of the author's windows (all of them, those of
/// one narrative mode, or all but one held-out source).
pub fn ranges_from(p: &Profile, window: usize, function_words: &[String], rows: &[&WindowRow]) -> Ranges {
    let defs = defs(function_words);
    let presence = markup_presence(p);
    let bands = bands_from(p, &defs, rows, &presence);
    let mean_chunks = if rows.is_empty() {
        1
    } else {
        (rows.iter().map(|r| r.chunks.len()).sum::<usize>() as f64 / rows.len() as f64).round() as usize
    };
    Ranges {
        window_words: window,
        passages_per_window: mean_chunks.max(1),
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
    /// Markup the author's sources barely use: not judged.
    Unmeasured,
}

/// How far from the band a value may sit before it counts as drift: half
/// the band's width, never less than a floor (half an event in the window
/// for rates, 0.05 for shares, half a unit for per-item counts), so a
/// zero-width band ("the author never does this here") divides by
/// something real instead of by zero.
pub fn tolerance(band: &Band, window_words: usize) -> f64 {
    let floor = match band.unit {
        Unit::Per1k => 0.5 * 1000.0 / window_words.max(1) as f64,
        Unit::Per1kSentences => 0.5 * 1000.0 / (window_words.max(1) as f64 / 12.0).max(1.0),
        Unit::Share => 0.05,
        Unit::Count => 0.5,
        Unit::Words => (0.05 * band.median.abs()).max(0.5),
        Unit::Score => 0.05 * band.median.abs().max(0.1),
    };
    (0.5 * (band.high - band.low)).max(floor)
}

/// Judge a value against a band. A p15–p85 band holds only 70% of the
/// author's own windows, so a hard edge there would fail the author on a
/// third of their metrics. The tolerance is half the band width (about the
/// 2nd–98th percentile for a normal spread), never less than one event in
/// the window for rates, so the author's own text passes and real drift
/// does not.
pub fn status(value: f64, band: &Band, window_words: usize) -> Status {
    if band.unmeasured {
        return Status::Unmeasured;
    }
    if value >= band.low && value <= band.high {
        return Status::InRange;
    }
    let tol = tolerance(band, window_words);
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
        Unit::Per1k | Unit::Words | Unit::Count => 100.0,
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
    fn links_are_counted_but_not_measured_as_prose() {
        assert_eq!(
            strip_links("See [this essay](https://x.com/a:b_(c)/2021.html) and https://t.co/9x for more.\nhttps://x.com."),
            "See this essay and for more.\n."
        );
        assert_eq!(strip_links("No links: here (none)."), "No links: here (none).");
        let plain = "I said it twice: once here. See this essay and for more.";
        let linked = "I said it twice: once here. See [this essay](https://example.com/2021/taste:aesthetics.html) and \
                      https://en.wikipedia.org/wiki/Taste_(sociology) for more.";
        let (a, b) = (measure(&paras(plain), &[]), measure(&paras(linked), &[]));
        for key in ["colons_per_1k", "parentheses_per_1k", "digits_per_1k", "word_letters_mean", "sentence_words_mean"] {
            assert_eq!(get(&a, key), get(&b, key), "{key} moved when links were added");
        }
        assert_eq!(get(&a, "links_per_1k"), 0.0);
        assert!(get(&b, "links_per_1k") > 0.0, "links still counted");
        // A paragraph that is only a link is not a paragraph of prose.
        let with_link_para = format!("{plain}\n\nhttps://example.com/x");
        assert_eq!(get(&measure(&paras(&with_link_para), &[]), "paragraph_words_mean"), get(&a, "paragraph_words_mean"));
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
        let b = Band { key: "x".into(), label: "x".into(), unit: Unit::Share, low: 0.2, median: 0.25, high: 0.3, family: "rhythm", unmeasured: false };
        assert_eq!(status(0.25, &b, 1000), Status::InRange);
        assert_eq!(status(0.33, &b, 1000), Status::Near);
        assert_eq!(status(0.5, &b, 1000), Status::OutOfRange);
    }
}


// ── Voice distance ───────────────────────────────────────────────────────────

/// How far measured values sit from the author's medians: per metric
/// min(3, |v − median| / tolerance)², averaged within each family, then the
/// families averaged with equal weight. 0 = every metric at the author's
/// median; ~1 = a typical spread; 9 = everything far outside. `skip` leaves
/// bands out (unmeasured markup, measures that mean nothing in a window).
pub fn distance(values: &[f64], bands: &[Band], window_words: usize, skip: &dyn Fn(&Band) -> bool) -> f64 {
    let mut fam: Vec<(&'static str, f64, usize)> = Vec::new();
    for (v, b) in values.iter().zip(bands) {
        if b.unmeasured || skip(b) {
            continue;
        }
        // Fewer stock LLM phrases than the author is never off-voice.
        let gap = if b.key == "llm_tells_per_1k" { (v - b.median).max(0.0) } else { (v - b.median).abs() };
        let r = (gap / tolerance(b, window_words)).min(3.0);
        match fam.iter_mut().find(|f| f.0 == b.family) {
            Some(f) => {
                f.1 += r * r;
                f.2 += 1;
            }
            None => fam.push((b.family, r * r, 1)),
        }
    }
    if fam.is_empty() {
        return 0.0;
    }
    fam.iter().map(|f| f.1 / f.2 as f64).sum::<f64>() / fam.len() as f64
}

/// Share (weighted) of reference values above `x`, ties counted half, as
/// 0–100: the percentile score. 50 = as typical as the author's median.
pub fn percentile_above(refs: &[(f64, f64)], x: f64) -> f64 {
    let total: f64 = refs.iter().map(|r| r.1).sum();
    if total <= 0.0 {
        return 0.0;
    }
    let above: f64 = refs
        .iter()
        .map(|(d, w)| {
            if *d > x + 1e-9 {
                *w
            } else if (*d - x).abs() <= 1e-9 {
                0.5 * w
            } else {
                0.0
            }
        })
        .sum();
    100.0 * above / total
}

// ── Narrative modes (fiction) ────────────────────────────────────────────────

/// A section's narrative mode: point of view and tense of the narration.
/// Dual-narrative novels alternate modes by chapter, so a band pooled over
/// both would invite an agent to mix them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Mode {
    /// first | third
    pub pov: &'static str,
    /// present | past | mixed
    pub tense: &'static str,
}

/// Mode from narration shares: present tense at ≥ 0.6 of narration verbs,
/// past at ≤ 0.3; first person when I/me/my make ≥ 0.2 of narration
/// pronouns (a first-person narrator still says he and she a lot).
pub fn mode_of(pov_first_share: f64, present_share: f64) -> Mode {
    Mode {
        pov: if pov_first_share >= 0.2 { "first" } else { "third" },
        tense: if present_share >= 0.6 {
            "present"
        } else if present_share <= 0.3 {
            "past"
        } else {
            "mixed"
        },
    }
}

/// Narration counts of a passage: (present verbs, past verbs, first-person
/// pronouns, third-person pronouns).
fn narration_counts(paragraphs: &[String]) -> [usize; 4] {
    let mut c = [0usize; 4];
    for p in paragraphs {
        for w in text::word_tokens(&narration(p)) {
            let w = w.as_str();
            if PRESENT.contains(&w) {
                c[0] += 1;
            } else if PAST.contains(&w) {
                c[1] += 1;
            }
            if FIRST_PERSON.contains(&w) {
                c[2] += 1;
            } else if THIRD_PERSON.contains(&w) {
                c[3] += 1;
            }
        }
    }
    c
}

/// The mode of each chunk, from its whole section (chapter): sections are
/// labelled once, so a window inside a present-tense chapter is present
/// even where a page of it recalls the past.
pub fn chunk_modes(p: &Profile) -> Vec<Mode> {
    let mut by_section: HashMap<(u32, u32), [usize; 4]> = HashMap::new();
    for c in &p.chunks {
        let n = narration_counts(&c.paragraphs);
        let e = by_section.entry((c.source, c.section)).or_insert([0; 4]);
        for k in 0..4 {
            e[k] += n[k];
        }
    }
    p.chunks
        .iter()
        .map(|c| {
            let n = by_section[&(c.source, c.section)];
            mode_of(share(n[2], n[2] + n[3]), share(n[0], n[0] + n[1]))
        })
        .collect()
}

/// A window's mode: the mode of most of its chunks.
pub fn window_mode(row: &WindowRow, modes: &[Mode]) -> Mode {
    let mut counts: Vec<(Mode, usize)> = Vec::new();
    for &i in &row.chunks {
        match counts.iter_mut().find(|(m, _)| *m == modes[i]) {
            Some(e) => e.1 += 1,
            None => counts.push((modes[i], 1)),
        }
    }
    counts.into_iter().max_by_key(|e| e.1).map(|e| e.0).unwrap_or(mode_of(0.0, 0.0))
}

// ── Headline measures per register ───────────────────────────────────────────

/// The 10–13 measures a brief and a revision plan lead with for a register.
pub fn headline_keys(register: crate::engine::profile::Register) -> &'static [&'static str] {
    use crate::engine::profile::Register as R;
    match register {
        R::Fiction => &[
            "sentence_words_median", "short_sentence_share", "paragraph_words_mean",
            "one_sentence_paragraph_share", "dialogue_word_share", "dialogue_tags_per_1k",
            "pov_first_share", "present_tense_share", "contractions_per_1k", "questions_per_1k",
            "em_dashes_per_1k", "similes_per_1k",
        ],
        R::Nonfiction => &[
            "sentence_words_median", "long_sentence_share", "paragraph_words_mean",
            "one_sentence_paragraph_share", "first_person_per_1k", "you_per_1k", "we_per_1k",
            "questions_per_1k", "digits_per_1k", "contractions_per_1k", "semicolons_per_1k",
            "em_dashes_per_1k", "colons_per_1k",
        ],
        R::Blog => &[
            "sentence_words_median", "paragraph_words_mean", "one_sentence_paragraph_share",
            "opening_paragraph_words", "headings_per_1k", "list_line_share", "bold_per_1k",
            "links_per_1k", "you_per_1k", "first_person_per_1k", "questions_per_1k",
            "contractions_per_1k", "digits_per_1k",
        ],
        R::Social => &[
            "item_words", "item_lines", "first_line_words", "emoji_per_item", "hashtags_per_item",
            "mentions_per_item", "links_per_item", "lowercase_start_share", "ends_with_punct_share",
            "exclaims_per_item", "questions_per_item", "all_caps_words_per_item",
        ],
        R::Email | R::Message => &[
            "item_words", "item_lines", "has_greeting_share", "has_signoff_share",
            "exclaims_per_item", "questions_per_item", "emoji_per_item", "lowercase_start_share",
            "ends_with_punct_share", "links_per_item",
        ],
    }
}

// ── Short-form items ─────────────────────────────────────────────────────────

/// Per-item measures: key, label, unit, advice to raise, advice to lower.
const ITEM_CORE: &[Core] = &[
    ("item_words", "words per item", Unit::Words,
     "write it longer", "cut it shorter"),
    ("item_chars", "characters per item (X-weighted: link 23, emoji 2)", Unit::Count,
     "write it longer", "cut it shorter"),
    ("item_lines", "lines per item", Unit::Count,
     "break it onto more lines", "put it on fewer lines"),
    ("first_line_words", "words in the first line (the hook)", Unit::Words,
     "let the first line run longer", "shorten the first line to a hook"),
    ("emoji_per_item", "emoji per item", Unit::Count,
     "add an emoji where the author would", "cut the emoji"),
    ("hashtags_per_item", "hashtags per item", Unit::Count,
     "add a hashtag", "cut the hashtags"),
    ("mentions_per_item", "@mentions per item", Unit::Count,
     "mention the person or account", "cut the @mentions"),
    ("links_per_item", "links per item", Unit::Count,
     "add the link", "drop the link"),
    ("exclaims_per_item", "exclamation marks per item", Unit::Count,
     "allow an exclamation", "cut the exclamation marks"),
    ("questions_per_item", "questions per item", Unit::Count,
     "ask a question", "turn the questions into statements"),
    ("all_caps_words_per_item", "emphasis caps per item (words the author also writes in lowercase)", Unit::Count,
     "stress a word in CAPS", "drop the CAPS emphasis"),
    ("llm_tells_per_item", "stock LLM words and phrases per item", Unit::Count,
     "keep to the author's own words", "cut the stock LLM phrasing"),
    ("lowercase_start_share", "starts in lowercase", Unit::Share,
     "start in lowercase", "start with a capital letter"),
    ("ends_with_punct_share", "ends with . ? ! or …", Unit::Share,
     "end on a full stop or other mark", "leave the end unpunctuated"),
    ("has_greeting_share", "opens with a greeting", Unit::Share,
     "open with the author's greeting", "drop the greeting"),
    ("has_signoff_share", "ends with a sign-off", Unit::Share,
     "close with the author's sign-off", "drop the sign-off"),
];

pub fn item_defs() -> Vec<Def> {
    ITEM_CORE
        .iter()
        .map(|(k, l, u, r, lo)| Def { key: k.to_string(), label: l.to_string(), unit: *u, raise: r, lower: lo })
        .collect()
}

pub fn item_index(key: &str) -> Option<usize> {
    ITEM_CORE.iter().position(|c| c.0 == key)
}

fn is_emoji_modifier(c: char) -> bool {
    c == '\u{FE0F}' || c == '\u{200D}' || (0x1F3FB..=0x1F3FF).contains(&(c as u32))
}

/// Emoji (pictographs, symbols, dingbats, flags), not skin-tone modifiers.
pub fn is_emoji(c: char) -> bool {
    let u = c as u32;
    if (0x1F3FB..=0x1F3FF).contains(&u) {
        return false;
    }
    matches!(u,
        0x1F000..=0x1FAFF | 0x2600..=0x27BF | 0x2B50 | 0x2B55 | 0x231A | 0x231B | 0x23E9..=0x23FA
        | 0x2934 | 0x2935 | 0x3030 | 0x303D | 0x3297 | 0x3299)
}

/// Emoji in a text, one per displayed glyph (ZWJ sequences and flag pairs
/// count once).
pub fn emojis_in(t: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut joining = false;
    let mut flag_half = false;
    for c in t.chars() {
        if c == '\u{200D}' {
            joining = true;
            continue;
        }
        if is_emoji_modifier(c) {
            if let Some(l) = out.last_mut() {
                l.push(c);
            }
            continue;
        }
        if is_emoji(c) {
            let flag = (0x1F1E6..=0x1F1FF).contains(&(c as u32));
            if (joining || (flag && flag_half)) && !out.is_empty() {
                let l = out.last_mut().unwrap();
                if joining {
                    l.push('\u{200D}');
                }
                l.push(c);
                flag_half = false;
            } else {
                out.push(c.to_string());
                flag_half = flag;
            }
        } else {
            flag_half = false;
        }
        joining = false;
    }
    out
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x1100..=0x115F | 0x2E80..=0xA4CF | 0xAC00..=0xD7A3 | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFF60 | 0xFFE0..=0xFFE6)
}

fn is_link(tok: &str) -> bool {
    let bare = tok.trim_start_matches(['(', '[', '<', '"', '\u{201C}']);
    bare.starts_with("http://") || bare.starts_with("https://") || bare.starts_with("www.") || tok.contains("](http")
}

fn is_hashtag(tok: &str) -> bool {
    let bare = tok.trim_start_matches(['(', '"', '\u{201C}']);
    bare.strip_prefix('#').is_some_and(|r| r.chars().next().is_some_and(|c| c.is_alphanumeric()))
}

fn is_mention(tok: &str) -> bool {
    let bare = tok.trim_start_matches(['(', '"', '\u{201C}', '.']);
    bare.strip_prefix('@').is_some_and(|r| r.chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_'))
}

/// Length as X counts it: a link is 23, an emoji or CJK character 2.
pub fn x_weighted_chars(t: &str) -> usize {
    let mut n = 0;
    for tok in t.split_inclusive(char::is_whitespace) {
        let core = tok.trim_end();
        if is_link(core) && !core.contains("](") {
            n += 23 + tok.chars().count() - core.chars().count();
            continue;
        }
        for c in tok.chars() {
            n += if is_emoji_modifier(c) {
                0
            } else if is_emoji(c) || is_cjk(c) {
                2
            } else {
                1
            };
        }
    }
    n
}

const GREETINGS: [&str; 12] = [
    "hi", "hey", "hello", "dear", "morning", "gm", "yo", "hiya", "heya", "greetings", "howdy", "hullo",
];
const SIGNOFFS: [&str; 26] = [
    "best", "cheers", "thanks", "thank you", "thx", "regards", "kind regards", "best regards",
    "warm regards", "warmly", "love", "talk soon", "speak soon", "all the best", "sincerely",
    "yours", "take care", "x", "xx", "xo", "ttyl", "many thanks", "thanks again", "best wishes",
    "with thanks", "later",
];

fn signoff_form(line: &str) -> Option<String> {
    let l = line.trim().trim_end_matches(['.', ',', '!', '-', '\u{2014}']).trim().to_lowercase();
    SIGNOFFS.contains(&l.as_str()).then_some(l)
}

/// One item's measures and its categorical habits.
#[derive(Debug, Clone)]
pub struct ItemMeasure {
    /// Values in `item_defs` order.
    pub values: Vec<f64>,
    /// none | . | ? | ! | … | emoji | link
    pub ending: &'static str,
    /// single_line (one line, one sentence) | paragraph (one line, several
    /// sentences) | one_sentence_per_line (several lines, no blank lines) |
    /// blank_line_blocks (blocks separated by blank lines)
    pub layout: &'static str,
    /// First two words, lowercased.
    pub opener: Vec<String>,
    pub emojis: Vec<String>,
    pub hashtags: Vec<String>,
    pub greeting: Option<String>,
    pub signoff: Option<String>,
    pub em_dashes: usize,
    pub semicolons: usize,
    pub capital_start: bool,
}

impl ItemMeasure {
    pub fn get(&self, key: &str) -> f64 {
        item_index(key).map(|i| self.values[i]).unwrap_or(0.0)
    }
}

/// Words the author writes in lowercase somewhere: an all-caps word is
/// emphasis only if it is also an ordinary word for them (AI, GPT and CEO
/// are acronyms; NOT and NEVER are shouting).
pub fn lowercase_vocab(p: &Profile) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for c in &p.chunks {
        for para in &c.paragraphs {
            for w in text::word_tokens_cased(para) {
                if w.chars().any(|ch| ch.is_alphabetic()) && !w.chars().any(|ch| ch.is_uppercase()) {
                    out.insert(w);
                }
            }
        }
    }
    out
}

fn char_runs(t: &str, ch: char) -> usize {
    let mut n = 0;
    let mut prev = None;
    for c in t.chars() {
        if c == ch && prev != Some(ch) {
            n += 1;
        }
        prev = Some(c);
    }
    n
}

fn item_ending(toks: &[&str]) -> &'static str {
    let mut end = toks.len();
    let mut link_last = false;
    while end > 0 {
        let last = toks[end - 1];
        if is_link(last) {
            link_last = true;
            end -= 1;
        } else if is_hashtag(last) || is_mention(last) {
            end -= 1;
        } else {
            break;
        }
    }
    let rest = toks[..end].join(" ");
    let rest = rest.trim_end_matches(['"', '\'', '\u{201D}', '\u{2019}', ')', '*', '_']);
    let last = rest.chars().rev().find(|c| !c.is_whitespace() && !is_emoji_modifier(*c));
    match last {
        Some(c) if is_emoji(c) => "emoji",
        Some('.') if rest.ends_with("...") => "\u{2026}",
        Some('.') => ".",
        Some('?') => "?",
        Some('!') => "!",
        Some('\u{2026}') => "\u{2026}",
        _ if link_last => "link",
        _ => "none",
    }
}

/// Stock LLM words and phrases in a token stream, each place once: "a
/// testament to" and the "testament" inside it are one phrase.
pub fn stock_phrases(tokens: &[String]) -> Vec<crate::engine::voice::TellHit> {
    let mut hits = crate::engine::voice::find_tells(tokens);
    hits.sort_by(|a, b| a.at.cmp(&b.at).then(b.len.cmp(&a.len)));
    let mut out: Vec<crate::engine::voice::TellHit> = Vec::new();
    for h in hits {
        if out.last().is_none_or(|l| h.at >= l.at + l.len) {
            out.push(h);
        }
    }
    out
}

/// Measure one post, email or message, verbatim (line breaks kept).
pub fn measure_item(t: &str, lower_vocab: &std::collections::HashSet<String>) -> ItemMeasure {
    let lines: Vec<&str> = t.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    let blocks = t.split("\n\n").filter(|b| !b.trim().is_empty()).count().max(1);
    let toks: Vec<&str> = t.split_whitespace().collect();
    let links = toks.iter().filter(|w| is_link(w)).count();
    let hashtags: Vec<String> = toks
        .iter()
        .filter(|w| is_hashtag(w))
        .map(|w| {
            w.trim_start_matches(['(', '"', '\u{201C}'])
                .trim_end_matches(|c: char| !c.is_alphanumeric() && c != '_')
                .to_lowercase()
        })
        .collect();
    let mentions = toks.iter().filter(|w| is_mention(w)).count();
    let emojis = emojis_in(t);
    let words: Vec<String> = text::word_tokens(t).into_iter().filter(|w| !text::is_placeholder(w)).collect();
    let first_line_words = lines
        .first()
        .map(|l| text::word_tokens(l).into_iter().filter(|w| !text::is_placeholder(w)).count())
        .unwrap_or(0);
    let caps = text::word_tokens_cased(&text::normalize_web(t))
        .into_iter()
        .filter(|w| {
            w.chars().filter(|c| c.is_alphabetic()).count() >= 2
                && w.chars().all(|c| !c.is_alphabetic() || c.is_uppercase())
                && !text::is_placeholder(w)
                && lower_vocab.contains(&w.to_lowercase())
        })
        .count();
    let first_letter = toks
        .iter()
        .filter(|w| !is_mention(w) && !is_link(w))
        .flat_map(|w| w.chars())
        .find(|c| c.is_alphanumeric()); // "22,000 new papers" starts with a digit, not lowercase
    let lowercase_start = first_letter.is_some_and(char::is_lowercase);
    let capital_start = first_letter.is_some_and(char::is_uppercase);
    let ending = item_ending(&toks);
    let layout = if lines.len() <= 1 {
        if text::sentences_for(t, true).len() >= 2 { "paragraph" } else { "single_line" }
    } else if blocks >= 2 {
        "blank_line_blocks"
    } else {
        "one_sentence_per_line"
    };
    let greeting = lines.first().and_then(|l| {
        let w = text::word_tokens(l);
        if w.is_empty() || w.len() > 6 {
            return None;
        }
        let two = w.get(..2).map(|x| x.join(" "));
        if GREETINGS.contains(&w[0].as_str()) {
            Some(w[0].clone())
        } else if matches!(two.as_deref(), Some("good morning" | "good afternoon" | "good evening")) {
            two
        } else {
            None
        }
    });
    let signoff = match lines.len() {
        0 => None,
        n => signoff_form(lines[n - 1]).or_else(|| {
            let name_like = text::count_words(lines[n - 1]) <= 2
                && !lines[n - 1].ends_with(['.', '?', '!'])
                && lines[n - 1].chars().next().is_some_and(char::is_uppercase);
            if n >= 2 && name_like { signoff_form(lines[n - 2]) } else { None }
        }),
    };
    let ends_punct = matches!(ending, "." | "?" | "!" | "\u{2026}");
    let values = vec![
        words.len() as f64,
        x_weighted_chars(t) as f64,
        lines.len() as f64,
        first_line_words as f64,
        emojis.len() as f64,
        hashtags.len() as f64,
        mentions as f64,
        links as f64,
        char_runs(t, '!') as f64,
        char_runs(t, '?') as f64,
        caps as f64,
        stock_phrases(&words).len() as f64,
        if lowercase_start { 1.0 } else { 0.0 },
        if ends_punct { 1.0 } else { 0.0 },
        if greeting.is_some() { 1.0 } else { 0.0 },
        if signoff.is_some() { 1.0 } else { 0.0 },
    ];
    debug_assert_eq!(values.len(), ITEM_CORE.len());
    ItemMeasure {
        values,
        ending,
        layout,
        opener: words.iter().take(2).cloned().collect(),
        emojis,
        hashtags,
        greeting,
        signoff,
        em_dashes: t.matches('\u{2014}').count() + t.matches("--").count(),
        semicolons: t.matches(';').count(),
        capital_start,
    }
}

/// A reply without the run of @handles X puts in front of it: those are
/// addressing, not writing, and would make every reply "start with a
/// mention" and skew its casing and hook length.
pub fn strip_leading_handles(t: &str) -> &str {
    let mut rest = t.trim_start();
    while rest.starts_with('@') {
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        rest = rest[end..].trim_start();
    }
    if rest.is_empty() { t } else { rest }
}

/// The author's items of `kind`, measured, with their weights. Falls back
/// to every item when fewer than 20 are of that kind (third value).
pub fn author_items(p: &Profile, kind: crate::engine::profile::ItemKind) -> (Vec<ItemMeasure>, Vec<f64>, bool) {
    let vocab = lowercase_vocab(p);
    let of_kind: Vec<usize> = (0..p.chunks.len())
        .filter(|&i| p.chunks[i].meta.as_ref().map(|m| m.kind).unwrap_or_default() == kind)
        .collect();
    let (idx, fallback): (Vec<usize>, bool) = if of_kind.len() >= 20 || of_kind.len() == p.chunks.len() {
        (of_kind, false)
    } else {
        ((0..p.chunks.len()).collect(), true)
    };
    let measured = idx
        .iter()
        .map(|&i| {
            let t = p.chunks[i].text();
            let reply = p.chunks[i].meta.as_ref().is_some_and(|m| m.kind == crate::engine::profile::ItemKind::Reply);
            measure_item(if reply { strip_leading_handles(&t) } else { &t }, &vocab)
        })
        .collect();
    let weights = idx.iter().map(|&i| p.chunks[i].weight as f64).collect();
    (measured, weights, fallback)
}

/// p15 / p50 / p85 of per-item measures over the author's items of `kind`
/// (every item when fewer than 20 are of that kind). Flags (shares) get the
/// author's share as low = median = high. Long-form profiles have no items:
/// empty bands.
#[allow(dead_code)] // cross-lane contract: the brief and `examples` read it
pub fn item_ranges(p: &Profile, kind: crate::engine::profile::ItemKind) -> Ranges {
    if !p.register.is_short_form() {
        return Ranges { window_words: 0, passages_per_window: 1, windows: 0, directional: true, bands: Vec::new(), defs: Vec::new() };
    }
    let (items, weights, fallback) = author_items(p, kind);
    item_ranges_of(&items, &weights, fallback)
}

pub fn item_ranges_of(items: &[ItemMeasure], weights: &[f64], fallback: bool) -> Ranges {
    let defs = item_defs();
    let bands = defs
        .iter()
        .enumerate()
        .map(|(j, d)| {
            let mut pairs: Vec<(f64, f64)> = items.iter().zip(weights).map(|(m, w)| (m.values[j], *w)).collect();
            pairs.sort_by(|a, b| a.0.total_cmp(&b.0));
            let (low, median, high) = if d.unit == Unit::Share {
                let tw: f64 = pairs.iter().map(|p| p.1).sum::<f64>().max(1e-9);
                let s = pairs.iter().map(|p| p.0 * p.1).sum::<f64>() / tw;
                (s, s, s)
            } else {
                (weighted_quantile(&pairs, 0.15), weighted_quantile(&pairs, 0.5), weighted_quantile(&pairs, 0.85))
            };
            Band {
                key: d.key.clone(),
                label: d.label.clone(),
                unit: d.unit,
                low: round_unit(low, d.unit),
                median: round_unit(median, d.unit),
                high: round_unit(high, d.unit),
                family: "items",
                unmeasured: false,
            }
        })
        .collect();
    Ranges {
        window_words: 0,
        passages_per_window: 1,
        windows: items.len(),
        directional: items.len() < 20 || fallback,
        bands,
        defs,
    }
}

/// A habit the author (almost) never shows: "hashtags: 2 of 640 posts".
#[derive(Debug, Clone, Default, Serialize)]
pub struct NeverHabit {
    pub key: String,
    pub label: String,
    pub count: usize,
    pub of: usize,
}

/// Per-item habits of a short-form profile for one kind of item: shares of
/// each form, most common first.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ItemHabits {
    pub items: usize,
    pub top_emojis: Vec<(String, f64)>,
    pub top_hashtags: Vec<(String, f64)>,
    /// How items end: none, ".", "?", "!", "…", emoji, link.
    pub endings: Vec<(String, f64)>,
    /// First one or two words of items.
    pub openers: Vec<(String, f64)>,
    /// single_line, one_sentence_per_line, blank_line_blocks, paragraph.
    pub layouts: Vec<(String, f64)>,
    pub greetings: Vec<(String, f64)>,
    pub signoffs: Vec<(String, f64)>,
    pub never: Vec<NeverHabit>,
}

/// Habits checked by the never-list: key, label, does the item show it.
pub fn never_flags(m: &ItemMeasure) -> [(&'static str, &'static str, bool); 11] {
    [
        ("hashtags", "hashtags", m.get("hashtags_per_item") > 0.0),
        ("emoji", "emoji", m.get("emoji_per_item") > 0.0),
        ("links", "links", m.get("links_per_item") > 0.0),
        ("exclamations", "exclamation marks", m.get("exclaims_per_item") > 0.0),
        ("em_dashes", "em dashes", m.em_dashes > 0),
        ("semicolons", "semicolons", m.semicolons > 0),
        ("emphasis_caps", "CAPS emphasis", m.get("all_caps_words_per_item") > 0.0),
        ("lowercase_start", "a lowercase first letter", m.get("lowercase_start_share") > 0.0),
        ("capital_start", "a capitalised first letter", m.capital_start),
        ("line_breaks", "line breaks", m.get("item_lines") > 1.0),
        ("stock_phrases", "three or more stock LLM phrases in one item", m.get("llm_tells_per_item") >= 3.0),
    ]
}

/// Minimum items before an absent habit becomes a hard rule.
pub const NEVER_MIN_ITEMS: usize = 100;
/// At most this share of items may show a habit for it to be "never".
pub const NEVER_MAX_SHARE: f64 = 0.03;

fn top_shares(counts: HashMap<String, f64>, total: f64, n: usize) -> Vec<(String, f64)> {
    let mut v: Vec<(String, f64)> = counts.into_iter().map(|(k, c)| (k, c / total.max(1e-9))).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(n);
    v.into_iter().map(|(k, s)| (k, (s * 1000.0).round() / 1000.0)).collect()
}

pub fn habits_of(items: &[ItemMeasure], weights: &[f64]) -> ItemHabits {
    let total: f64 = weights.iter().sum();
    let mut emo: HashMap<String, f64> = HashMap::new();
    let mut tags: HashMap<String, f64> = HashMap::new();
    let mut ends: HashMap<String, f64> = HashMap::new();
    let mut lays: HashMap<String, f64> = HashMap::new();
    let mut greet: HashMap<String, f64> = HashMap::new();
    let mut sign: HashMap<String, f64> = HashMap::new();
    let mut two: HashMap<String, f64> = HashMap::new();
    for (m, w) in items.iter().zip(weights) {
        let uniq: std::collections::BTreeSet<&String> = m.emojis.iter().collect();
        for e in uniq {
            *emo.entry(e.clone()).or_insert(0.0) += w;
        }
        let uniq: std::collections::BTreeSet<&String> = m.hashtags.iter().collect();
        for t in uniq {
            *tags.entry(t.clone()).or_insert(0.0) += w;
        }
        *ends.entry(m.ending.to_string()).or_insert(0.0) += w;
        *lays.entry(m.layout.to_string()).or_insert(0.0) += w;
        if let Some(g) = &m.greeting {
            *greet.entry(g.clone()).or_insert(0.0) += w;
        }
        if let Some(s) = &m.signoff {
            *sign.entry(s.clone()).or_insert(0.0) += w;
        }
        if m.opener.len() == 2 {
            *two.entry(m.opener.join(" ")).or_insert(0.0) += w;
        }
    }
    // Openers: a two-word opener when it recurs (3+ items), else the first word.
    let mut openers: HashMap<String, f64> = HashMap::new();
    for (m, w) in items.iter().zip(weights) {
        let Some(first) = m.opener.first() else { continue };
        let pair = m.opener.join(" ");
        let key = if m.opener.len() == 2 && two.get(&pair).copied().unwrap_or(0.0) >= 3.0 { pair } else { first.clone() };
        *openers.entry(key).or_insert(0.0) += w;
    }
    let never = if items.len() >= NEVER_MIN_ITEMS {
        let mut shown = [0usize; 11];
        for m in items {
            for (i, f) in never_flags(m).iter().enumerate() {
                if f.2 {
                    shown[i] += 1;
                }
            }
        }
        never_flags(&items[0])
            .iter()
            .zip(shown)
            .filter(|(_, c)| (*c as f64) <= NEVER_MAX_SHARE * items.len() as f64)
            .map(|(f, c)| NeverHabit { key: f.0.to_string(), label: f.1.to_string(), count: c, of: items.len() })
            .collect()
    } else {
        Vec::new()
    };
    let mut openers = top_shares(openers, total, 12);
    openers.retain(|(_, s)| *s >= 0.02);
    ItemHabits {
        items: items.len(),
        top_emojis: top_shares(emo, total, 10),
        top_hashtags: top_shares(tags, total, 10),
        endings: top_shares(ends, total, 8),
        openers,
        layouts: top_shares(lays, total, 4),
        greetings: top_shares(greet, total, 6),
        signoffs: top_shares(sign, total, 6),
        never,
    }
}

/// Habits of the author's items of `kind` (every item when fewer than 20
/// are of that kind). Long-form profiles have none. Cross-lane contract:
/// the brief reads it (check uses `habits_of` on items it already measured).
#[allow(dead_code)]
pub fn item_habits(p: &Profile, kind: crate::engine::profile::ItemKind) -> ItemHabits {
    if !p.register.is_short_form() {
        return ItemHabits::default();
    }
    let (items, weights, _) = author_items(p, kind);
    habits_of(&items, &weights)
}

// ── Item typicality (surprisal) ──────────────────────────────────────────────

/// Feature names in `ItemModel::bins` order, for habit rows.
pub const ITEM_FEATURES: [&str; 15] = [
    "item_words", "first_line_words", "emoji_per_item", "hashtags_per_item",
    "mentions_per_item", "links_per_item", "exclaims_per_item", "questions_per_item",
    "all_caps_words_per_item", "lowercase_start", "ending", "layout", "llm_tells_per_item",
    "greeting", "signoff",
];

const ENDINGS: [&str; 7] = ["none", ".", "?", "!", "\u{2026}", "emoji", "link"];
const LAYOUTS: [&str; 4] = ["single_line", "paragraph", "one_sentence_per_line", "blank_line_blocks"];

/// Binned per-item features for typicality: counts as 0/1/2/3+, flags,
/// categorical endings and layouts, lengths by the author's own deciles.
/// Bands can't score one post (a habit the author never shows has a 0–0
/// band), so a post is scored by how surprising its bins are to the author:
/// Σ −ln P_author(bin), add-1 smoothed.
pub struct ItemModel {
    word_edges: Vec<f64>,
    line_edges: Vec<f64>,
    /// counts[feature][bin], weighted.
    counts: Vec<Vec<f64>>,
    total: f64,
    with_greetings: bool,
    /// Each author item's surprisal with itself left out, and its weight.
    pub reference: Vec<(f64, f64)>,
}

fn edges(vals: &mut [f64], n: usize) -> Vec<f64> {
    vals.sort_by(|a, b| a.total_cmp(b));
    let mut e: Vec<f64> = (1..n).map(|i| quantile(vals, i as f64 / n as f64)).collect();
    e.dedup();
    e
}

fn bin_of(v: f64, edges: &[f64]) -> usize {
    edges.iter().filter(|e| v > **e).count()
}

impl ItemModel {
    pub fn new(items: &[ItemMeasure], weights: &[f64], with_greetings: bool) -> ItemModel {
        let mut words: Vec<f64> = items.iter().map(|m| m.get("item_words")).collect();
        // The hook only counts for multi-line items: a one-liner's first line is its length.
        let mut first: Vec<f64> = items.iter().filter(|m| m.get("item_lines") > 1.0).map(|m| m.get("first_line_words")).collect();
        let mut model = ItemModel {
            word_edges: edges(&mut words, 10),
            line_edges: edges(&mut first, 5),
            counts: Vec::new(),
            total: 0.0,
            with_greetings,
            reference: Vec::new(),
        };
        model.counts = model.sizes().iter().map(|&n| vec![0.0; n]).collect();
        let binned: Vec<Vec<usize>> = items.iter().map(|m| model.bins(m)).collect();
        for (b, w) in binned.iter().zip(weights) {
            for (f, &bin) in b.iter().enumerate() {
                model.counts[f][bin] += w;
            }
            model.total += w;
        }
        model.reference = binned.iter().zip(weights).map(|(b, w)| (model.surprisal_of(b, *w), *w)).collect();
        model
    }

    fn sizes(&self) -> Vec<usize> {
        let mut s = vec![
            self.word_edges.len() + 1,
            self.line_edges.len() + 2,
            4, 4, 4, 3, 4, 3, 3, 2,
            ENDINGS.len(),
            LAYOUTS.len(),
            4,
        ];
        if self.with_greetings {
            s.extend([2, 2]);
        }
        s
    }

    pub fn features(&self) -> usize {
        self.counts.len()
    }

    pub fn bins(&self, m: &ItemMeasure) -> Vec<usize> {
        let c = |key: &str, cap: usize| (m.get(key).max(0.0) as usize).min(cap);
        // Marks other than the final one: the ending feature already holds
        // a closing "!" or "?", and counting it twice would make one
        // exclamation cost a post twice.
        let interior = |key: &str, mark: &str, cap: usize| {
            let n = m.get(key).max(0.0) as usize;
            n.saturating_sub(usize::from(m.ending == mark)).min(cap)
        };
        let mut b = vec![
            bin_of(m.get("item_words"), &self.word_edges),
            if m.get("item_lines") > 1.0 { 1 + bin_of(m.get("first_line_words"), &self.line_edges) } else { 0 },
            c("emoji_per_item", 3),
            c("hashtags_per_item", 3),
            c("mentions_per_item", 3),
            c("links_per_item", 2),
            interior("exclaims_per_item", "!", 3),
            interior("questions_per_item", "?", 2),
            c("all_caps_words_per_item", 2),
            c("lowercase_start_share", 1),
            ENDINGS.iter().position(|e| *e == m.ending).unwrap_or(0),
            LAYOUTS.iter().position(|l| *l == m.layout).unwrap_or(0),
            // A post rarely repeats a stock phrase, so one is already a
            // habit: the author's own share of posts with one decides
            // what it costs.
            c("llm_tells_per_item", 3),
        ];
        if self.with_greetings {
            b.push(c("has_greeting_share", 1));
            b.push(c("has_signoff_share", 1));
        }
        b
    }

    /// Surprisal of binned features; `leave_out` removes one item's own
    /// weight from the counts (leave-one-out for the author's items).
    fn surprisal_of(&self, bins: &[usize], leave_out: f64) -> f64 {
        bins.iter()
            .enumerate()
            .map(|(f, &bin)| {
                let k = self.counts[f].len() as f64;
                let c = (self.counts[f][bin] - leave_out).max(0.0);
                -((c + 1.0) / (self.total - leave_out + k)).ln()
            })
            .sum()
    }

    pub fn surprisal(&self, m: &ItemMeasure) -> f64 {
        self.surprisal_of(&self.bins(m), 0.0)
    }

    /// Share of the author's items (0–100) at least as surprising as `m`.
    pub fn score(&self, m: &ItemMeasure) -> f64 {
        percentile_above(&self.reference, self.surprisal(m))
    }

    /// The author's share of items in a feature's first bin (none, for counts).
    pub fn zero_share(&self, feature: usize) -> f64 {
        self.counts.get(feature).and_then(|c| c.first()).copied().unwrap_or(0.0) / self.total.max(1e-9)
    }

    /// The author's share of items in the same bin as `m`, per feature.
    pub fn bin_shares(&self, m: &ItemMeasure) -> Vec<f64> {
        self.bins(m)
            .iter()
            .enumerate()
            .map(|(f, &b)| self.counts[f][b] / self.total.max(1e-9))
            .collect()
    }
}
