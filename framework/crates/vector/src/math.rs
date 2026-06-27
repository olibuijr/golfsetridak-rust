//! Vector math: cosine similarity and top-k ranking.
//!
//! Embeddings are plain `&[f32]`. We deliberately avoid a newtype here — the
//! storage layer hands us raw slices and the CLI wants zero ceremony.

/// Cosine similarity of two equal-length vectors, in `[-1.0, 1.0]`.
///
/// Edge cases (documented, never panic):
/// - **Mismatched lengths** → `0.0`. The vectors aren't comparable; treating
///   them as orthogonal is the safe, non-panicking choice for a search path.
/// - **Either vector is zero-norm** (all zeros / empty) → `0.0`, since cosine
///   is undefined when a magnitude is zero.
pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;
    for (&x, &y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }
    let sim = dot / (norm_a.sqrt() * norm_b.sqrt());
    // Floating-point error can nudge an identical-vector result just past 1.0;
    // clamp so callers can rely on the [-1, 1] contract.
    sim.clamp(-1.0, 1.0)
}

/// Rank `candidates` by cosine similarity to `query`, returning the top `k` as
/// `(id, score)` pairs in descending score order.
///
/// - Ties keep their original input order (the sort is stable).
/// - `k` larger than the candidate count returns all candidates.
/// - `k == 0` returns an empty vector.
pub fn rank(query: &[f32], candidates: &[(u64, Vec<f32>)], k: usize) -> Vec<(u64, f32)> {
    let mut scored: Vec<(u64, f32)> = candidates
        .iter()
        .map(|(id, vec)| (*id, cosine(query, vec)))
        .collect();
    // Stable sort by score descending. `total_cmp` gives a total order even if
    // a score were NaN (it can't be, given `cosine`'s guards) so we never panic.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    scored.truncate(k);
    scored
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_vectors_score_one() {
        let v = [1.0, 2.0, 3.0];
        assert!((cosine(&v, &v) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn orthogonal_vectors_score_zero() {
        assert!(cosine(&[1.0, 0.0], &[0.0, 1.0]).abs() < 1e-6);
    }

    #[test]
    fn opposite_vectors_score_minus_one() {
        assert!((cosine(&[1.0, 0.0], &[-1.0, 0.0]) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn zero_norm_scores_zero() {
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[1.0, 2.0], &[0.0, 0.0]), 0.0);
        assert_eq!(cosine(&[], &[]), 0.0);
    }

    #[test]
    fn mismatched_lengths_score_zero() {
        assert_eq!(cosine(&[1.0, 2.0, 3.0], &[1.0, 2.0]), 0.0);
    }

    #[test]
    fn cosine_stays_in_range() {
        let s = cosine(&[3.0, 3.0, 3.0], &[3.0, 3.0, 3.0]);
        assert!((-1.0..=1.0).contains(&s));
    }

    #[test]
    fn rank_orders_by_descending_similarity() {
        let query = vec![1.0, 0.0];
        let candidates = vec![
            (10, vec![0.0, 1.0]), // orthogonal -> 0.0
            (20, vec![1.0, 0.0]), // identical -> 1.0
            (30, vec![1.0, 1.0]), // 45deg     -> ~0.707
        ];
        let got = rank(&query, &candidates, 3);
        assert_eq!(got[0].0, 20);
        assert_eq!(got[1].0, 30);
        assert_eq!(got[2].0, 10);
    }

    #[test]
    fn rank_top_k_truncates() {
        let query = vec![1.0, 0.0];
        let candidates = vec![
            (10, vec![0.0, 1.0]),
            (20, vec![1.0, 0.0]),
            (30, vec![1.0, 1.0]),
        ];
        let got = rank(&query, &candidates, 2);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].0, 20);
        assert_eq!(got[1].0, 30);
    }

    #[test]
    fn rank_k_zero_is_empty() {
        let query = vec![1.0, 0.0];
        let candidates = vec![(1, vec![1.0, 0.0])];
        assert!(rank(&query, &candidates, 0).is_empty());
    }

    #[test]
    fn rank_k_larger_than_len_returns_all() {
        let query = vec![1.0, 0.0];
        let candidates = vec![(1, vec![1.0, 0.0]), (2, vec![0.0, 1.0])];
        let got = rank(&query, &candidates, 99);
        assert_eq!(got.len(), 2);
    }

    #[test]
    fn rank_ties_keep_input_order() {
        let query = vec![1.0, 0.0];
        // Three identical candidates all score 1.0; stable sort preserves order.
        let candidates = vec![
            (7, vec![1.0, 0.0]),
            (8, vec![1.0, 0.0]),
            (9, vec![1.0, 0.0]),
        ];
        let got = rank(&query, &candidates, 3);
        assert_eq!(got.iter().map(|(id, _)| *id).collect::<Vec<_>>(), [7, 8, 9]);
    }

    #[test]
    fn rank_empty_candidates() {
        assert!(rank(&[1.0, 0.0], &[], 5).is_empty());
    }
}
