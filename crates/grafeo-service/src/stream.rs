//! Row-batch iterator for streaming query results.
//!
//! Provides [`RowBatchIter`], which yields `&[Vec<Value>]` slices of
//! configurable batch size from a [`QueryResult`]'s rows.  Used by
//! transport adapters to encode and send results incrementally instead
//! of materializing the entire encoded response at once.

use grafeo_engine::database::QueryResult;

/// Default number of rows per streaming batch.
pub const DEFAULT_BATCH_SIZE: usize = 1000;

/// Iterator that yields row slices of `batch_size` from a `QueryResult`.
///
/// The final slice may be shorter than `batch_size`.  Empty results
/// yield zero batches.
pub struct RowBatchIter<'a> {
    rows: &'a [Vec<grafeo_common::Value>],
    batch_size: usize,
    offset: usize,
}

impl<'a> RowBatchIter<'a> {
    /// Creates a new batch iterator over the given result's rows.
    pub fn new(result: &'a QueryResult, batch_size: usize) -> Self {
        Self {
            rows: &result.rows,
            batch_size: batch_size.max(1),
            offset: 0,
        }
    }

    /// Total number of rows remaining.
    pub fn remaining(&self) -> usize {
        self.rows.len().saturating_sub(self.offset)
    }
}

impl<'a> Iterator for RowBatchIter<'a> {
    type Item = &'a [Vec<grafeo_common::Value>];

    fn next(&mut self) -> Option<Self::Item> {
        if self.offset >= self.rows.len() {
            return None;
        }
        let end = (self.offset + self.batch_size).min(self.rows.len());
        let slice = &self.rows[self.offset..end];
        self.offset = end;
        Some(slice)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let batches = self.remaining().div_ceil(self.batch_size);
        (batches, Some(batches))
    }
}

/// Extension trait for convenient batch iteration on `QueryResult`.
pub trait QueryResultExt {
    /// Returns an iterator yielding row slices of the given batch size.
    fn row_batches(&self, batch_size: usize) -> RowBatchIter<'_>;
}

impl QueryResultExt for QueryResult {
    fn row_batches(&self, batch_size: usize) -> RowBatchIter<'_> {
        RowBatchIter::new(self, batch_size)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grafeo_common::Value;
    use grafeo_common::types::LogicalType;

    fn make_result(num_rows: usize) -> QueryResult {
        QueryResult {
            columns: vec!["x".to_string()],
            column_types: vec![LogicalType::Int64],
            rows: (0..num_rows)
                .map(|i| vec![Value::Int64(i as i64)])
                .collect(),
            execution_time_ms: Some(1.0),
            rows_scanned: Some(num_rows as u64),
        }
    }

    #[test]
    fn empty_result_yields_no_batches() {
        let result = make_result(0);
        let batches: Vec<_> = result.row_batches(100).collect();
        assert!(batches.is_empty());
    }

    #[test]
    fn exact_multiple() {
        let result = make_result(10);
        let batches: Vec<_> = result.row_batches(5).collect();
        assert_eq!(batches.len(), 2);
        assert_eq!(batches[0].len(), 5);
        assert_eq!(batches[1].len(), 5);
    }

    #[test]
    fn partial_final_batch() {
        let result = make_result(7);
        let batches: Vec<_> = result.row_batches(3).collect();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 3);
        assert_eq!(batches[1].len(), 3);
        assert_eq!(batches[2].len(), 1);
    }

    #[test]
    fn batch_larger_than_rows() {
        let result = make_result(3);
        let batches: Vec<_> = result.row_batches(1000).collect();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 3);
    }

    #[test]
    fn size_hint_accurate() {
        let result = make_result(7);
        let iter = result.row_batches(3);
        assert_eq!(iter.size_hint(), (3, Some(3)));
    }

    #[test]
    fn remaining_decreases() {
        let result = make_result(10);
        let mut iter = result.row_batches(3);
        assert_eq!(iter.remaining(), 10);
        iter.next();
        assert_eq!(iter.remaining(), 7);
    }

    #[test]
    fn batch_size_zero_floors_to_one() {
        let result = make_result(3);
        let batches: Vec<_> = result.row_batches(0).collect();
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 1);
    }
}
