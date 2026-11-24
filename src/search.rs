use std::collections::HashMap;
use std::ops::Range;

use anyhow::Result;
use ndarray::{Array2, ArrayView2, Axis};
use rayon::prelude::*;

use crate::chunk::ChunkHash;
use crate::embed::Encoder;
use crate::store::{Filter, Passage, SessionRow, Store};
use crate::vector;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub limit: usize,
    pub before: usize,
    pub after: usize,
}

/// One match: the passage at index `matched`, shown with the passages in `range` around it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    pub matched: usize,
    pub range: Range<usize>,
    /// How many other chunks in scope have the same text.
    pub elsewhere: usize,
}

/// The matches in one session, in session order, with every passage of the session they index into.
#[derive(Debug, Clone)]
pub struct SessionHits {
    pub session: SessionRow,
    pub passages: Vec<Passage>,
    pub hits: Vec<Hit>,
}

pub fn find(store: &Store, filter: &Filter, encoder: &impl Encoder, query: &str, window: Window) -> Result<Vec<SessionHits>> {
    let pool = window.limit * 4 + 50;
    let semantic = top_k(store, filter, &encoder.encode_query(query)?, pool)?;
    let lexical = match fts_query(query) {
        Some(fts_query) => store.lexical(filter, &fts_query, pool)?,
        None => Vec::new(),
    };
    collect_hits(store, filter, fuse(&semantic, &lexical), window)
}

/// Turns ranked chunk hashes into hits until `window.limit` are found. A chunk inside the range of a better hit of the
/// same session is skipped, and ranges are cut so that hits of one session never overlap.
fn collect_hits(store: &Store, filter: &Filter, ranked: Vec<ChunkHash>, window: Window) -> Result<Vec<SessionHits>> {
    let mut passages: HashMap<String, Vec<Passage>> = HashMap::new();
    let mut found: Vec<(SessionRow, Hit)> = Vec::new();
    for hash in ranked {
        if found.len() == window.limit {
            break;
        }
        let occurrences = store.occurrences(filter, &hash)?;
        let Some(occurrence) = occurrences.first() else {
            continue;
        };
        let path = occurrence.session.path.clone();
        if !passages.contains_key(&path) {
            passages.insert(path.clone(), store.passages(&path)?);
        }
        let all = &passages[&path];
        let Some(matched) = all.iter().position(|p| *p == occurrence.passage) else {
            continue;
        };
        let taken = || found.iter().filter(|(session, _)| session.path == path).map(|(_, hit)| &hit.range);
        if taken().any(|range| range.contains(&matched)) {
            continue;
        }
        let range = around(matched, all.len(), window, taken());
        found.push((occurrence.session.clone(), Hit { matched, range, elsewhere: occurrences.len() - 1 }));
    }
    Ok(group(found, passages))
}

fn around<'a>(matched: usize, len: usize, window: Window, taken: impl Iterator<Item = &'a Range<usize>>) -> Range<usize> {
    let mut range = matched.saturating_sub(window.before)..(matched + window.after + 1).min(len);
    for other in taken {
        if other.end <= matched {
            range.start = range.start.max(other.end);
        } else {
            range.end = range.end.min(other.start);
        }
    }
    range
}

/// Groups hits by session, sessions in the order of their best hit, hits in session order.
fn group(found: Vec<(SessionRow, Hit)>, mut passages: HashMap<String, Vec<Passage>>) -> Vec<SessionHits> {
    let mut groups: Vec<SessionHits> = Vec::new();
    for (session, hit) in found {
        if let Some(group) = groups.iter_mut().find(|group| group.session.path == session.path) {
            group.hits.push(hit);
        } else {
            let passages = passages.remove(&session.path).unwrap_or_default();
            groups.push(SessionHits { session, passages, hits: vec![hit] });
        }
    }
    for group in &mut groups {
        group.hits.sort_by_key(|hit| hit.matched);
    }
    groups
}

fn top_k(store: &Store, filter: &Filter, query: &Array2<f32>, k: usize) -> Result<Vec<ChunkHash>> {
    let dim = query.ncols();
    // Scoring runs in parallel, so the blobs are copied out of the rows first, into one buffer: that takes a third
    // of the time of one allocation per blob.
    let mut data = Vec::new();
    let mut documents = Vec::new();
    store.for_each_vector(filter, |stored| {
        let start = data.len();
        data.extend_from_slice(stored.data);
        documents.push(Document { hash: stored.hash, tokens: stored.tokens, blob: start..data.len() });
    })?;

    let mut ranked: Vec<(ChunkHash, f32)> = documents
        .par_iter()
        .filter_map(|document| {
            let matrix = vector::decode(&data[document.blob.clone()], document.tokens, dim)?;
            Some((document.hash, max_sim(query.view(), matrix.view())))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
    ranked.truncate(k);
    Ok(ranked.into_iter().map(|(hash, _)| hash).collect())
}

struct Document {
    hash: ChunkHash,
    tokens: usize,
    blob: Range<usize>,
}

fn fuse(semantic: &[ChunkHash], lexical: &[ChunkHash]) -> Vec<ChunkHash> {
    let mut weights: HashMap<ChunkHash, f32> = HashMap::new();
    for list in [semantic, lexical] {
        for (rank, hash) in list.iter().enumerate() {
            *weights.entry(*hash).or_default() += 1.0 / (RRF_K + rank as f32 + 1.0);
        }
    }
    let mut fused: Vec<(ChunkHash, f32)> = weights.into_iter().collect();
    fused.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    fused.into_iter().map(|(hash, _)| hash).collect()
}

const RRF_K: f32 = 60.0;

pub fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|word| word.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|word| word.chars().count() >= 3)
        .map(|word| format!("\"{}\"", word.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" OR "))
}

pub(crate) fn max_sim(query: ArrayView2<'_, f32>, document: ArrayView2<'_, f32>) -> f32 {
    if document.nrows() == 0 {
        return 0.0;
    }
    query.dot(&document.t()).map_axis(Axis(1), |row| row.fold(f32::NEG_INFINITY, |best, &x| best.max(x))).sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embed::pipeline;
    use crate::store::StoredVector;
    use crate::test_support::encoder::FakeEncoder;
    use crate::test_support::lines::{Transcript, text};
    use crate::test_support::store::TempStore;
    use crate::vector;
    use ndarray::array;

    fn five_turns(store: &mut TempStore, id: &str, start: &str) {
        let transcript = Transcript::new("/p")
            .at(start)
            .user("alpha one")
            .assistant("a1", vec![text("beta two")])
            .user("gamma three")
            .assistant("a2", vec![text("delta four")])
            .user("epsilon five");
        store.add("-p", id, &transcript);
    }

    fn hashes(texts: &[&str]) -> Vec<ChunkHash> {
        texts.iter().map(|text| ChunkHash::of(text)).collect()
    }

    fn window(limit: usize, before: usize, after: usize) -> Window {
        Window { limit, before, after }
    }

    /// `(session id, [(matched, range)])` per group.
    type Summary = Vec<(String, Vec<(usize, Range<usize>)>)>;

    fn summary(groups: &[SessionHits]) -> Summary {
        groups
            .iter()
            .map(|group| (group.session.session_id.clone(), group.hits.iter().map(|hit| (hit.matched, hit.range.clone())).collect()))
            .collect()
    }

    fn collect(store: &TempStore, texts: &[&str], window: Window) -> Summary {
        summary(&collect_hits(store, &Filter::default(), hashes(texts), window).unwrap())
    }

    #[test]
    fn a_hit_without_context_is_just_the_match() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        let groups = collect_hits(&store, &Filter::default(), hashes(&["gamma three"]), window(8, 0, 0)).unwrap();
        assert_eq!(summary(&groups), [("s1".to_owned(), vec![(2, 2..3)])]);
        assert_eq!(groups[0].passages.len(), 5);
        assert_eq!(groups[0].passages[2].text, "gamma three");
        assert_eq!(groups[0].hits[0].elsewhere, 0);
    }

    #[test]
    fn context_is_clamped_to_the_session() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        assert_eq!(collect(&store, &["gamma three"], window(8, 1, 1)), [("s1".to_owned(), vec![(2, 1..4)])]);
        assert_eq!(collect(&store, &["alpha one"], window(8, 2, 1)), [("s1".to_owned(), vec![(0, 0..2)])]);
        assert_eq!(collect(&store, &["epsilon five"], window(8, 1, 3)), [("s1".to_owned(), vec![(4, 3..5)])]);
    }

    #[test]
    fn a_match_inside_a_better_hits_range_is_skipped() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        assert_eq!(collect(&store, &["gamma three", "delta four"], window(8, 1, 1)), [("s1".to_owned(), vec![(2, 1..4)])]);
    }

    #[test]
    fn ranges_of_one_session_never_overlap() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        let groups = collect(&store, &["beta two", "delta four"], window(8, 1, 1));
        assert_eq!(groups, [("s1".to_owned(), vec![(1, 0..3), (3, 3..5)])]);
        // The better-ranked hit keeps its context and the other one is cut.
        let groups = collect(&store, &["delta four", "beta two"], window(8, 1, 1));
        assert_eq!(groups, [("s1".to_owned(), vec![(1, 0..2), (3, 2..5)])]);
    }

    #[test]
    fn hits_are_in_session_order_and_sessions_in_rank_order() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        let transcript = Transcript::new("/p").at("2026-01-02T10:00:00.000Z").user("zeta six").assistant("z", vec![text("eta seven")]);
        store.add("-p", "s2", &transcript);

        let groups = collect(&store, &["epsilon five", "eta seven", "alpha one"], window(8, 0, 0));
        assert_eq!(groups, [("s1".to_owned(), vec![(0, 0..1), (4, 4..5)]), ("s2".to_owned(), vec![(1, 1..2)])]);
        let groups = collect(&store, &["eta seven", "epsilon five"], window(8, 0, 0));
        assert_eq!(groups, [("s2".to_owned(), vec![(1, 1..2)]), ("s1".to_owned(), vec![(4, 4..5)])]);
    }

    #[test]
    fn collecting_stops_at_the_limit() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        let groups = collect(&store, &["alpha one", "gamma three", "epsilon five"], window(2, 0, 0));
        assert_eq!(groups, [("s1".to_owned(), vec![(0, 0..1), (2, 2..3)])]);
    }

    #[test]
    fn unknown_hashes_are_skipped() {
        let mut store = TempStore::new();
        five_turns(&mut store, "s1", "2026-01-01T10:00:00.000Z");
        assert_eq!(collect(&store, &["not indexed", "beta two"], window(1, 0, 0)), [("s1".to_owned(), vec![(1, 1..2)])]);
        assert!(collect_hits(&store, &Filter::default(), Vec::new(), window(8, 0, 0)).unwrap().is_empty());
    }

    #[test]
    fn text_repeated_across_sessions_counts_as_elsewhere_and_shows_the_latest() {
        let mut store = TempStore::new();
        five_turns(&mut store, "old", "2026-01-01T10:00:00.000Z");
        five_turns(&mut store, "new", "2026-02-01T10:00:00.000Z");
        let groups = collect_hits(&store, &Filter::default(), hashes(&["gamma three"]), window(8, 0, 0)).unwrap();
        assert_eq!(summary(&groups), [("new".to_owned(), vec![(2, 2..3)])]);
        assert_eq!(groups[0].hits[0].elsewhere, 1);
    }

    #[test]
    fn max_sim_takes_best_token_per_query_token() {
        let query = array![[1.0, 0.0], [0.0, 1.0]];
        let document = array![[1.0, 0.0], [0.6, 0.8]];
        assert!((max_sim(query.view(), document.view()) - 1.8).abs() < 1e-6);
    }

    #[test]
    fn fusion_rewards_agreement() {
        let [a, b, c, d] = [1, 2, 3, 9].map(ChunkHash::filled);
        let fused = fuse(&[a, b, c], &[c, d]);
        assert_eq!(fused[0], c);
        assert_eq!(fused.len(), 4);
    }

    #[test]
    fn max_sim_of_a_document_without_tokens_is_zero() {
        assert!(max_sim(array![[1.0, 0.0]].view(), Array2::zeros((0, 2)).view()).abs() < f32::EPSILON);
    }

    fn with_vectors(store: &mut TempStore, documents: &[(&str, Array2<f32>)]) {
        let transcript = documents.iter().fold(Transcript::new("/p"), |transcript, (chunk, _)| transcript.user(chunk));
        store.add("-p", "s1", &transcript);
        let blobs: Vec<_> = documents.iter().map(|(_, matrix)| vector::encode(matrix)).collect();
        let vectors: Vec<_> = documents
            .iter()
            .zip(&blobs)
            .map(|((chunk, matrix), data)| StoredVector { hash: ChunkHash::of(chunk), tokens: matrix.nrows(), data })
            .collect();
        store.insert_vectors(&vectors).unwrap();
    }

    #[test]
    fn top_k_ranks_by_max_sim_and_keeps_k() {
        let mut store = TempStore::new();
        let documents = [
            ("worst", array![[-1.0, 0.0]]),
            ("best", array![[0.0, 1.0], [1.0, 0.0]]),
            ("middle", array![[0.6, 0.8]]),
            ("empty", Array2::zeros((0, 2))),
        ];
        with_vectors(&mut store, &documents);
        let query = array![[1.0, 0.0]];
        assert_eq!(top_k(&store, &Filter::default(), &query, 10).unwrap(), hashes(&["best", "middle", "empty", "worst"]));
        assert_eq!(top_k(&store, &Filter::default(), &query, 2).unwrap(), hashes(&["best", "middle"]));
        assert_eq!(top_k(&store, &Filter::default(), &query, 0).unwrap(), Vec::<ChunkHash>::new());
    }

    #[test]
    fn top_k_skips_vectors_of_another_dimension() {
        let mut store = TempStore::new();
        with_vectors(&mut store, &[("narrow", array![[1.0, 0.0]]), ("wide", array![[1.0, 0.0, 0.0]])]);
        assert_eq!(top_k(&store, &Filter::default(), &array![[1.0, 0.0]], 10).unwrap(), hashes(&["narrow"]));
        assert_eq!(top_k(&store, &Filter::default(), &array![[1.0, 0.0, 0.0]], 10).unwrap(), hashes(&["wide"]));
    }

    #[test]
    fn fusion_breaks_ties_by_hash() {
        let [low, high] = [1, 2].map(ChunkHash::filled);
        assert_eq!(fuse(&[high], &[low]), [low, high]);
        assert_eq!(fuse(&[low], &[high]), [low, high]);
        assert_eq!(fuse(&[high, low], &[low, high]), [low, high]);
    }

    #[test]
    fn fusion_with_an_empty_list_keeps_the_other_ranking() {
        let ranked = [3, 1, 2].map(ChunkHash::filled);
        assert_eq!(fuse(&ranked, &[]), ranked);
        assert_eq!(fuse(&[], &ranked), ranked);
        assert_eq!(fuse(&[], &[]), Vec::<ChunkHash>::new());
    }

    #[test]
    fn fts_query_quotes_terms_and_drops_short_ones() {
        assert_eq!(fts_query("is \"Ident.php\" ok?").as_deref(), Some("\"Ident.php\""));
        assert_eq!(fts_query("a b"), None);
    }

    #[test]
    fn fts_query_doubles_quotes_inside_a_term() {
        assert_eq!(fts_query("say\"hi\"there").as_deref(), Some("\"say\"\"hi\"\"there\""));
    }

    #[test]
    fn fts_query_counts_characters_not_bytes() {
        assert_eq!(fts_query("Größe überprüfen").as_deref(), Some("\"Größe\" OR \"überprüfen\""));
        assert_eq!(fts_query("日本語 äö").as_deref(), Some("\"日本語\""));
    }

    #[test]
    fn fts_query_ignores_terms_that_are_only_punctuation() {
        assert_eq!(fts_query("--- ... !!! \"\""), None);
        assert_eq!(fts_query("(token) --refresh--").as_deref(), Some("\"token\" OR \"refresh\""));
    }

    fn indexed() -> (TempStore, FakeEncoder) {
        let mut store = TempStore::new();
        let old = Transcript::new("/p")
            .at("2026-01-01T10:00:00.000Z")
            .user("the token refresh fails after an hour")
            .assistant("o1", vec![text("the clock skew breaks the expiry check")])
            .user("thanks that fixed it");
        let new = Transcript::new("/p")
            .at("2026-01-02T10:00:00.000Z")
            .user("set up the continuous integration workflow")
            .assistant("n1", vec![text("added a workflow that runs the tests")])
            .user("the token refresh fails after an hour");
        let cake = Transcript::new("/p")
            .at("2026-01-03T10:00:00.000Z")
            .user("bake a chocolate cake")
            .assistant("c1", vec![text("mix flour and sugar")]);
        store.add("-p", "old", &old);
        store.add("-p", "new", &new);
        store.add("-p", "cake", &cake);
        let encoder = FakeEncoder::new();
        pipeline::embed_pending(&mut store, &encoder, |_| {}).unwrap();
        (store, encoder)
    }

    #[test]
    fn find_ranks_shared_words_first_and_shows_repeated_text_where_it_is_latest() {
        let (store, encoder) = indexed();
        let groups = find(&store, &Filter::default(), &encoder, "token refresh expiry", window(2, 0, 0)).unwrap();
        assert_eq!(summary(&groups), [("new".to_owned(), vec![(2, 2..3)]), ("old".to_owned(), vec![(1, 1..2)])]);
        assert_eq!(groups[0].passages[2].text, "the token refresh fails after an hour");
        assert_eq!(groups[0].hits[0].elsewhere, 1);
        assert_eq!(groups[1].passages[1].text, "the clock skew breaks the expiry check");
        assert_eq!(groups[1].hits[0].elsewhere, 0);
    }

    #[test]
    fn find_shows_context_around_each_match() {
        let (store, encoder) = indexed();
        let groups = find(&store, &Filter::default(), &encoder, "token refresh expiry", window(2, 1, 1)).unwrap();
        assert_eq!(summary(&groups), [("new".to_owned(), vec![(2, 1..3)]), ("old".to_owned(), vec![(1, 0..3)])]);
    }

    #[test]
    fn find_ranks_by_meaning_without_a_lexical_match() {
        let (store, encoder) = indexed();
        // Every word is shorter than three characters, so there is no text search, only the vectors.
        let groups = find(&store, &Filter::default(), &encoder, "a cake", window(1, 0, 0)).unwrap();
        assert_eq!(summary(&groups), [("cake".to_owned(), vec![(0, 0..1)])]);
    }
}
