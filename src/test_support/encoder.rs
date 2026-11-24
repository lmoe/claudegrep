use std::cell::RefCell;

use anyhow::{Result, bail};
use ndarray::Array2;

use crate::embed::Encoder;

pub const DIM: usize = 32;

/// A hashed bag of words: one row per word, each the L2-normalised hash of the lowercased word. The same word always
/// gets the same row and different words get roughly orthogonal ones, so under `MaxSim` texts that share words score
/// higher than texts that don't.
pub struct FakeEncoder {
    id: String,
    fail_on: Option<String>,
    batches: RefCell<Vec<usize>>,
}

impl FakeEncoder {
    pub fn new() -> Self {
        Self::with_id("fake")
    }

    pub fn with_id(id: &str) -> Self {
        Self { id: id.to_owned(), fail_on: None, batches: RefCell::new(Vec::new()) }
    }

    pub fn failing_on(mut self, word: &str) -> Self {
        self.fail_on = Some(word.to_lowercase());
        self
    }

    pub fn batches(&self) -> Vec<usize> {
        self.batches.borrow().clone()
    }
}

impl Encoder for FakeEncoder {
    fn id(&self) -> &str {
        &self.id
    }

    fn encode_documents(&self, texts: &[&str]) -> Result<Vec<Array2<f32>>> {
        self.batches.borrow_mut().push(texts.len());
        if let Some(word) = &self.fail_on
            && texts.iter().any(|text| words(text).any(|w| w == *word))
        {
            bail!("the fake encoder fails on {word:?}");
        }
        Ok(texts.iter().map(|text| encode(text)).collect())
    }

    fn encode_query(&self, query: &str) -> Result<Array2<f32>> {
        Ok(encode(query))
    }
}

pub fn encode(text: &str) -> Array2<f32> {
    let rows: Vec<f32> = words(text).flat_map(|word| row(&word)).collect();
    Array2::from_shape_vec((rows.len() / DIM, DIM), rows).expect("whole rows")
}

fn words(text: &str) -> impl Iterator<Item = String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|word| !word.is_empty()).map(str::to_lowercase)
}

fn row(word: &str) -> [f32; DIM] {
    let values = blake3::hash(word.as_bytes()).as_bytes().map(|byte| f32::from(byte) - 127.5);
    let norm = values.iter().map(|x| x * x).sum::<f32>().sqrt();
    values.map(|x| x / norm)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_unit_row_per_word_and_the_same_row_for_the_same_word() {
        let matrix = encode("Alpha, beta: alpha!");
        assert_eq!(matrix.dim(), (3, DIM));
        assert_eq!(matrix.row(0), matrix.row(2));
        assert_ne!(matrix.row(0), matrix.row(1));
        for row in matrix.rows() {
            assert!((row.dot(&row) - 1.0).abs() < 1e-5);
        }
        assert_eq!(encode("alpha"), encode("ALPHA"));
    }

    #[test]
    fn text_without_words_has_no_rows() {
        assert_eq!(encode(" -- ").dim(), (0, DIM));
    }

    #[test]
    fn records_batches_and_fails_on_the_chosen_word() {
        let encoder = FakeEncoder::new().failing_on("Boom");
        assert_eq!(encoder.encode_documents(&["a b", "c"]).unwrap().len(), 2);
        assert!(encoder.encode_documents(&["fine", "boom here"]).is_err());
        assert_eq!(encoder.batches(), [2, 2]);
    }
}
