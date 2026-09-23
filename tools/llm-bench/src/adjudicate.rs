//! Adjudication of one corrected transcript, in pure code.
//!
//! A test case is `input` (what the recognizer heard) and `expected` (what the speaker said). Where the two differ is
//! where a model had to correct: the *sites*. Nothing else is declared by hand.
//!
//!   dictionary site   `expected` carries a term of the user dictionary there. It passes iff that term stands at that
//!                     place in the output, in any casing and with any suffix. Settled by code, never by a model.
//!   semantic site     every other difference. Code settles what is exact by definition (written as expected, left as
//!                     heard, a casing variant of either); anything else becomes a question for the judge.
//!   clean             what the model changed outside the sites. Spacing, sentence punctuation and casing are neutral
//!                     by code; any other change becomes a question, and only then is the output as a whole gated.
//!
//! `Reference::analyze` finds all of this and returns the questions still open. `score` turns an analysis plus whatever
//! answers exist into credits. A question without an answer leaves its item *unjudged*: reported, never scored as 0.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// An actual edit may reach this many characters past a site and still be the model's answer to that site.
const SLACK: usize = 4;
/// Judge answers below this confidence go on the review list.
pub const REVIEW_BELOW: f64 = 0.4;
/// Credit per judged level of a site: not recovered / right word, imperfect form / recovered.
pub const SITE_CREDIT: [f64; 3] = [0.0, 0.5, 1.0];
/// What an output keeps of its clean credit when every cleanup site is still in it: the app prompt asks for the
/// fillers to go, so leaving them all in costs half of `clean`, taking them all out costs nothing.
pub const FILLERS_KEPT_CREDIT: f64 = 0.5;
/// Credit per judged kind of an unrequested change.
pub const EDIT_CREDIT: [(&str, f64); 5] = [
    ("fixes_error", 1.0),
    ("neutral", 1.0),
    ("rephrases", 0.7),
    ("damages", 0.0),
    ("commentary", 0.0),
];
/// What the gate can call an output. Only `transcript` keeps its clean credit; `not_transcript` also voids the sites.
pub const GATE_KINDS: [&str; 4] = ["transcript", "incomplete", "extended", "not_transcript"];
/// Punctuation that separates clauses and sentences. Changing it (outside a number) does not change a word.
const SENTENCE_PUNCT: [char; 14] = ['，', '。', '、', '；', '：', '？', '！', ',', '.', ';', ':', '?', '!', '-'];

// ── Text primitives ─────────────────────────────────────────────────────────

fn is_latin(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

fn folded(s: &str) -> String {
    s.chars().map(fold).collect()
}

fn string(chars: &[char]) -> String {
    chars.iter().collect()
}

/// `s` without spacing and sentence punctuation. A mark between two digits (1.5, 10:30) is content and stays.
fn words_only(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    (0..c.len())
        .filter(|&k| {
            let in_number = k > 0 && k + 1 < c.len() && c[k - 1].is_ascii_digit() && c[k + 1].is_ascii_digit();
            !(c[k].is_whitespace() || (SENTENCE_PUNCT.contains(&c[k]) && !in_number))
        })
        .map(|k| c[k])
        .collect()
}

/// `s` without spacing and sentence punctuation at its two ends.
fn core(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || SENTENCE_PUNCT.contains(&c))
}

/// Whether every character of `part` occurs in `whole`, in order.
fn subsequence(part: &str, whole: &str) -> bool {
    let mut rest = whole.chars();
    part.chars().all(|c| rest.by_ref().any(|w| w == c))
}

/// Whether an edit belongs to the window `[l, r)`. Text inserted between two sentences continues the first one.
fn within(x: &Edit, l: usize, r: usize) -> bool {
    if x.a0 < x.a1 {
        l <= x.a0 && x.a1 <= r
    } else {
        (l < x.a0 || l == 0) && x.a0 <= r
    }
}

/// Half-open ranges that share text. An empty range (an insertion point) also touches what it borders.
fn touches(x0: usize, x1: usize, s: usize, e: usize) -> bool {
    if x0 == x1 || s == e {
        x0 <= e && x1 >= s
    } else {
        x0 < e && x1 > s
    }
}

/// Occurrences of `term` as a whole word: a latin end of the term does not run on into a latin neighbour.
fn find_term(text: &[char], term: &[char], exact_case: bool) -> Vec<(usize, usize)> {
    let n = term.len();
    if n == 0 || n > text.len() {
        return Vec::new();
    }
    let same = |x: char, y: char| if exact_case { x == y } else { fold(x) == fold(y) };
    (0..=text.len() - n)
        .filter(|&k| {
            (0..n).all(|d| same(text[k + d], term[d]))
                && !(is_latin(term[0]) && k > 0 && is_latin(text[k - 1]))
                && !(is_latin(term[n - 1]) && k + n < text.len() && is_latin(text[k + n]))
        })
        .map(|k| (k, k + n))
        .collect()
}

fn has_term(text: &str, term: &str, exact_case: bool) -> bool {
    let (text, term): (Vec<char>, Vec<char>) = (text.chars().collect(), term.chars().collect());
    !find_term(&text, &term, exact_case).is_empty()
}

/// The dictionary as the app sees it: one term per line.
pub fn dictionary_terms(dictionary: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for line in dictionary.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if !terms.iter().any(|t| t == line) {
            terms.push(line.to_string());
        }
    }
    terms
}

// ── Diff ────────────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Latin,
    Soft,
    Other,
}

/// `[A-Za-z0-9_]+ | \s+ | .` — a latin word, a run of spacing, or one character.
struct Tokens {
    start: Vec<usize>,
    kind: Vec<Kind>,
    id: Vec<u32>,
}

impl Tokens {
    fn new(text: &[char], ids: &mut HashMap<String, u32>) -> Self {
        let (mut start, mut kind, mut id) = (Vec::new(), Vec::new(), Vec::new());
        let mut i = 0;
        while i < text.len() {
            let from = i;
            let k = if is_latin(text[i]) {
                while i < text.len() && is_latin(text[i]) {
                    i += 1;
                }
                Kind::Latin
            } else if text[i].is_whitespace() {
                while i < text.len() && text[i].is_whitespace() {
                    i += 1;
                }
                Kind::Soft
            } else {
                i += 1;
                if matches!(text[from], '.' | '-' | '/') {
                    Kind::Soft
                } else {
                    Kind::Other
                }
            };
            let next = ids.len() as u32;
            id.push(*ids.entry(string(&text[from..i])).or_insert(next));
            start.push(from);
            kind.push(k);
        }
        start.push(text.len());
        Tokens { start, kind, id }
    }

    fn len(&self) -> usize {
        self.kind.len()
    }

    fn latin(&self, k: usize) -> bool {
        k < self.len() && self.kind[k] == Kind::Latin
    }
}

/// Python's `difflib.SequenceMatcher(autojunk=False).find_longest_match`, tie-breaks included.
fn longest_match(
    a: &[u32],
    b2j: &HashMap<u32, Vec<usize>>,
    (alo, ahi, blo, bhi): (usize, usize, usize, usize),
) -> (usize, usize, usize) {
    let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for i in alo..ahi {
        let mut next: HashMap<usize, usize> = HashMap::new();
        for &j in b2j.get(&a[i]).map(Vec::as_slice).unwrap_or(&[]) {
            if j < blo {
                continue;
            }
            if j >= bhi {
                break;
            }
            let k = if j == 0 { 1 } else { j2len.get(&(j - 1)).copied().unwrap_or(0) + 1 };
            next.insert(j, k);
            if k > bestsize {
                (besti, bestj, bestsize) = (i + 1 - k, j + 1 - k, k);
            }
        }
        j2len = next;
    }
    (besti, bestj, bestsize)
}

fn matching_blocks(a: &[u32], b: &[u32]) -> Vec<(usize, usize, usize)> {
    let mut b2j: HashMap<u32, Vec<usize>> = HashMap::new();
    for (j, &t) in b.iter().enumerate() {
        b2j.entry(t).or_default().push(j);
    }
    let mut queue = vec![(0, a.len(), 0, b.len())];
    let mut blocks = Vec::new();
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(a, &b2j, (alo, ahi, blo, bhi));
        if k > 0 {
            blocks.push((i, j, k));
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    blocks.sort();
    let mut merged: Vec<(usize, usize, usize)> = Vec::new();
    for (i, j, k) in blocks {
        match merged.last_mut() {
            Some(last) if last.0 + last.2 == i && last.1 + last.2 == j => last.2 += k,
            _ => merged.push((i, j, k)),
        }
    }
    merged.push((a.len(), b.len(), 0));
    merged
}

/// Widens each change over the latin term it cuts into, so `cloud md` → `CLAUDE.md` is one edit and not
/// `cloud` → `CLAUDE` plus ` ` → `.`. Both sides grow over the same unchanged tokens, so an edit never claims text on
/// one side that it leaves out on the other. Changes that come to touch are one change.
fn grow(ta: &Tokens, tb: &Tokens, mut ops: Vec<[usize; 4]>) -> Vec<[usize; 4]> {
    loop {
        let before = ops.clone();
        for k in 0..ops.len() {
            let left = if k == 0 { 0 } else { ops[k - 1][1] };
            let right = if k + 1 == ops.len() { ta.len() } else { ops[k + 1][0] };
            let [mut i1, mut i2, mut j1, mut j2] = ops[k];
            let inside = (i1..i2).any(|t| ta.latin(t)) || (j1..j2).any(|t| tb.latin(t));
            let beside = (i1 > left && ta.latin(i1 - 1)) || (i2 < right && ta.latin(i2));
            if !(inside || beside) {
                continue;
            }
            while i1 > left {
                let beyond = (i1 >= 2 && ta.latin(i1 - 2)) || (j1 >= 2 && tb.latin(j1 - 2));
                if ta.kind[i1 - 1] == Kind::Latin || (ta.kind[i1 - 1] == Kind::Soft && beyond) {
                    (i1, j1) = (i1 - 1, j1 - 1);
                } else {
                    break;
                }
            }
            while i2 < right {
                let beyond = ta.latin(i2 + 1) || tb.latin(j2 + 1);
                if ta.kind[i2] == Kind::Latin || (ta.kind[i2] == Kind::Soft && beyond) {
                    (i2, j2) = (i2 + 1, j2 + 1);
                } else {
                    break;
                }
            }
            ops[k] = [i1, i2, j1, j2];
        }
        let mut merged: Vec<[usize; 4]> = Vec::new();
        for op in ops {
            match merged.last_mut() {
                Some(last) if last[1] == op[0] => (last[1], last[3]) = (op[1], op[3]),
                _ => merged.push(op),
            }
        }
        ops = merged;
        if ops == before {
            return ops;
        }
    }
}

/// One change from text `a` to text `b`: `a[a0..a1]` became `b[b0..b1]`, which is `new`. Offsets count characters.
#[derive(Debug, Clone, PartialEq)]
pub struct Edit {
    pub a0: usize,
    pub a1: usize,
    pub b0: usize,
    pub b1: usize,
    pub new: String,
}

pub fn edits(a: &[char], b: &[char]) -> Vec<Edit> {
    let mut ids = HashMap::new();
    let (ta, tb) = (Tokens::new(a, &mut ids), Tokens::new(b, &mut ids));
    let mut ops: Vec<[usize; 4]> = Vec::new();
    let (mut i, mut j) = (0, 0);
    for (ai, bj, size) in matching_blocks(&ta.id, &tb.id) {
        if i < ai || j < bj {
            ops.push([i, ai, j, bj]);
        }
        (i, j) = (ai + size, bj + size);
    }
    grow(&ta, &tb, ops)
        .into_iter()
        .map(|[i1, i2, j1, j2]| Edit {
            a0: ta.start[i1],
            a1: ta.start[i2],
            b0: tb.start[j1],
            b1: tb.start[j2],
            new: string(&b[tb.start[j1]..tb.start[j2]]),
        })
        .collect()
}

type Replacement<'a> = (usize, usize, &'a str);

fn replacement(e: &Edit) -> Replacement<'_> {
    (e.a0, e.a1, e.new.as_str())
}

/// `text[l..r]` with every replacement applied. Replacements lie inside the window and do not overlap.
fn splice(text: &[char], l: usize, r: usize, reps: &[Replacement]) -> String {
    let mut reps = reps.to_vec();
    reps.sort_by_key(|x| (x.0, x.1));
    let mut out = String::new();
    let mut cur = l;
    for (from, to, new) in reps {
        assert!(cur <= from && from <= to && to <= r, "replacements overlap or leave the window");
        out.extend(&text[cur..from]);
        out.push_str(new);
        cur = to;
    }
    out.extend(&text[cur..r]);
    out
}

/// Sentence spans. They tile the text; a sentence ends after `。！？!?；;`, a line break, or a full stop before spacing.
fn sentences(a: &[char]) -> Vec<(usize, usize)> {
    let (mut cuts, mut last) = (Vec::new(), 0);
    for k in 0..a.len() {
        let stop = a[k] == '.' && (k + 1 == a.len() || a[k + 1].is_whitespace());
        if stop || matches!(a[k], '。' | '！' | '？' | '!' | '?' | '；' | ';' | '\n') {
            cuts.push((last, k + 1));
            last = k + 1;
        }
    }
    if last < a.len() || cuts.is_empty() {
        cuts.push((last, a.len()));
    }
    cuts
}

// ── Questions for the judge ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AskKind {
    Site,
    Edit,
    Gate,
}

impl AskKind {
    pub fn name(self) -> &'static str {
        match self {
            AskKind::Site => "site",
            AskKind::Edit => "edit",
            AskKind::Gate => "gate",
        }
    }
}

/// One question for the judge. Equal keys are the same question: it is asked once, whoever needs the answer.
#[derive(Debug, Clone)]
pub struct Ask {
    pub kind: AskKind,
    pub state: Value,
    /// What a report calls this question. Not part of its identity.
    pub label: String,
}

impl Ask {
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind.name(), self.state)
    }
}

#[derive(Debug, Clone)]
pub enum Answer {
    /// A `score` question: probability of each of the three site levels.
    Levels { p: [f64; 3], confidence: f64 },
    /// A `choice` question: the chosen option and the probability of every option.
    Choice { choice: String, p: BTreeMap<String, f64>, confidence: f64 },
}

pub type Answers = HashMap<String, Answer>;

/// A human verdict from the case file: whoever wrote `written` where `heard` stood gets `credit`. Final, never re-asked.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub heard: String,
    pub written: String,
    pub credit: f64,
}

fn pinned(pins: &[Pin], heard: &str, written: &str) -> Option<f64> {
    pins.iter().find(|p| p.heard == heard && p.written == written).map(|p| p.credit)
}

// ── Reference: what a case requires ─────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Dictionary,
    Semantic,
    /// Words the speaker did not mean: a filler, a stutter, an abandoned half-sentence. `expected` has none of
    /// them, so the diff only takes words out there. Code alone sees whether they are gone; the credit goes
    /// into `clean`.
    Cleanup,
}

impl Tier {
    pub fn name(self) -> &'static str {
        match self {
            Tier::Dictionary => "dictionary",
            Tier::Semantic => "semantic",
            Tier::Cleanup => "cleanup",
        }
    }
}

struct TermReq {
    /// The term as the dictionary spells it, and as `expected` renders it here (CLAUDE for Claude).
    form: String,
    rendering: String,
    /// The recognizer already produced the term, only in another casing: then the casing is the correction.
    casing_only: bool,
}

struct Spec {
    tier: Tier,
    a0: usize,
    a1: usize,
    intended: String,
    own: Vec<usize>,
    terms: Vec<TermReq>,
}

pub struct Reference {
    inp: Vec<char>,
    expected: String,
    req: Vec<Edit>,
    sents: Vec<(usize, usize)>,
    specs: Vec<Spec>,
    terms: Vec<String>,
}

impl Reference {
    pub fn new(input: &str, expected: &str, terms: &[String]) -> Self {
        let inp: Vec<char> = input.trim().chars().collect();
        let exp: Vec<char> = expected.trim().chars().collect();
        let req = edits(&inp, &exp);

        // where `expected` carries a dictionary term; inside a longer term the shorter one does not count
        let mut occ: Vec<(usize, usize, usize)> = Vec::new();
        for (t, term) in terms.iter().enumerate() {
            let term: Vec<char> = term.chars().collect();
            occ.extend(find_term(&exp, &term, false).into_iter().map(|(s, e)| (s, e, t)));
        }
        let longest: Vec<(usize, usize, usize)> = occ
            .iter()
            .filter(|x| !occ.iter().any(|y| y.0 <= x.0 && x.1 <= y.1 && y.1 - y.0 > x.1 - x.0))
            .copied()
            .collect();

        // required edits that touch the same term belong to one site
        let mut group: Vec<usize> = (0..req.len()).collect();
        let mut touched: Vec<Vec<usize>> = Vec::new();
        for &(ts, te, _) in &longest {
            let over: Vec<usize> = (0..req.len()).filter(|&k| touches(req[k].b0, req[k].b1, ts, te)).collect();
            if let Some(&first) = over.first() {
                let (to, from): (usize, Vec<usize>) = (group[first], over.iter().map(|&k| group[k]).collect());
                for g in group.iter_mut() {
                    if from.contains(g) {
                        *g = to;
                    }
                }
            }
            touched.push(over);
        }

        let mut specs: Vec<Spec> = Vec::new();
        let mut in_dictionary_site = vec![false; req.len()];
        let mut roots: Vec<usize> = group.clone();
        roots.sort_unstable();
        roots.dedup();
        for root in roots {
            let own: Vec<usize> = (0..req.len()).filter(|&k| group[k] == root).collect();
            let here: Vec<&(usize, usize, usize)> = longest
                .iter()
                .zip(&touched)
                .filter(|(_, over)| over.iter().any(|k| own.contains(k)))
                .map(|(o, _)| o)
                .collect();
            if here.is_empty() {
                continue;
            }
            // widen to whole terms through the unchanged text around the edits
            let (first, last) = (&req[own[0]], &req[own[own.len() - 1]]);
            let b0 = here.iter().map(|o| o.0).fold(first.b0, usize::min);
            let b1 = here.iter().map(|o| o.1).fold(last.b1, usize::max);
            let (a0, a1) = (first.a0 + b0 - first.b0, last.a1 + b1 - last.b1);
            assert!(
                inp[a0..first.a0] == exp[b0..first.b0] && inp[last.a1..a1] == exp[last.b1..b1],
                "a term reaches past the unchanged text around its site"
            );
            let heard = &inp[a0..a1];
            let mut required: Vec<TermReq> = Vec::new();
            for &&(ts, te, t) in &here {
                let (form, rendering): (Vec<char>, &[char]) = (terms[t].chars().collect(), &exp[ts..te]);
                if !find_term(heard, rendering, true).is_empty() || !find_term(heard, &form, true).is_empty() {
                    continue; // already there as heard: whatever changed here, it was not this term
                }
                required.push(TermReq {
                    form: terms[t].clone(),
                    rendering: string(rendering),
                    casing_only: !find_term(heard, &form, false).is_empty(),
                });
            }
            if required.is_empty() {
                continue;
            }
            for &k in &own {
                in_dictionary_site[k] = true;
            }
            specs.push(Spec { tier: Tier::Dictionary, a0, a1, intended: string(&exp[b0..b1]), own, terms: required });
        }
        for (k, e) in req.iter().enumerate() {
            // spacing and sentence punctuation are formatting: required, but not what a site is scored for
            let (old, new) = (words_only(&string(&inp[e.a0..e.a1])), words_only(&e.new));
            if in_dictionary_site[k] || old == new {
                continue;
            }
            // words that only go away were never meant: a filler, a stutter, a half word before the whole one
            let tier = if subsequence(&new, &old) { Tier::Cleanup } else { Tier::Semantic };
            specs.push(Spec { tier, a0: e.a0, a1: e.a1, intended: e.new.clone(), own: vec![k], terms: Vec::new() });
        }
        specs.sort_by_key(|s| (s.a0, s.a1));
        let sents = sentences(&inp);
        Reference { inp, expected: string(&exp), req, sents, specs, terms: terms.to_vec() }
    }

    /// The sites of this case: tier, what was heard, what was intended.
    pub fn sites(&self) -> Vec<(Tier, String, String)> {
        self.specs.iter().map(|s| (s.tier, string(&self.inp[s.a0..s.a1]), s.intended.clone())).collect()
    }

    /// The whole sentences around `[lo, hi)`, widened until no edit of `edits` reaches across the border.
    fn window<'a>(&self, lo: usize, hi: usize, edits: impl Iterator<Item = &'a Edit> + Clone) -> (usize, usize) {
        let hit: Vec<&(usize, usize)> = self.sents.iter().filter(|(l, r)| touches(*l, *r, lo, hi)).collect();
        let mut l = hit.iter().map(|s| s.0).fold(lo, usize::min);
        let mut r = hit.iter().map(|s| s.1).fold(hi, usize::max);
        loop {
            let before = (l, r);
            for x in edits.clone().filter(|x| x.a0 < x.a1) {
                if x.a0 < r && x.a1 > l {
                    (l, r) = (l.min(x.a0), r.max(x.a1));
                }
            }
            if before == (l, r) {
                return (l, r);
            }
        }
    }

    /// The judge's view of one site: its sentence, identical in all three versions except at `[lo, hi)`. Every other
    /// required correction in the sentence is shown already made, so there is exactly one thing to judge.
    fn isolated(&self, spec: &Spec, (lo, hi): (usize, usize), [h, i, w]: [&str; 3]) -> Value {
        let (l, r) = self.window(lo, hi, self.req.iter());
        let others: Vec<Replacement> = self
            .req
            .iter()
            .enumerate()
            .filter(|(k, x)| !spec.own.contains(k) && within(x, l, r) && !touches(x.a0, x.a1, lo, hi))
            .map(|(_, x)| replacement(x))
            .collect();
        let ctx = |x: &str| {
            let mut reps = others.clone();
            reps.push((lo, hi, x));
            splice(&self.inp, l, r, &reps)
        };
        json!({
            "heard": ctx(h),
            "intended": ctx(i),
            "written": ctx(w),
            "phrase": { "heard": h, "intended": i, "written": w },
        })
    }

    /// The whole sentences around `[lo, hi)` as heard, as intended and as written, for a site that sits in a rewrite.
    fn rewritten(&self, (lo, hi): (usize, usize), act: &[Edit]) -> [String; 3] {
        let (l, r) = self.window(lo, hi, self.req.iter().chain(act));
        let pick = |es: &[Edit]| -> String {
            let reps: Vec<Replacement> = es.iter().filter(|x| within(x, l, r)).map(replacement).collect();
            splice(&self.inp, l, r, &reps)
        };
        [string(&self.inp[l..r]), pick(&self.req), pick(act)]
    }

    /// Finds, in one model output, what stands at every site and what else was changed.
    pub fn analyze(&self, output: &str, pins: &[Pin]) -> Analysis {
        let inp = &self.inp;
        let out: Vec<char> = output.trim().chars().collect();
        let act = edits(inp, &out);
        let mut claimed = vec![false; act.len()];
        let mut sites: Vec<SiteFinding> = Vec::new();

        for spec in &self.specs {
            let (s, e) = (spec.a0, spec.a1);
            let over: Vec<usize> = (0..act.len()).filter(|&k| touches(act[k].a0, act[k].a1, s, e)).collect();
            let lo = over.iter().map(|&k| act[k].a0).fold(s, usize::min);
            let hi = over.iter().map(|&k| act[k].a1).fold(e, usize::max);
            let reps: Vec<Replacement> = over.iter().map(|&k| replacement(&act[k])).collect();
            let h = string(&inp[lo..hi]);
            let i = splice(inp, lo, hi, &[(s, e, spec.intended.as_str())]);
            let w = splice(inp, lo, hi, &reps);
            // the model answered this site, and only this site, when its edit stays close on both sides of the diff
            let room = h.chars().count().max(i.chars().count()) + 2 * SLACK;
            let local = lo + SLACK >= s && hi <= e + SLACK && w.chars().count() <= room;
            let entangled = self.specs.iter().any(|other| {
                !std::ptr::eq(other, spec) && other.own.iter().any(|&k| touches(self.req[k].a0, self.req[k].a1, lo, hi))
            });
            if local {
                for &k in &over {
                    claimed[k] = true;
                }
            }

            let mut depends_on_gate = false;
            let outcome = if let Some(credit) = pinned(pins, &h, &w) {
                Outcome::settled("pin", credit)
            } else if spec.tier == Tier::Dictionary {
                if w == i {
                    Outcome::settled("code:exact", 1.0)
                } else if w == h {
                    Outcome::settled("code:unchanged", 0.0)
                } else {
                    let stands = |t: &TermReq| {
                        if t.casing_only {
                            has_term(&w, &t.form, true) || has_term(&w, &t.rendering, true)
                        } else {
                            has_term(&w, &t.form, false)
                        }
                    };
                    let credit = spec.terms.iter().filter(|t| stands(t)).count() as f64 / spec.terms.len() as f64;
                    depends_on_gate = !local && credit > 0.0;
                    Outcome::settled(if credit > 0.0 { "code:term" } else { "code:term-missing" }, credit)
                }
            } else if spec.tier == Tier::Cleanup && local {
                // the job is that the words are gone, and code can see that; what else changed here is an edit
                let (hw, iw, ww) = (folded(&words_only(&h)), folded(&words_only(&i)), folded(&words_only(&w)));
                let n = |x: &str| x.chars().count() as f64;
                let (credit, claim) = if ww == iw {
                    (1.0, true)
                } else if ww == hw {
                    (0.0, true)
                } else if subsequence(&ww, &hw) && subsequence(&iw, &ww) {
                    (1.0 - (n(&ww) - n(&iw)) / (n(&hw) - n(&iw)), true) // some of the words went, not all
                } else {
                    let filler = folded(&words_only(&string(&inp[s..e])));
                    let gone = ww.matches(filler.as_str()).count() < hw.matches(filler.as_str()).count();
                    (if gone { 1.0 } else { 0.0 }, false)
                };
                if !claim && !entangled {
                    for &k in &over {
                        claimed[k] = false;
                    }
                }
                Outcome::settled(if credit >= 1.0 { "code:removed" } else if credit <= 0.0 { "code:kept" } else { "code:part" }, credit)
            } else if spec.tier == Tier::Cleanup {
                let [heard, intended, written] = self.rewritten((lo, hi), &act);
                let (hw, iw, ww) = (folded(&words_only(&heard)), folded(&words_only(&intended)), folded(&words_only(&written)));
                let filler = folded(&words_only(&string(&inp[s..e])));
                let gone = ww == iw || ww.matches(filler.as_str()).count() < hw.matches(filler.as_str()).count();
                depends_on_gate = gone;
                Outcome::settled(if gone { "code:removed" } else { "code:kept" }, if gone { 1.0 } else { 0.0 })
            } else if local && !entangled {
                let (ch, ci, cw) = (core(&h), core(&i), core(&w));
                let casing_is_content = folded(ci) == folded(ch);
                if cw == ci {
                    Outcome::settled("code:exact", 1.0)
                } else if cw == ch {
                    Outcome::settled("code:unchanged", 0.0)
                } else if !casing_is_content && folded(cw) == folded(ci) {
                    Outcome::settled("code:case", 1.0)
                } else if !casing_is_content && folded(cw) == folded(ch) {
                    Outcome::settled("code:misheard-kept", 0.0)
                } else if cw.is_empty() {
                    Outcome::settled("code:dropped", 0.0)
                } else {
                    Outcome::Ask(Ask { kind: AskKind::Site, state: self.isolated(spec, (lo, hi), [&h, &i, &w]), label: format!("site  {:?} → {:?}", h, w) })
                }
            } else {
                let [heard, intended, written] = self.rewritten((lo, hi), &act);
                if written == intended {
                    Outcome::settled("code:exact", 1.0)
                } else {
                    let phrase = json!({ "heard": string(&inp[s..e]), "intended": spec.intended });
                    let state = json!({ "heard": heard, "intended": intended, "written": written, "phrase": phrase });
                    Outcome::Ask(Ask { kind: AskKind::Site, state, label: format!("site  {:?} → (rewritten)", string(&inp[s..e])) })
                }
            };

            let (hc, ic, wc): (Vec<char>, Vec<char>, Vec<char>) = (h.chars().collect(), i.chars().collect(), w.chars().collect());
            let half_fix = hc.len() == ic.len()
                && ic.len() == wc.len()
                && wc != hc
                && wc != ic
                && (0..wc.len()).all(|k| wc[k] == hc[k] || wc[k] == ic[k]);
            let context = format!(
                "{}[{}→{}]{}",
                string(&inp[lo.saturating_sub(6)..lo]),
                h,
                w,
                string(&inp[hi..(hi + 6).min(inp.len())])
            );
            sites.push(SiteFinding { tier: spec.tier, heard: h, intended: i, written: w, context, outcome, half_fix, depends_on_gate });
        }

        // everything else the model changed, grouped by the sentences it touches
        let mut groups: Vec<(usize, usize, Vec<usize>)> = Vec::new();
        let mut neutral = 0;
        for (k, x) in act.iter().enumerate().filter(|(k, _)| !claimed[*k]) {
            let old = string(&inp[x.a0..x.a1]);
            if folded(&words_only(&old)) == folded(&words_only(&x.new)) && !self.loses_term(&old, &x.new) {
                neutral += 1;
                continue;
            }
            let mut hit: Vec<usize> = (0..self.sents.len())
                .filter(|&n| touches(x.a0, x.a1, self.sents[n].0, self.sents[n].1))
                .collect();
            if x.a0 == x.a1 {
                hit.truncate(1); // text inserted between two sentences continues the first
            }
            groups.push((hit[0], hit[hit.len() - 1], vec![k]));
        }
        groups.sort();
        let mut merged: Vec<(usize, usize, Vec<usize>)> = Vec::new();
        for (first, last, xs) in groups {
            match merged.last_mut() {
                Some(g) if first <= g.1 => {
                    g.1 = g.1.max(last);
                    g.2.extend(xs);
                }
                _ => merged.push((first, last, xs)),
            }
        }
        let groups: Vec<GroupFinding> = merged
            .into_iter()
            .map(|(first, last, xs)| {
                let changes: Vec<[String; 2]> =
                    xs.iter().map(|&k| [string(&inp[act[k].a0..act[k].a1]), act[k].new.clone()]).collect();
                let verdicts: Vec<Option<f64>> = changes.iter().map(|c| pinned(pins, &c[0], &c[1])).collect();
                let outcome = if changes.iter().any(|c| self.loses_term(&c[0], &c[1])) {
                    Outcome::settled("code:term-lost", 0.0)
                } else if verdicts.iter().all(Option::is_some) {
                    Outcome::settled("pin", verdicts.iter().flatten().product())
                } else {
                    // the sentences as a reviewer accepts them: every other required correction made, these changes not
                    let (l, r) = self.window(self.sents[first].0, self.sents[last].1, self.req.iter());
                    let mut reps: Vec<Replacement> = self
                        .req
                        .iter()
                        .filter(|q| within(q, l, r) && !xs.iter().any(|&k| touches(act[k].a0, act[k].a1, q.a0, q.a1)))
                        .map(replacement)
                        .collect();
                    let before = splice(inp, l, r, &reps);
                    reps.extend(xs.iter().map(|&k| replacement(&act[k])));
                    let after = splice(inp, l, r, &reps);
                    let label = format!("edit  {}", changes_label(&changes));
                    Outcome::Ask(Ask { kind: AskKind::Edit, state: json!({ "before": before, "after": after }), label })
                };
                GroupFinding { changes, outcome }
            })
            .collect();

        let open = groups.iter().any(|g| matches!(g.outcome, Outcome::Ask(_))) || sites.iter().any(|s| s.depends_on_gate);
        let state = json!({ "reference": self.expected, "output": string(&out) });
        let gate = open.then(|| Ask { kind: AskKind::Gate, state, label: "gate  is this output a transcript at all?".to_string() });
        Analysis { sites, groups, gate, neutral }
    }

    /// A dictionary term stood in `old` exactly as the dictionary spells it, and `new` no longer has it.
    fn loses_term(&self, old: &str, new: &str) -> bool {
        let (old, new): (Vec<char>, Vec<char>) = (old.chars().collect(), new.chars().collect());
        self.terms.iter().any(|t| {
            let t: Vec<char> = t.chars().collect();
            find_term(&old, &t, true).len() > find_term(&new, &t, true).len()
        })
    }
}

// ── Analysis and score ──────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum Outcome {
    Settled { how: &'static str, credit: f64 },
    Ask(Ask),
}

impl Outcome {
    fn settled(how: &'static str, credit: f64) -> Self {
        Outcome::Settled { how, credit }
    }
}

#[derive(Debug, Clone)]
pub struct SiteFinding {
    pub tier: Tier,
    pub heard: String,
    pub intended: String,
    pub written: String,
    pub context: String,
    pub outcome: Outcome,
    /// Every character is either the misheard or the intended one, and both occur: judges are unreliable on these.
    pub half_fix: bool,
    /// Found inside a rewrite, not at its place: it only counts if the output is a transcript at all.
    pub depends_on_gate: bool,
}

#[derive(Debug, Clone)]
pub struct GroupFinding {
    pub changes: Vec<[String; 2]>,
    pub outcome: Outcome,
}

#[derive(Debug, Clone)]
pub struct Analysis {
    pub sites: Vec<SiteFinding>,
    pub groups: Vec<GroupFinding>,
    pub gate: Option<Ask>,
    /// Unrequested changes that code settled as spacing, sentence punctuation or casing.
    pub neutral: usize,
}

impl Analysis {
    pub fn asks(&self) -> Vec<&Ask> {
        let outcomes = self.sites.iter().map(|s| &s.outcome).chain(self.groups.iter().map(|g| &g.outcome));
        outcomes
            .filter_map(|o| match o {
                Outcome::Ask(ask) => Some(ask),
                Outcome::Settled { .. } => None,
            })
            .chain(self.gate.as_ref())
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SiteScore {
    pub tier: Tier,
    pub heard: String,
    pub intended: String,
    pub written: String,
    pub how: String,
    /// `None`: the judge was needed and gave no answer.
    pub credit: Option<f64>,
    pub level: Option<usize>,
    pub confidence: Option<f64>,
    pub probabilities: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditScore {
    pub changes: Vec<[String; 2]>,
    pub how: String,
    pub credit: Option<f64>,
    pub confidence: Option<f64>,
    pub probabilities: Option<BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateScore {
    pub kind: Option<String>,
    pub confidence: Option<f64>,
    pub probabilities: Option<BTreeMap<String, f64>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Scored {
    pub sites: Vec<SiteScore>,
    pub edits: Vec<EditScore>,
    pub gate: Option<GateScore>,
    /// 1.0 when the model touched nothing but the sites. `None`: unjudged.
    pub clean: Option<f64>,
    /// Changes outside the sites that were only spacing, sentence punctuation or casing. They cost nothing.
    pub neutral_edits: usize,
    /// Verdicts a human should look at, each with the strings a pin needs.
    #[serde(skip)]
    pub review: Vec<String>,
    /// What needed the judge and has no answer.
    #[serde(skip)]
    pub unjudged: Vec<String>,
}

fn changes_label(changes: &[[String; 2]]) -> String {
    changes.iter().map(|c| format!("{:?} → {:?}", c[0], c[1])).collect::<Vec<_>>().join(", ")
}

pub fn score(analysis: &Analysis, answers: &Answers) -> Scored {
    let (mut review, mut unjudged) = (Vec::new(), Vec::new());

    let gate = analysis.gate.as_ref().map(|ask| match answers.get(&ask.key()) {
        Some(Answer::Choice { choice, p, confidence }) => {
            if *confidence < REVIEW_BELOW {
                review.push(format!("gate  output called `{}` (conf {:.2})", choice, confidence));
            }
            GateScore { kind: Some(choice.clone()), confidence: Some(*confidence), probabilities: Some(p.clone()) }
        }
        _ => {
            unjudged.push("gate  is this output a transcript at all?".to_string());
            GateScore { kind: None, confidence: None, probabilities: None }
        }
    });
    let kind = gate.as_ref().and_then(|g| g.kind.as_deref());
    let gate_open = gate.is_some() && kind.is_none();
    let rejected = kind == Some("not_transcript");

    let mut sites = Vec::new();
    for f in &analysis.sites {
        let label = format!("{:?} → {:?} (wanted {:?})  in {}", f.heard, f.written, f.intended, f.context);
        let mut s = SiteScore {
            tier: f.tier,
            heard: f.heard.clone(),
            intended: f.intended.clone(),
            written: f.written.clone(),
            how: String::new(),
            credit: None,
            level: None,
            confidence: None,
            probabilities: None,
        };
        match &f.outcome {
            Outcome::Settled { how, credit } => {
                s.how = how.to_string();
                if !(f.depends_on_gate && gate_open) {
                    s.credit = Some(*credit);
                }
            }
            Outcome::Ask(ask) => {
                if let Some(Answer::Levels { p, confidence }) = answers.get(&ask.key()) {
                    let level = (0..3).fold(0, |best, k| if p[k] > p[best] { k } else { best });
                    s.how = "judge".to_string();
                    s.credit = Some((0..3).map(|k| p[k] * SITE_CREDIT[k]).sum());
                    (s.level, s.confidence, s.probabilities) = (Some(level), Some(*confidence), Some(*p));
                    if *confidence < REVIEW_BELOW || f.half_fix {
                        let why = if f.half_fix { ", half-fixed" } else { "" };
                        review.push(format!("site  {}: level {} (conf {:.2}{})", label, level, confidence, why));
                    }
                }
            }
        }
        if rejected {
            s.credit = Some(0.0);
            s.how = format!("{} gate:not_transcript", s.how).trim().to_string();
        } else if s.credit.is_none() {
            s.how = "unjudged".to_string();
            unjudged.push(format!("site  {}", label));
        }
        sites.push(s);
    }

    let mut edits = Vec::new();
    for g in &analysis.groups {
        let mut e = EditScore { changes: g.changes.clone(), how: String::new(), credit: None, confidence: None, probabilities: None };
        match &g.outcome {
            Outcome::Settled { how, credit } => (e.how, e.credit) = (how.to_string(), Some(*credit)),
            Outcome::Ask(ask) => match answers.get(&ask.key()) {
                Some(Answer::Choice { choice, p, confidence }) => {
                    let credit: f64 = EDIT_CREDIT.iter().map(|(k, c)| p[*k] * c).sum();
                    (e.how, e.credit) = (format!("judge:{}", choice), Some(credit));
                    (e.confidence, e.probabilities) = (Some(*confidence), Some(p.clone()));
                    if *confidence < REVIEW_BELOW {
                        review.push(format!("edit  {}: {} (conf {:.2})", changes_label(&g.changes), choice, confidence));
                    }
                }
                _ => {
                    e.how = "unjudged".to_string();
                    unjudged.push(format!("edit  {}", changes_label(&g.changes)));
                }
            },
        }
        edits.push(e);
    }

    // the fillers: all gone leaves `clean` whole, all still there halves it, in between pro rata
    let cleanup: Vec<Option<f64>> = sites.iter().filter(|s| s.tier == Tier::Cleanup).map(|s| s.credit).collect();
    let clean = if matches!(kind, Some(k) if k != "transcript") {
        Some(0.0)
    } else if gate_open || edits.iter().any(|e| e.credit.is_none()) || cleanup.contains(&None) {
        None
    } else {
        let tidy = match cleanup.len() {
            0 => 1.0,
            n => FILLERS_KEPT_CREDIT + (1.0 - FILLERS_KEPT_CREDIT) * cleanup.iter().flatten().sum::<f64>() / n as f64,
        };
        Some(edits.iter().filter_map(|e| e.credit).product::<f64>() * tidy)
    };
    Scored { sites, edits, gate, clean, neutral_edits: analysis.neutral, review, unjudged }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEARD: &str = "应该会记录一些我们正在进行的事情，但是不要记录细节，而是指向下一集的导航，比如说我们这次发现了哪些信息、哪些报告、哪些结论，在查看哪个目录。这是一个 high level 的项目介绍，以及一些关键的功能以及关键的导航。然后我们的 cloud md 里边就去指向 read me。然后把一些过时的文档做一下清理。";
    const SAID: &str = "应该会记录一些我们正在进行的事情，但是不要记录细节，而是指向下一级的导航，比如说我们这次发现了哪些信息、哪些报告、哪些结论，在查看哪个目录。这是一个 high level 的项目介绍，以及一些关键的功能以及关键的导航。然后我们的 CLAUDE.md 里边就去指向 README。然后把一些过时的文档做一下清理。";

    fn chars(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn reference(input: &str, expected: &str, terms: &[&str]) -> Reference {
        Reference::new(input, expected, &terms.iter().map(|t| t.to_string()).collect::<Vec<_>>())
    }

    /// Credit of every site, by code alone.
    fn credits(r: &Reference, output: &str) -> Vec<Option<f64>> {
        score(&r.analyze(output, &[]), &Answers::new()).sites.iter().map(|s| s.credit).collect()
    }

    #[test]
    fn a_change_covers_the_whole_latin_term_and_both_sides_alike() {
        let a = chars("我们的 cloud md 里边");
        let found = edits(&a, &chars("我们的 CLAUDE.md 里边"));
        assert_eq!(found.len(), 1);
        assert_eq!((string(&a[found[0].a0..found[0].a1]).as_str(), found[0].new.as_str()), ("cloud md", "CLAUDE.md"));
        // applying the edits to the first text gives the second, whatever grew or merged
        for (a, b) in [("用 docker 库伯 部署", "用 Docker Kube 部署"), ("跑 k8s 的 pod", "在 K8s 跑 Pod。"), ("", "x y"), ("a-b c", "")] {
            let (a, b) = (chars(a), chars(b));
            let reps = edits(&a, &b);
            assert_eq!(splice(&a, 0, a.len(), &reps.iter().map(replacement).collect::<Vec<_>>()), string(&b));
        }
        assert_eq!(edits(&chars("用 docker 库伯 部署"), &chars("用 Docker Kube 部署")).len(), 1);
    }

    #[test]
    fn sites_come_from_the_diff_and_the_dictionary_decides_their_tier() {
        let r = reference(HEARD, SAID, &["Claude"]);
        let site = |tier, heard: &str, intended: &str| (tier, heard.to_string(), intended.to_string());
        assert_eq!(
            r.sites(),
            vec![site(Tier::Semantic, "集", "级"), site(Tier::Dictionary, "cloud md", "CLAUDE.md"), site(Tier::Semantic, "read me", "README")]
        );
        // a dictionary site is the whole term, not only the character that differs
        assert_eq!(reference("请章伟看一下", "请章玮看一下。", &["章玮"]).sites(), vec![site(Tier::Dictionary, "章伟", "章玮")]);
        // formatting is required but is not a site; a term the recognizer already got right is not a dictionary site
        assert!(reference("这是 high level 的介绍,好", "这是 high-level 的介绍，好。", &[]).sites().is_empty());
        assert_eq!(reference("看 Claude md", "看 CLAUDE.md", &["Claude"]).sites(), vec![site(Tier::Semantic, "Claude md", "CLAUDE.md")]);
    }

    #[test]
    fn a_dictionary_site_passes_iff_the_term_stands_there() {
        let r = reference(HEARD, SAID, &["Claude"]);
        for (written, credit) in [("CLAUDE.md", 1.0), ("Claude.md", 1.0), ("Claude md", 1.0), ("Claude", 1.0), ("claude.md", 1.0)] {
            assert_eq!(credits(&r, &SAID.replace("CLAUDE.md", written))[1], Some(credit), "{}", written);
        }
        for written in ["Cloud.md", "cloud.md", "cloud md", "Claudette.md", ""] {
            assert_eq!(credits(&r, &SAID.replace("CLAUDE.md", written))[1], Some(0.0), "{:?}", written);
        }
        // the term somewhere else in the output does not stand at the site
        assert_eq!(credits(&r, &(SAID.replace("CLAUDE.md", "Cloud.md") + "（注：Claude 相关）"))[1], Some(0.0));

        let name = reference("请章伟看一下。", "请章玮看一下。", &["章玮"]);
        assert_eq!(credits(&name, "请章玮看一下。"), vec![Some(1.0)]);
        assert_eq!(credits(&name, "请张玮看一下。"), vec![Some(0.0)]);
        // where the recognizer had the term and only its casing was wrong, the casing is what is scored
        let casing = reference("装一下 docker 吧", "装一下 Docker 吧", &["Docker"]);
        assert_eq!(credits(&casing, "装一下 Docker 吧"), vec![Some(1.0)]);
        assert_eq!(credits(&casing, "装一下 DOCKER 吧"), vec![Some(0.0)]);
    }

    #[test]
    fn code_settles_a_semantic_site_only_where_that_is_exact() {
        let r = reference(HEARD, SAID, &["Claude"]);
        let readme = |written: &str| r.analyze(&SAID.replace("README", written), &[]).sites[2].outcome.clone();
        let settled = |o: Outcome| match o {
            Outcome::Settled { how, credit } => Some((how, credit)),
            Outcome::Ask(_) => None,
        };
        assert_eq!(settled(readme("README")), Some(("code:exact", 1.0)));
        assert_eq!(settled(readme("read me")), Some(("code:unchanged", 0.0)));
        assert_eq!(settled(readme("Readme")), Some(("code:case", 1.0)));
        assert_eq!(settled(readme("Read Me")), Some(("code:misheard-kept", 0.0)));
        assert_eq!(settled(readme("")), Some(("code:dropped", 0.0)));
        let Outcome::Ask(ask) = readme("README.md") else { panic!("an extension takes judgment") };
        // the judge sees the sentence with every other correction made, differing only at the site
        assert_eq!(ask.state["intended"], "然后我们的 CLAUDE.md 里边就去指向 README。");
        assert_eq!(ask.state["written"], "然后我们的 CLAUDE.md 里边就去指向 README.md。");
        assert_eq!(ask.state["phrase"], json!({ "heard": "read me", "intended": "README", "written": "README.md" }));
        // where casing is the correction, another casing is not settled by folding
        let casing = reference("装一下 docker 吧", "装一下 Docker 吧", &[]);
        assert_eq!(settled(casing.analyze("装一下  docker  吧", &[]).sites[0].outcome.clone()), Some(("code:unchanged", 0.0)));
        assert!(settled(casing.analyze("装一下 DOCKER 吧", &[]).sites[0].outcome.clone()).is_none());
    }

    #[test]
    fn changes_outside_the_sites_are_neutral_lost_terms_or_questions() {
        let r = reference(HEARD, SAID, &["Claude"]);
        // spacing, sentence punctuation and casing: clean by code, no question at all
        let tidy = r.analyze(&SAID.replace("high level", "high-level").replace("，但是", "。但是").replace(" 的项目", "的项目"), &[]);
        assert_eq!((tidy.neutral, tidy.groups.len(), tidy.asks().len()), (2, 0, 0));
        assert_eq!(score(&tidy, &Answers::new()).clean, Some(1.0));
        // a wrapper is not neutral, and one question covers all changes to a sentence
        let quoted = r.analyze(&format!("“{}”", SAID), &[]);
        assert_eq!(quoted.asks().iter().map(|a| a.kind).collect::<Vec<_>>(), vec![AskKind::Edit, AskKind::Edit, AskKind::Gate]);
        // a long insertion where a short one was required is not the answer to that site
        let tail = reference("今天开会讨论预算", "今天开会讨论了预算", &[]);
        assert_eq!(tail.analyze("今天开会讨论了预算", &[]).groups.len(), 0);
        assert_eq!(tail.analyze("今天开会讨论了很多很多其他的事情，顺便还有预算", &[]).groups.len(), 1);
        // a dictionary term the recognizer got right and the model broke
        let kept = reference("用 Claude 写下一集", "用 Claude 写下一级", &["Claude"]);
        let broken = score(&kept.analyze("用 Cloud 写下一级", &[]), &Answers::new());
        assert_eq!((broken.sites[0].credit, broken.edits[0].how.as_str(), broken.clean), (Some(1.0), "code:term-lost", Some(0.0)));
        assert_eq!(score(&kept.analyze("用 claude 写下一级", &[]), &Answers::new()).clean, Some(0.0));
    }

    #[test]
    fn a_pin_is_final_and_a_missing_answer_is_unjudged() {
        let r = reference(HEARD, SAID, &["Claude"]);
        let output = SAID.replace("下一级", "下一层");
        let open = score(&r.analyze(&output, &[]), &Answers::new());
        assert_eq!((open.sites[0].credit, open.sites[0].how.as_str(), open.unjudged.len()), (None, "unjudged", 1));
        assert_eq!(open.clean, Some(1.0));
        let pin = Pin { heard: "集".to_string(), written: "层".to_string(), credit: 1.0 };
        let pinned = r.analyze(&output, &[pin]);
        assert!(pinned.asks().is_empty());
        assert_eq!(score(&pinned, &Answers::new()).sites[0].credit, Some(1.0));
    }

    fn levels(p: [f64; 3], confidence: f64) -> Answer {
        Answer::Levels { p, confidence }
    }

    fn choice(options: &[&str], chosen: &str, p_chosen: f64, other: &str, confidence: f64) -> Answer {
        let mut p: BTreeMap<String, f64> = options.iter().map(|o| (o.to_string(), 0.0)).collect();
        p.insert(other.to_string(), 1.0 - p_chosen);
        p.insert(chosen.to_string(), p_chosen);
        Answer::Choice { choice: chosen.to_string(), p, confidence }
    }

    /// Twelve outputs for one case: the perfect one, the untouched one, and ten with one known defect each. Code settles
    /// what it can; where the judge is asked, it gets the verdict it gave on that output when the questions were
    /// validated. Every defect has to cost credit in the tier it belongs to, and nothing may outrank the perfect output.
    #[test]
    fn every_defect_costs_credit_and_nothing_outranks_the_perfect_output() {
        let r = reference(HEARD, SAID, &["Claude"]);
        let edit_kinds: Vec<&str> = EDIT_CREDIT.iter().map(|(k, _)| *k).collect();
        let edit = |chosen, p, confidence| Some(choice(&edit_kinds, chosen, p, "neutral", confidence));
        let gate = |chosen, confidence| Some(choice(&GATE_KINDS, chosen, 0.9, "transcript", confidence));
        // output, the judge's site / edit / gate verdict, expected [dictionary, semantic, clean]
        let outputs: Vec<(&str, String, [Option<Answer>; 3], [f64; 3])> = vec![
            ("perfect", SAID.to_string(), [None, None, None], [1.0, 2.0, 1.0]),
            ("untouched", HEARD.to_string(), [None, None, None], [0.0, 0.0, 1.0]),
            ("synonym", SAID.replace("下一级", "下一层"), [Some(levels([0.40, 0.04, 0.56], 0.0)), None, None], [1.0, 1.58, 1.0]),
            ("extension", SAID.replace("README", "README.md"), [Some(levels([0.05, 0.20, 0.75], 0.6)), None, None], [1.0, 1.85, 1.0]),
            ("misheard term reformatted", SAID.replace("CLAUDE.md", "Cloud.md"), [None, None, None], [0.0, 2.0, 1.0]),
            (
                "term only mentioned",
                SAID.replace("CLAUDE.md", "Cloud.md") + "（注：Claude 相关）",
                [None, edit("commentary", 1.0, 0.9), gate("transcript", 0.32)],
                [0.0, 2.0, 0.0],
            ),
            ("negation flipped", SAID.replace("不要记录细节", "要记录细节"), [None, edit("damages", 0.97, 0.9), gate("transcript", 0.9)], [1.0, 2.0, 0.03]),
            (
                "hallucinated tail",
                SAID.to_string() + "此外，建议你使用 Git 进行版本管理，并定期归档旧文档到 archive 目录。",
                [None, edit("damages", 0.9, 0.8), gate("extended", 0.9)],
                [1.0, 2.0, 0.0],
            ),
            ("preamble", format!("好的，以下是纠正后的文本：\n\n{}", SAID), [None, edit("commentary", 1.0, 0.9), gate("extended", 0.9)], [1.0, 2.0, 0.0]),
            (
                "answered instead of correcting",
                "你说得对，CLAUDE.md 应该只做高层导航，指向 README，并且清理过时文档。".to_string(),
                [None, None, gate("not_transcript", 0.9)],
                [0.0, 0.0, 0.0],
            ),
            (
                "summarized",
                "记录进行中的事项并指向下一级导航；CLAUDE.md 指向 README；清理过时文档。".to_string(),
                [None, None, gate("not_transcript", 0.9)],
                [0.0, 0.0, 0.0],
            ),
            (
                "restyled",
                SAID.replace("然后把一些过时的文档做一下清理。", "然后清理一些过时的文档。"),
                [None, edit("rephrases", 1.0, 0.9), gate("transcript", 0.9)],
                [1.0, 2.0, 0.7],
            ),
        ];
        for (name, output, verdicts, expected) in &outputs {
            let analysis = r.analyze(output, &[]);
            let mut answers = Answers::new();
            for ask in analysis.asks() {
                if let Some(answer) = &verdicts[ask.kind as usize] {
                    answers.insert(ask.key(), answer.clone());
                }
            }
            assert_eq!(verdicts.iter().all(Option::is_none), analysis.asks().is_empty(), "{}: what needs the judge", name);
            let scored = score(&analysis, &answers);
            let tier = |t| scored.sites.iter().filter(|s| s.tier == t).map(|s| s.credit.expect("judged")).sum::<f64>();
            let got = [tier(Tier::Dictionary), tier(Tier::Semantic), scored.clean.expect("judged")];
            for k in 0..3 {
                assert!((got[k] - expected[k]).abs() < 1e-9, "{}: got {:?}, expected {:?}", name, got, expected);
            }
            assert!(*name == "perfect" || got.iter().sum::<f64>() < 4.0, "{} scores like the perfect output", name);
            // a verdict the judge was unsure of is put before a human
            let unsure = ["synonym", "term only mentioned"].contains(name);
            assert_eq!(!scored.review.is_empty(), unsure, "{}: review {:?}", name, scored.review);
        }
    }

    #[test]
    fn fillers_are_cleanup_sites_that_code_settles_into_clean() {
        let site = |tier, heard: &str, intended: &str| (tier, heard.to_string(), intended.to_string());
        let r = reference("嗯，把这这个数据处理一下，然后呃看看变化。", "把这个数据处理一下，然后看看变化。", &[]);
        assert_eq!(r.sites(), vec![site(Tier::Cleanup, "嗯，", ""), site(Tier::Cleanup, "这", ""), site(Tier::Cleanup, "呃", "")]);
        let clean = |output: &str| {
            let analysis = r.analyze(output, &[]);
            assert!(analysis.asks().is_empty(), "{:?}: {:?}", output, analysis.asks());
            score(&analysis, &Answers::new()).clean
        };
        // all gone, none gone, some gone: the credit goes into clean, and no question is asked
        assert_eq!(credits(&r, "把这个数据处理一下，然后看看变化。"), vec![Some(1.0); 3]);
        assert_eq!(clean("把这个数据处理一下，然后看看变化。"), Some(1.0));
        assert_eq!(credits(&r, "嗯，把这这个数据处理一下，然后呃看看变化。"), vec![Some(0.0); 3]);
        assert_eq!(clean("嗯，把这这个数据处理一下，然后呃看看变化。"), Some(FILLERS_KEPT_CREDIT));
        assert_eq!(credits(&r, "嗯，把这个数据处理一下，然后看看变化。"), vec![Some(0.0), Some(1.0), Some(1.0)]);
        assert_eq!(clean("嗯，把这个数据处理一下，然后看看变化。"), Some(FILLERS_KEPT_CREDIT + (1.0 - FILLERS_KEPT_CREDIT) * 2.0 / 3.0));
        // other words where the filler stood: it is gone, and what came instead is an edit for the judge
        let reworded = reference("本本质上是这样。", "本质上是这样。", &[]);
        let analysis = reworded.analyze("从本质上是这样。", &[]);
        assert_eq!((credits(&reworded, "从本质上是这样。"), analysis.groups.len()), (vec![Some(1.0)], 1));
        assert_eq!(analysis.groups[0].changes, vec![["本".to_string(), "从".to_string()]]);
        // a stutter half taken out is half done
        let stutter = reference("那就可以去呃去去去呃图放大。", "那就可以去图放大。", &[]);
        assert_eq!(stutter.sites(), vec![site(Tier::Cleanup, "呃去去去呃", "")]);
        let part = score(&stutter.analyze("那就可以去去图放大。", &[]), &Answers::new());
        assert_eq!((part.sites[0].how.as_str(), part.sites[0].credit), ("code:part", Some(0.8)));
        // more than the filler went: the site is done, the rest is an edit for the judge like any other
        let collateral = reference("呃，然后我们看图。", "然后我们看图。", &[]);
        let analysis = collateral.analyze("我们看图。", &[]);
        assert_eq!(credits(&collateral, "我们看图。"), vec![Some(1.0)]);
        assert_eq!(analysis.groups.len(), 1);
        assert_eq!(analysis.groups[0].changes, vec![["呃，然后".to_string(), String::new()]]);
        // a filler that merges into a dictionary site does not decide it: the term standing there does
        let merged = reference("这个呃制作反力很大", "支座反力很大", &["支座"]);
        assert_eq!(merged.sites(), vec![site(Tier::Dictionary, "这个呃制作", "支座")]);
        assert_eq!(credits(&merged, "这个呃支座反力很大"), vec![Some(1.0)]);
        // a filler left in `expected` is one the author keeps: taking it out is an ordinary edit
        let kept = reference("嗯，我们看下一集。", "嗯，我们看下一级。", &[]);
        let analysis = kept.analyze("我们看下一级。", &[]);
        assert_eq!((analysis.sites.len(), analysis.groups.len()), (1, 1));
    }

    #[test]
    fn degenerate_outputs_are_analyzed_without_a_panic() {
        let r = reference(HEARD, SAID, &["Claude"]);
        for output in ["", "。", "cloud md", "README", &SAID.repeat(2), &SAID.chars().rev().collect::<String>()] {
            let analysis = r.analyze(output, &[]);
            assert!(analysis.gate.is_some(), "{:?}", output);
            score(&analysis, &Answers::new());
        }
        let empty = reference("", "", &["Claude"]);
        assert!(empty.analyze("", &[]).asks().is_empty());
        score(&empty.analyze("Claude", &[]), &Answers::new());
    }
}
