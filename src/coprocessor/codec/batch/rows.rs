// Copyright 2018 PingCAP, Inc.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// See the License for the specific language governing permissions and
// limitations under the License.

// TODO: Remove this.
#![allow(dead_code)]

use tipb::schema::ColumnInfo;

use super::{LazyBatchColumn, VectorValue};
use crate::coprocessor::codec::mysql::Tz;
use crate::coprocessor::codec::Result;

pub struct BatchRows<E> {
    /// Multiple interested columns. Each column is either decoded, or not decoded.
    ///
    /// For decoded columns, they may be in different types. If the column is in
    /// type `LazyBatchColumn::Encoded`, it means that it is not decoded.
    columns: Vec<LazyBatchColumn>,

    /// The optional error occurred during batch execution. Once there is an error, this field
    /// is set and no more rows should be fetched or executed.
    some_error: Option<E>,
}

impl<E: Clone> Clone for BatchRows<E> {
    #[inline]
    fn clone(&self) -> Self {
        Self {
            columns: self.columns.clone(),
            some_error: self.some_error.clone(),
        }
    }
}

impl<E> BatchRows<E> {
    /// Creates a new `BatchRows`, which contains `columns_count` number of raw columns
    /// and each of them reserves capacity according to `rows_capacity`.
    pub fn raw(columns_count: usize, rows_capacity: usize) -> Self {
        let mut columns = Vec::with_capacity(columns_count);
        for _ in 0..columns_count {
            let column = LazyBatchColumn::raw_with_capacity(rows_capacity);
            columns.push(column);
        }
        Self {
            columns,
            some_error: None,
        }
    }

    /// Pushes each raw datum into columns at corresponding index.
    ///
    /// If datum is missing, the corresponding element should be an empty slice.
    ///
    /// # Panics
    ///
    /// It is caller's duty to ensure that these columns are not decoded and the number of datums
    /// matches the number of columns. Otherwise there will be panics.
    #[inline]
    pub fn push_raw_row<D, V>(&mut self, raw_row: V)
    where
        D: AsRef<[u8]>,
        V: AsRef<[D]>,
    {
        let raw_row_slice = raw_row.as_ref();
        assert_eq!(self.columns_len(), raw_row_slice.len());
        for (col_index, raw_datum) in raw_row_slice.iter().enumerate() {
            let lazy_col = &mut self.columns[col_index];
            assert!(lazy_col.is_raw());
            lazy_col.push_raw(raw_datum);
        }
    }

    /// Ensures that a column at specified `column_index` is decoded and returns a reference
    /// to the decoded column.
    ///
    /// If the column is already decoded, this function does nothing.
    #[inline]
    pub fn ensure_column_decoded(
        &mut self,
        column_index: usize,
        time_zone: Tz,
        column_info: &ColumnInfo,
    ) -> Result<&VectorValue> {
        let number_of_rows = self.rows_len();
        let col = &mut self.columns[column_index];
        assert_eq!(col.len(), number_of_rows);
        col.decode(time_zone, column_info)?;
        Ok(col.get_decoded())
    }

    /// Returns whether column at specified `column_index` is decoded.
    #[inline]
    pub fn is_column_decoded(&self, column_index: usize) -> bool {
        self.columns[column_index].is_decoded()
    }

    /// Returns the number of columns.
    ///
    /// It might be possible that there is no row but multiple columns.
    #[inline]
    pub fn columns_len(&self) -> usize {
        self.columns.len()
    }

    /// Returns the number of rows.
    #[inline]
    pub fn rows_len(&self) -> usize {
        if self.columns.is_empty() {
            return 0;
        }
        self.columns[0].len()
    }

    /// Returns the number of rows this container can hold without reallocating.
    #[inline]
    pub fn rows_capacity(&self) -> usize {
        if self.columns.is_empty() {
            return 0;
        }
        self.columns[0].capacity()
    }

    /// Retains only the rows specified by the predicate, which accepts index only.
    ///
    /// In other words, remove all rows such that `f(row_index)` returns `false`.
    #[inline]
    pub fn retain_by_index<F>(&mut self, mut f: F)
    where
        F: FnMut(usize) -> bool,
    {
        if self.rows_len() == 0 {
            return;
        }

        let current_rows_len = self.rows_len();
        let mut new_rows = None;

        // We retain column by column to be efficient.
        for col in &mut self.columns {
            assert_eq!(col.len(), current_rows_len);
            col.retain_by_index(&mut f);

            match new_rows {
                None => {
                    new_rows = Some(col.len());
                }
                Some(rows) => {
                    assert_eq!(col.len(), rows);
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    use cop_datatype::{EvalType, FieldTypeAccessor};

    use crate::coprocessor::codec::datum::{Datum, DatumEncoder};

    /// Helper method to generate raw row ([u8] vector) from datum vector.
    fn raw_row_from_datums(datums: impl AsRef<[Option<Datum>]>, comparable: bool) -> Vec<Vec<u8>> {
        datums
            .as_ref()
            .iter()
            .map(|some_datum| {
                let mut ret = Vec::new();
                if some_datum.is_some() {
                    DatumEncoder::encode(
                        &mut ret,
                        &[some_datum.clone().take().unwrap()],
                        comparable,
                    )
                    .unwrap();
                }
                ret
            })
            .collect()
    }

    #[test]
    fn test_ensure_column_decoded() {
        use cop_datatype::FieldTypeTp;

        for comparable in &[true, false] {
            let schema = [
                {
                    // Column 1: Long, Default value 5
                    let mut default_value = Vec::new();
                    DatumEncoder::encode(&mut default_value, &[Datum::U64(5)], *comparable)
                        .unwrap();

                    let mut col_info = ColumnInfo::new();
                    col_info.as_mut_accessor().set_tp(FieldTypeTp::Long);
                    col_info.set_default_val(default_value);
                    col_info
                },
                {
                    // Column 2: Double, Default value NULL
                    let mut col_info = ColumnInfo::new();
                    col_info.as_mut_accessor().set_tp(FieldTypeTp::Double);
                    col_info
                },
                {
                    // Column 3: VarChar, Default value NULL
                    let mut col_info = ColumnInfo::new();
                    col_info.as_mut_accessor().set_tp(FieldTypeTp::VarChar);
                    col_info
                },
            ];

            let values = vec![
                vec![Some(Datum::U64(1)), Some(Datum::F64(1.0)), None],
                // None in Column 0 should be decoded to its default value 5.
                vec![None, Some(Datum::Null), Some(Datum::Bytes(vec![0u8, 2u8]))],
                // Null in Column 0 should be decoded to NULL.
                vec![Some(Datum::Null), Some(Datum::F64(3.0)), None],
                // None in Column 1 should be decoded to NULL because of no default value.
                vec![Some(Datum::U64(4)), None, Some(Datum::Null)],
            ];

            // Empty BatchRows
            let mut rows = BatchRows::<()>::raw(3, 1);
            assert_eq!(rows.rows_len(), 0);
            assert_eq!(rows.columns_len(), 3);

            for raw_row in values.iter().map(|r| raw_row_from_datums(r, *comparable)) {
                rows.push_raw_row(raw_row.iter());
            }
            assert_eq!(rows.rows_len(), values.len());

            // Decode Column Index 2
            assert!(!rows.is_column_decoded(2));
            {
                let col = rows
                    .ensure_column_decoded(2, Tz::utc(), &schema[2])
                    .unwrap();
                assert_eq!(col.len(), 4);
                assert_eq!(col.eval_type(), EvalType::Bytes);
                assert_eq!(
                    col.as_bytes_slice(),
                    &[None, Some(vec![0u8, 2u8]), None, None]
                );
            }
            // Decode a decoded column
            assert!(rows.is_column_decoded(2));
            {
                let col = rows
                    .ensure_column_decoded(2, Tz::utc(), &schema[2])
                    .unwrap();
                assert_eq!(col.len(), 4);
                assert_eq!(col.eval_type(), EvalType::Bytes);
                assert_eq!(
                    col.as_bytes_slice(),
                    &[None, Some(vec![0u8, 2u8]), None, None]
                );
            }
            assert!(rows.is_column_decoded(2));

            // Decode Column Index 0
            assert!(!rows.is_column_decoded(0));
            {
                let col = rows
                    .ensure_column_decoded(0, Tz::utc(), &schema[0])
                    .unwrap();
                assert_eq!(col.len(), 4);
                assert_eq!(col.eval_type(), EvalType::Int);
                assert_eq!(col.as_int_slice(), &[Some(1), Some(5), None, Some(4)]);
            }
            assert!(rows.is_column_decoded(0));

            // Decode Column Index 1
            assert!(!rows.is_column_decoded(1));
            {
                let col = rows
                    .ensure_column_decoded(1, Tz::utc(), &schema[1])
                    .unwrap();
                assert_eq!(col.len(), 4);
                assert_eq!(col.eval_type(), EvalType::Real);
                assert_eq!(col.as_real_slice(), &[Some(1.0), None, Some(3.0), None]);
            }
            assert!(rows.is_column_decoded(1));
        }
    }

    #[test]
    fn test_retain_by_index() {
        use cop_datatype::FieldTypeTp;

        let schema = [
            {
                // Column 1: Long, Default value 5
                let mut default_value = Vec::new();
                DatumEncoder::encode(&mut default_value, &[Datum::U64(5)], true).unwrap();

                let mut col_info = ColumnInfo::new();
                col_info.as_mut_accessor().set_tp(FieldTypeTp::Long);
                col_info.set_default_val(default_value);
                col_info
            },
            {
                // Column 2: Double
                let mut col_info = ColumnInfo::new();
                col_info.as_mut_accessor().set_tp(FieldTypeTp::Double);
                col_info
            },
        ];

        let mut rows = BatchRows::<()>::raw(2, 3);
        assert_eq!(rows.rows_len(), 0);
        assert_eq!(rows.columns_len(), 2);
        assert_eq!(rows.rows_capacity(), 3);
        rows.retain_by_index(|_| true);
        assert_eq!(rows.rows_len(), 0);
        assert_eq!(rows.columns_len(), 2);
        assert_eq!(rows.rows_capacity(), 3);
        rows.retain_by_index(|_| false);
        assert_eq!(rows.rows_len(), 0);
        assert_eq!(rows.columns_len(), 2);
        assert_eq!(rows.rows_capacity(), 3);

        rows.push_raw_row(raw_row_from_datums(&[None, Some(Datum::F64(1.3))], false));
        rows.push_raw_row(raw_row_from_datums(&[None, None], false));
        rows.push_raw_row(raw_row_from_datums(&[Some(Datum::U64(3)), None], true));
        rows.push_raw_row(raw_row_from_datums(
            &[Some(Datum::U64(3)), Some(Datum::F64(5.0))],
            false,
        ));
        rows.push_raw_row(raw_row_from_datums(
            &[Some(Datum::U64(11)), Some(Datum::F64(7.5))],
            true,
        ));
        rows.push_raw_row(raw_row_from_datums(&[None, Some(Datum::F64(13.1))], true));

        let retain_map = &[true, true, false, false, true, false];
        rows.retain_by_index(|idx| retain_map[idx]);

        assert_eq!(rows.rows_len(), 3);
        assert_eq!(rows.columns_len(), 2);
        assert!(rows.rows_capacity() > 3);
        {
            // Clone `rows` because `ensure_column_decoded` will mutate lazy column from raw bytes
            // to decoded permanently.
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 3);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(column0.as_int_slice(), &[Some(5), Some(5), Some(11)]);
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 3);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(column1.as_real_slice(), &[Some(1.3), None, Some(7.5)]);
        }

        rows.push_raw_row(raw_row_from_datums(
            &[None, Some(Datum::F64(101.51))],
            false,
        ));
        rows.push_raw_row(raw_row_from_datums(&[Some(Datum::U64(1)), None], false));
        rows.push_raw_row(raw_row_from_datums(
            &[Some(Datum::U64(5)), Some(Datum::F64(1.9))],
            true,
        ));
        rows.push_raw_row(raw_row_from_datums(
            &[None, Some(Datum::F64(101.51))],
            false,
        ));

        assert_eq!(rows.rows_len(), 7);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 7);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(
                column0.as_int_slice(),
                &[
                    Some(5),
                    Some(5),
                    Some(11),
                    Some(5),
                    Some(1),
                    Some(5),
                    Some(5)
                ]
            );
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 7);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(
                column1.as_real_slice(),
                &[
                    Some(1.3),
                    None,
                    Some(7.5),
                    Some(101.51),
                    None,
                    Some(1.9),
                    Some(101.51)
                ]
            );
        }

        let retain_map = &[true, false, true, false, false, true, true];
        rows.retain_by_index(|idx| retain_map[idx]);

        assert_eq!(rows.rows_len(), 4);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 4);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(
                column0.as_int_slice(),
                &[Some(5), Some(11), Some(5), Some(5)]
            );
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 4);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(
                column1.as_real_slice(),
                &[Some(1.3), Some(7.5), Some(1.9), Some(101.51)]
            );
        }

        rows.retain_by_index(|_| true);

        assert_eq!(rows.rows_len(), 4);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 4);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(
                column0.as_int_slice(),
                &[Some(5), Some(11), Some(5), Some(5)]
            );
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 4);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(
                column1.as_real_slice(),
                &[Some(1.3), Some(7.5), Some(1.9), Some(101.51)]
            );
        }

        rows.retain_by_index(|_| false);

        assert_eq!(rows.rows_len(), 0);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 0);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(column0.as_int_slice(), &[]);
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 0);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(column1.as_real_slice(), &[]);
        }

        rows.push_raw_row(raw_row_from_datums(&[None, Some(Datum::F64(7.77))], true));
        rows.push_raw_row(raw_row_from_datums(&[Some(Datum::U64(5)), None], false));
        rows.push_raw_row(raw_row_from_datums(
            &[Some(Datum::U64(1)), Some(Datum::F64(7.17))],
            false,
        ));

        assert_eq!(rows.rows_len(), 3);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 3);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(column0.as_int_slice(), &[Some(5), Some(5), Some(1)]);
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 3);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(column1.as_real_slice(), &[Some(7.77), None, Some(7.17)]);
        }

        // Let's change a column from lazy to decoded and test whether retain works
        rows.ensure_column_decoded(0, Tz::utc(), &schema[0])
            .unwrap();

        rows.retain_by_index(|_| true);

        assert_eq!(rows.rows_len(), 3);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 3);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(column0.as_int_slice(), &[Some(5), Some(5), Some(1)]);
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 3);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(column1.as_real_slice(), &[Some(7.77), None, Some(7.17)]);
        }

        let retain_map = &[true, false, true];
        rows.retain_by_index(|idx| retain_map[idx]);

        assert_eq!(rows.rows_len(), 2);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 2);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(column0.as_int_slice(), &[Some(5), Some(1)]);
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 2);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(column1.as_real_slice(), &[Some(7.77), Some(7.17)]);
        }

        rows.retain_by_index(|_| false);

        assert_eq!(rows.rows_len(), 0);
        assert_eq!(rows.columns_len(), 2);
        {
            let mut rows = rows.clone();
            let column0 = rows
                .ensure_column_decoded(0, Tz::utc(), &schema[0])
                .unwrap();
            assert_eq!(column0.len(), 0);
            assert_eq!(column0.eval_type(), EvalType::Int);
            assert_eq!(column0.as_int_slice(), &[]);
        }
        {
            let mut rows = rows.clone();
            let column1 = rows
                .ensure_column_decoded(1, Tz::utc(), &schema[1])
                .unwrap();
            assert_eq!(column1.len(), 0);
            assert_eq!(column1.eval_type(), EvalType::Real);
            assert_eq!(column1.as_real_slice(), &[]);
        }
    }
}
