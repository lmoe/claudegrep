//! The stored form of an embedding: its rows of `f32` as little-endian `f16`, row after row.

use half::f16;
use ndarray::Array2;

pub fn encode(matrix: &Array2<f32>) -> Vec<u8> {
    matrix.iter().flat_map(|&x| f16::from_f32(x).to_le_bytes()).collect()
}

pub fn decode(data: &[u8], tokens: usize, dim: usize) -> Option<Array2<f32>> {
    let values = data.as_chunks::<2>().0.iter().map(|&b| f16::from_le_bytes(b).to_f32()).collect();
    Array2::from_shape_vec((tokens, dim), values).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ndarray::array;

    #[test]
    fn round_trip_keeps_values_at_f16_precision() {
        let matrix = array![[0.5, -0.25, 1.0 / 3.0], [0.0, 1e-3, -7.75]];
        let decoded = decode(&encode(&matrix), 2, 3).unwrap();
        for (a, b) in matrix.iter().zip(&decoded) {
            assert!((a - b).abs() <= a.abs() * 1e-3, "{a} vs {b}");
        }
        assert!((decoded[[0, 0]] - 0.5).abs() < f32::EPSILON);
        assert!((decoded[[1, 2]] + 7.75).abs() < f32::EPSILON);
    }

    #[test]
    fn values_are_little_endian_f16_row_after_row() {
        assert_eq!(encode(&array![[1.0, -2.0]]), [0x00, 0x3c, 0x00, 0xc0]);
    }

    #[test]
    fn an_empty_matrix_is_no_bytes() {
        let empty = Array2::<f32>::zeros((0, 4));
        assert_eq!(encode(&empty), Vec::<u8>::new());
        assert_eq!(decode(&[], 0, 4).unwrap().dim(), (0, 4));
    }

    #[test]
    fn a_wrong_length_is_none() {
        let data = encode(&array![[0.5, -0.25]]);
        assert!(decode(&data, 1, 3).is_none());
        assert!(decode(&data, 2, 2).is_none());
        assert!(decode(&data[..3], 1, 2).is_none());
    }
}
