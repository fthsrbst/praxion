pub fn percentile_i64(sorted: &[i64], p: f64) -> i64 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    let rank = (p * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

pub fn percentile_f64(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let n = sorted.len();
    let rank = (p * n as f64).ceil() as usize;
    let idx = rank.saturating_sub(1).min(n - 1);
    sorted[idx]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_percentile() {
        let v = vec![1i64, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile_i64(&v, 0.5), 5);
        assert_eq!(percentile_i64(&v, 1.0), 10);
    }

    #[test]
    fn empty() {
        let v: Vec<i64> = vec![];
        assert_eq!(percentile_i64(&v, 0.5), 0);
    }

    #[test]
    fn single() {
        let v = vec![42i64];
        assert_eq!(percentile_i64(&v, 0.5), 42);
        assert_eq!(percentile_i64(&v, 0.99), 42);
    }

    #[test]
    fn percentile_f64_test() {
        let mut v = vec![1.0f64, 2.0, 3.0, 4.0, 5.0];
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(percentile_f64(&v, 0.5), 3.0);
        assert_eq!(percentile_f64(&v, 1.0), 5.0);
    }
}