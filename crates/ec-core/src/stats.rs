//! Small timing summaries for the benchmark and the preview's read-out.

/// Collects durations in milliseconds and reports the median and 95th percentile.
#[derive(Default, Clone)]
pub struct Timings {
    ms: Vec<f64>,
}

impl Timings {
    pub fn push(&mut self, ms: f64) {
        self.ms.push(ms);
    }

    pub fn len(&self) -> usize {
        self.ms.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ms.is_empty()
    }

    fn sorted(&self) -> Vec<f64> {
        let mut v = self.ms.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        v
    }

    /// The value below which `p` (0..=1) of the samples fall. `None` when empty.
    pub fn percentile(&self, p: f64) -> Option<f64> {
        let v = self.sorted();
        if v.is_empty() {
            return None;
        }
        let i = ((v.len() - 1) as f64 * p.clamp(0.0, 1.0)).round() as usize;
        Some(v[i])
    }

    pub fn median(&self) -> Option<f64> {
        self.percentile(0.5)
    }

    pub fn p95(&self) -> Option<f64> {
        self.percentile(0.95)
    }

    /// Keep only the newest `n` samples, for a rolling read-out.
    pub fn keep_last(&mut self, n: usize) {
        if self.ms.len() > n {
            self.ms.drain(..self.ms.len() - n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_has_no_numbers() {
        let t = Timings::default();
        assert_eq!(t.median(), None);
        assert_eq!(t.p95(), None);
    }

    #[test]
    fn median_and_p95() {
        let mut t = Timings::default();
        for v in 1..=101 {
            t.push(v as f64);
        }
        assert_eq!(t.median(), Some(51.0));
        assert_eq!(t.p95(), Some(96.0));
        t.keep_last(10);
        assert_eq!(t.len(), 10);
        assert_eq!(t.percentile(0.0), Some(92.0));
    }
}
