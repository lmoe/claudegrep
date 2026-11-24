use anyhow::{Result, ensure};

use crate::embed::Encoder;
use crate::store::{Store, StoredVector};
use crate::vector;

const BATCH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    Reembedding,
    Started { total: usize },
    Embedded(usize),
}

pub fn embed_pending(store: &mut Store, encoder: &impl Encoder, mut progress: impl FnMut(Progress)) -> Result<usize> {
    if store.use_embedder(encoder.id())? {
        progress(Progress::Reembedding);
    }
    let mut pending = store.pending()?;
    pending.sort_by_key(|chunk| chunk.text.len());
    if pending.is_empty() {
        return Ok(0);
    }

    progress(Progress::Started { total: pending.len() });
    for batch in pending.chunks(BATCH) {
        let texts: Vec<&str> = batch.iter().map(|chunk| chunk.text.as_str()).collect();
        let embeddings = encoder.encode_documents(&texts)?;
        ensure!(embeddings.len() == texts.len(), "the encoder returned {} embeddings for {} texts", embeddings.len(), texts.len());
        let blobs: Vec<_> = embeddings.iter().map(vector::encode).collect();
        let vectors: Vec<_> = batch
            .iter()
            .zip(&embeddings)
            .zip(&blobs)
            .map(|((chunk, matrix), data)| StoredVector { hash: chunk.hash, tokens: matrix.nrows(), data })
            .collect();
        store.insert_vectors(&vectors)?;
        progress(Progress::Embedded(batch.len()));
    }
    Ok(pending.len())
}

#[cfg(test)]
mod tests {
    use ndarray::Array2;

    use super::*;
    use crate::store::Filter;
    use crate::test_support::encoder::FakeEncoder;
    use crate::test_support::lines::{Transcript, text};
    use crate::test_support::store::TempStore;

    fn session(store: &mut TempStore, id: &str, texts: &[String]) {
        let mut transcript = Transcript::new("/p");
        for (i, chunk) in texts.iter().enumerate() {
            transcript = if i % 2 == 0 { transcript.user(chunk) } else { transcript.assistant(&format!("a{i}"), vec![text(chunk)]) };
        }
        store.add("-p", id, &transcript);
    }

    fn texts(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("chunk number {i} {}", "x".repeat(i))).collect()
    }

    fn embed(store: &mut TempStore, encoder: &FakeEncoder) -> (Result<usize>, Vec<Progress>) {
        let mut events = Vec::new();
        let result = embed_pending(store, encoder, |event| events.push(event));
        (result, events)
    }

    #[test]
    fn every_pending_chunk_is_embedded_in_batches() {
        let mut store = TempStore::new();
        session(&mut store, "s1", &texts(2 * BATCH + 6));
        assert_eq!(store.pending().unwrap().len(), 2 * BATCH + 6);

        let encoder = FakeEncoder::new();
        assert_eq!(embed(&mut store, &encoder).0.unwrap(), 2 * BATCH + 6);
        assert_eq!(encoder.batches(), [BATCH, BATCH, 6]);
        assert_eq!(store.pending().unwrap(), []);
        assert_eq!(store.stats(&Filter::default()).unwrap().embedded, 2 * BATCH + 6);
    }

    #[test]
    fn progress_adds_up_to_the_total() {
        let mut store = TempStore::new();
        session(&mut store, "s1", &texts(BATCH + 1));
        let (result, events) = embed(&mut store, &FakeEncoder::new());
        assert_eq!(events, [Progress::Started { total: BATCH + 1 }, Progress::Embedded(BATCH), Progress::Embedded(1)]);
        assert_eq!(result.unwrap(), BATCH + 1);
    }

    #[test]
    fn a_second_run_embeds_nothing() {
        let mut store = TempStore::new();
        session(&mut store, "s1", &texts(3));
        embed(&mut store, &FakeEncoder::new()).0.unwrap();

        let encoder = FakeEncoder::new();
        let (result, events) = embed(&mut store, &encoder);
        assert_eq!(result.unwrap(), 0);
        assert_eq!(events, []);
        assert_eq!(encoder.batches(), Vec::<usize>::new());
    }

    #[test]
    fn another_encoder_drops_every_vector_and_embeds_again() {
        let mut store = TempStore::new();
        session(&mut store, "s1", &texts(3));
        embed(&mut store, &FakeEncoder::new()).0.unwrap();

        let (result, events) = embed(&mut store, &FakeEncoder::with_id("other"));
        assert_eq!(result.unwrap(), 3);
        assert_eq!(events[0], Progress::Reembedding);
        assert_eq!(store.embedder().unwrap().as_deref(), Some("other"));
        assert_eq!(store.stats(&Filter::default()).unwrap().embedded, 3);
    }

    #[test]
    fn text_shared_by_sessions_is_embedded_once() {
        let mut store = TempStore::new();
        session(&mut store, "s1", &texts(3));
        session(&mut store, "s2", &texts(4));
        let encoder = FakeEncoder::new();
        assert_eq!(embed(&mut store, &encoder).0.unwrap(), 4);
        assert_eq!(encoder.batches(), [4]);
    }

    struct MiscountingEncoder(isize);

    impl Encoder for MiscountingEncoder {
        fn id(&self) -> &'static str {
            "miscounting"
        }

        fn encode_documents(&self, texts: &[&str]) -> Result<Vec<Array2<f32>>> {
            let count = texts.len().saturating_add_signed(self.0);
            Ok(vec![Array2::zeros((1, 2)); count])
        }

        fn encode_query(&self, _query: &str) -> Result<Array2<f32>> {
            unreachable!("embedding encodes no queries")
        }
    }

    #[test]
    fn an_encoder_that_miscounts_is_an_error_and_stores_nothing_of_the_batch() {
        for (miscount, returned) in [(-1, 2), (1, 4)] {
            let mut store = TempStore::new();
            session(&mut store, "s1", &texts(3));
            let error = embed_pending(&mut store, &MiscountingEncoder(miscount), |_| {}).unwrap_err();
            assert_eq!(error.to_string(), format!("the encoder returned {returned} embeddings for 3 texts"));
            assert_eq!(store.stats(&Filter::default()).unwrap().embedded, 0);
            assert_eq!(store.pending().unwrap().len(), 3, "they stay pending for the next run");
        }
    }

    #[test]
    fn an_encoder_error_keeps_the_batches_stored_before_it() {
        let mut store = TempStore::new();
        // The longest text fails, so it comes last, in the second batch.
        let mut chunks = texts(BATCH + 5);
        chunks.push(format!("boom {}", "y".repeat(2 * BATCH)));
        session(&mut store, "s1", &chunks);

        let (result, events) = embed(&mut store, &FakeEncoder::new().failing_on("boom"));
        assert!(result.is_err());
        assert_eq!(events, [Progress::Started { total: BATCH + 6 }, Progress::Embedded(BATCH)]);
        assert_eq!(store.stats(&Filter::default()).unwrap().embedded, BATCH);
        assert_eq!(store.pending().unwrap().len(), 6);
    }
}
