// Copyright 2019 PingCAP, Inc.
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

use std::convert::TryFrom;

use smallvec::SmallVec;

use cop_datatype::{EvalType, FieldTypeAccessor, FieldTypeFlag};
use tipb::schema::ColumnInfo;

use super::VectorValue;
use crate::coprocessor::codec::mysql::Tz;
use crate::coprocessor::codec::{datum, Error, Result};

pub enum LazyBatchColumn {
    /// Ensure that small datum values (i.e. Int, Real, Time) are stored compactly.
    /// Notice that there is an extra 1 byte for datum to store the flag, so there are 9 bytes.
    Raw(Vec<SmallVec<[u8; 9]>>),
    Decoded(VectorValue),
}

impl Clone for LazyBatchColumn {
    #[inline]
    fn clone(&self) -> Self {
        match self {
            LazyBatchColumn::Raw(v) => {
                // This is much more efficient than `SmallVec::clone`.
                let mut raw_vec = Vec::with_capacity(v.capacity());
                for d in v {
                    raw_vec.push(SmallVec::from_slice(d.as_slice()));
                }
                LazyBatchColumn::Raw(raw_vec)
            }
            LazyBatchColumn::Decoded(v) => LazyBatchColumn::Decoded(v.clone()),
        }
    }
}

impl LazyBatchColumn {
    /// Creates a new `LazyBatchColumn::Raw` with specified capacity.
    #[inline]
    pub fn raw_with_capacity(capacity: usize) -> Self {
        LazyBatchColumn::Raw(Vec::with_capacity(capacity))
    }

    #[inline]
    pub fn is_raw(&self) -> bool {
        match self {
            LazyBatchColumn::Raw(_) => true,
            LazyBatchColumn::Decoded(_) => false,
        }
    }

    #[inline]
    pub fn is_decoded(&self) -> bool {
        match self {
            LazyBatchColumn::Raw(_) => false,
            LazyBatchColumn::Decoded(_) => true,
        }
    }

    #[inline]
    pub fn get_decoded(&self) -> &VectorValue {
        match self {
            LazyBatchColumn::Raw(_) => panic!("LazyBatchColumn is not decoded"),
            LazyBatchColumn::Decoded(ref v) => v,
        }
    }

    #[inline]
    pub fn get_raw(&self) -> &Vec<SmallVec<[u8; 9]>> {
        match self {
            LazyBatchColumn::Raw(ref v) => v,
            LazyBatchColumn::Decoded(_) => panic!("LazyBatchColumn is already decoded"),
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        match self {
            LazyBatchColumn::Raw(ref v) => v.len(),
            LazyBatchColumn::Decoded(ref v) => v.len(),
        }
    }

    #[inline]
    pub fn truncate(&mut self, len: usize) {
        match self {
            LazyBatchColumn::Raw(ref mut v) => v.truncate(len),
            LazyBatchColumn::Decoded(ref mut v) => v.truncate(len),
        };
    }

    #[inline]
    pub fn clear(&mut self) {
        self.truncate(0)
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        match self {
            LazyBatchColumn::Raw(ref v) => v.capacity(),
            LazyBatchColumn::Decoded(ref v) => v.capacity(),
        }
    }

    #[inline]
    pub fn retain_by_index<F>(&mut self, mut f: F)
    where
        F: FnMut(usize) -> bool,
    {
        match self {
            LazyBatchColumn::Raw(ref mut v) => {
                let mut idx = 0;
                v.retain(|_| {
                    let r = f(idx);
                    idx += 1;
                    r
                });
            }
            LazyBatchColumn::Decoded(ref mut v) => {
                v.retain_by_index(f);
            }
        }
    }

    /// Decodes this column according to column info if the column is not decoded.
    pub fn decode(&mut self, time_zone: Tz, column_info: &ColumnInfo) -> Result<()> {
        if self.is_decoded() {
            return Ok(());
        }

        let eval_type =
            EvalType::try_from(column_info.tp()).map_err(|e| Error::Other(box_err!(e)))?;

        let mut decoded_column = VectorValue::with_capacity(self.capacity(), eval_type);
        {
            let raw_values = self.get_raw();
            for raw_value in raw_values {
                let raw_datum = if raw_value.is_empty() {
                    if column_info.has_default_val() {
                        column_info.get_default_val()
                    } else if !column_info.flag().contains(FieldTypeFlag::NOT_NULL) {
                        datum::DATUM_DATA_NULL
                    } else {
                        return Err(box_err!(
                            "Column (id = {}) has flag NOT NULL, but no value is given",
                            column_info.get_column_id()
                        ));
                    }
                } else {
                    raw_value.as_slice()
                };
                decoded_column.push_datum(raw_datum, time_zone, column_info)?;
            }
        }
        *self = LazyBatchColumn::Decoded(decoded_column);

        Ok(())
    }

    /// Push a raw datum which is not yet decoded.
    ///
    /// `raw_datum.len()` can be 0, indicating a missing value for corresponding cell.
    ///
    /// # Panics
    ///
    /// Panics when current column is already decoded.
    #[inline]
    pub fn push_raw(&mut self, raw_datum: impl AsRef<[u8]>) {
        match self {
            LazyBatchColumn::Raw(ref mut v) => {
                v.push(SmallVec::from_slice(raw_datum.as_ref()));
            }
            LazyBatchColumn::Decoded(_) => panic!("LazyBatchColumn is already decoded"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn test_lazy_batch_column_clone() {
        use cop_datatype::FieldTypeTp;

        let mut col_info = ColumnInfo::new();
        col_info.as_mut_accessor().set_tp(FieldTypeTp::Long);

        let mut col = LazyBatchColumn::raw_with_capacity(5);
        assert!(col.is_raw());
        assert_eq!(col.len(), 0);
        assert_eq!(col.capacity(), 5);
        assert_eq!(col.get_raw().len(), 0);
        {
            // Clone empty raw LazyBatchColumn.
            let col = col.clone();
            assert!(col.is_raw());
            assert_eq!(col.len(), 0);
            assert_eq!(col.capacity(), 5);
            assert_eq!(col.get_raw().len(), 0);
        }
        {
            // Empty raw to empty decoded.
            let mut col = col.clone();
            col.decode(Tz::utc(), &col_info).unwrap();
            assert!(col.is_decoded());
            assert_eq!(col.len(), 0);
            assert_eq!(col.capacity(), 5);
            assert_eq!(col.get_decoded().as_int_slice(), &[]);
            {
                // Clone empty decoded LazyBatchColumn.
                let col = col.clone();
                assert!(col.is_decoded());
                assert_eq!(col.len(), 0);
                assert_eq!(col.capacity(), 5);
                assert_eq!(col.get_decoded().as_int_slice(), &[]);
            }
        }

        let mut datum_raw_1 = Vec::new();
        DatumEncoder::encode(&mut datum_raw_1, &[Datum::U64(32)], false).unwrap();
        col.push_raw(&datum_raw_1);

        let mut datum_raw_2 = Vec::new();
        DatumEncoder::encode(&mut datum_raw_2, &[Datum::U64(7)], true).unwrap();
        col.push_raw(&datum_raw_2);

        assert!(col.is_raw());
        assert_eq!(col.len(), 2);
        assert_eq!(col.capacity(), 5);
        assert_eq!(col.get_raw().len(), 2);
        assert_eq!(col.get_raw()[0].as_slice(), datum_raw_1.as_slice());
        assert_eq!(col.get_raw()[1].as_slice(), datum_raw_2.as_slice());
        {
            // Clone non-empty raw LazyBatchColumn.
            let col = col.clone();
            assert!(col.is_raw());
            assert_eq!(col.len(), 2);
            assert_eq!(col.capacity(), 5);
            assert_eq!(col.get_raw().len(), 2);
            assert_eq!(col.get_raw()[0].as_slice(), datum_raw_1.as_slice());
            assert_eq!(col.get_raw()[1].as_slice(), datum_raw_2.as_slice());
        }
        // Non-empty raw to non-empty decoded.
        col.decode(Tz::utc(), &col_info).unwrap();
        assert!(col.is_decoded());
        assert_eq!(col.len(), 2);
        assert_eq!(col.capacity(), 5);
        assert_eq!(col.get_decoded().as_int_slice(), &[Some(32), Some(7)]);
        {
            // Clone non-empty decoded LazyBatchColumn.
            let col = col.clone();
            assert!(col.is_decoded());
            assert_eq!(col.len(), 2);
            assert_eq!(col.capacity(), 5);
            assert_eq!(col.get_decoded().as_int_slice(), &[Some(32), Some(7)]);
        }
    }
}

#[cfg(test)]
mod benches {
    use crate::test;

    use super::*;

    #[bench]
    fn bench_lazy_batch_column_push_raw_4bytes(b: &mut test::Bencher) {
        let mut column = LazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 4];
        b.iter(|| {
            let column = test::black_box(&mut column);
            for _ in 0..1000 {
                column.push_raw(test::black_box(&val))
            }
            test::black_box(&column);
            column.clear();
            test::black_box(&column);
        });
    }

    #[bench]
    fn bench_lazy_batch_column_push_raw_9bytes(b: &mut test::Bencher) {
        let mut column = LazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 9];
        b.iter(|| {
            let column = test::black_box(&mut column);
            for _ in 0..1000 {
                column.push_raw(test::black_box(&val))
            }
            test::black_box(&column);
            column.clear();
            test::black_box(&column);
        });
    }

    /// 10 bytes > inline size for LazyBatchColumn, which will be slower.
    /// This benchmark shows how slow it will be.
    #[bench]
    fn bench_lazy_batch_column_push_raw_10bytes(b: &mut test::Bencher) {
        let mut column = LazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 10];
        b.iter(|| {
            let column = test::black_box(&mut column);
            for _ in 0..1000 {
                column.push_raw(test::black_box(&val))
            }
            test::black_box(&column);
            column.clear();
            test::black_box(&column);
        });
    }

    /// Bench performance of cloning a raw column which size <= inline size.
    #[bench]
    fn bench_lazy_batch_column_clone(b: &mut test::Bencher) {
        let mut column = LazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 9];
        for _ in 0..1000 {
            column.push_raw(&val);
        }
        b.iter(|| {
            test::black_box(test::black_box(&column).clone());
        });
    }

    /// Bench performance of cloning a raw column which size > inline size.
    #[bench]
    fn bench_lazy_batch_column_clone_10bytes(b: &mut test::Bencher) {
        let mut column = LazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 10];
        for _ in 0..1000 {
            column.push_raw(&val);
        }
        b.iter(|| {
            test::black_box(test::black_box(&column).clone());
        });
    }

    /// Bench performance of naively cloning a raw column
    /// (which uses `SmallVec::clone()` instead of our own)
    #[bench]
    fn bench_lazy_batch_column_clone_naive(b: &mut test::Bencher) {
        let mut column = LazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 10];
        for _ in 0..1000 {
            column.push_raw(&val);
        }
        b.iter(|| match test::black_box(&column) {
            LazyBatchColumn::Raw(raw_vec) => {
                test::black_box(raw_vec.clone());
            }
            _ => panic!(),
        })
    }

    /// Bench performance of cloning a decoded column.
    #[bench]
    fn bench_lazy_batch_column_clone_decoded(b: &mut test::Bencher) {
        use crate::coprocessor::codec::datum::{Datum, DatumEncoder};
        use cop_datatype::FieldTypeTp;

        let mut column = LazyBatchColumn::raw_with_capacity(1000);

        let mut datum_raw: Vec<u8> = Vec::new();
        DatumEncoder::encode(&mut datum_raw, &[Datum::U64(0xDEADBEEF)], true).unwrap();

        for _ in 0..1000 {
            column.push_raw(datum_raw.as_slice());
        }

        let col_info = {
            let mut col_info = tipb::schema::ColumnInfo::new();
            col_info.as_mut_accessor().set_tp(FieldTypeTp::LongLong);
            col_info
        };
        let tz = Tz::utc();

        column.decode(tz, &col_info).unwrap();

        b.iter(|| {
            test::black_box(test::black_box(&column).clone());
        });
    }

    /// Bench performance of decoding a raw batch column.
    ///
    /// Note that there is a clone in the bench suite, whose cost should be excluded.
    #[bench]
    fn bench_lazy_batch_column_clone_and_decode(b: &mut test::Bencher) {
        use crate::coprocessor::codec::datum::{Datum, DatumEncoder};
        use cop_datatype::FieldTypeTp;

        let mut column = LazyBatchColumn::raw_with_capacity(1000);

        let mut datum_raw: Vec<u8> = Vec::new();
        DatumEncoder::encode(&mut datum_raw, &[Datum::U64(0xDEADBEEF)], true).unwrap();

        for _ in 0..1000 {
            column.push_raw(datum_raw.as_slice());
        }

        let col_info = {
            let mut col_info = tipb::schema::ColumnInfo::new();
            col_info.as_mut_accessor().set_tp(FieldTypeTp::LongLong);
            col_info
        };
        let tz = Tz::utc();

        b.iter(|| {
            let mut col = test::black_box(&column).clone();
            col.decode(test::black_box(tz), test::black_box(&col_info))
                .unwrap();
            test::black_box(&col);
        });
    }

    /// Bench performance of decoding a decoded lazy batch column.
    ///
    /// Note that there is a clone in the bench suite, whose cost should be excluded.
    #[bench]
    fn bench_lazy_batch_column_clone_and_decode_decoded(b: &mut test::Bencher) {
        use crate::coprocessor::codec::datum::{Datum, DatumEncoder};
        use cop_datatype::FieldTypeTp;

        let mut column = LazyBatchColumn::raw_with_capacity(1000);

        let mut datum_raw: Vec<u8> = Vec::new();
        DatumEncoder::encode(&mut datum_raw, &[Datum::U64(0xDEADBEEF)], true).unwrap();

        for _ in 0..1000 {
            column.push_raw(datum_raw.as_slice());
        }

        let col_info = {
            let mut col_info = tipb::schema::ColumnInfo::new();
            col_info.as_mut_accessor().set_tp(FieldTypeTp::LongLong);
            col_info
        };
        let tz = Tz::utc();

        column.decode(tz, &col_info).unwrap();

        b.iter(|| {
            let mut col = test::black_box(&column).clone();
            col.decode(test::black_box(tz), test::black_box(&col_info))
                .unwrap();
            test::black_box(&col);
        });
    }

    /// A vector based LazyBatchColumn
    #[derive(Clone)]
    enum VectorLazyBatchColumn {
        Raw(Vec<Vec<u8>>),
        Decoded(VectorValue),
    }

    impl VectorLazyBatchColumn {
        #[inline]
        pub fn raw_with_capacity(capacity: usize) -> Self {
            VectorLazyBatchColumn::Raw(Vec::with_capacity(capacity))
        }

        #[inline]
        pub fn clear(&mut self) {
            match self {
                VectorLazyBatchColumn::Raw(ref mut v) => v.clear(),
                VectorLazyBatchColumn::Decoded(ref mut v) => v.clear(),
            }
        }

        #[inline]
        pub fn push_raw(&mut self, raw_datum: &[u8]) {
            match self {
                VectorLazyBatchColumn::Raw(ref mut v) => v.push(raw_datum.to_vec()),
                VectorLazyBatchColumn::Decoded(_) => panic!(),
            }
        }
    }

    /// Bench performance of push 10 bytes to a vector based LazyBatchColumn.
    #[bench]
    fn bench_lazy_batch_column_by_vec_push_raw_10bytes(b: &mut test::Bencher) {
        let mut column = VectorLazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 10];
        b.iter(|| {
            let column = test::black_box(&mut column);
            for _ in 0..1000 {
                column.push_raw(test::black_box(&val))
            }
            test::black_box(&column);
            column.clear();
            test::black_box(&column);
        });
    }

    /// Bench performance of cloning a raw vector based LazyBatchColumn.
    #[bench]
    fn bench_lazy_batch_column_by_vec_clone(b: &mut test::Bencher) {
        let mut column = VectorLazyBatchColumn::raw_with_capacity(1000);
        let val = vec![0; 10];
        for _ in 0..1000 {
            column.push_raw(&val);
        }
        b.iter(|| {
            test::black_box(test::black_box(&column).clone());
        });
    }
}
