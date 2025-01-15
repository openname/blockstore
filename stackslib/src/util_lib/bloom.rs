// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020-2021 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use std::cmp;
use std::collections::HashMap;
use std::hash::Hasher;
use std::io::{Read, Seek, SeekFrom, Write};

use rand::prelude::*;
use rand::thread_rng;
use rusqlite::blob::Blob;
use rusqlite::types::ToSql;
use rusqlite::{params, Error as sqlite_error, Row};
use siphasher::sip::SipHasher; // this is SipHash-2-4
use stacks_common::codec::{read_next, write_next, Error as codec_error, StacksMessageCodec};
use stacks_common::types::sqlite::NO_PARAMS;
use stacks_common::util::hash::{to_hex, Sha512Trunc256Sum};

use crate::util_lib::db::{query_expect_row, DBConn, DBTx, Error as db_error};

/// A field of bits of known length!
#[derive(Debug, Clone, PartialEq)]
struct BitField(Vec<u8>, u32);

impl BitField {
    /// Make a new bitfield with sz bits represented (rounded up to the nearest byte in space used)
    pub fn new(sz: u32) -> BitField {
        BitField(vec![0u8; BITVEC_LEN!(sz) as usize], sz)
    }

    pub fn num_bits(&self) -> u32 {
        self.1
    }

    pub fn test(&self, bit: u32) -> bool {
        if bit >= self.1 {
            panic!("Attempted to read beyind end of bitfield");
        }
        self.0[(bit / 8) as usize] & (1u8 << ((bit % 8) as u8)) != 0
    }

    pub fn set(&mut self, bit: u32) {
        if bit >= self.1 {
            panic!("Attempted to write beyond end of bitfield");
        }
        self.0[(bit / 8) as usize] |= 1u8 << ((bit % 8) as u8);
    }

    pub fn clear(&mut self, bit: u32) {
        if bit >= self.1 {
            panic!("Attempted to write beyond end of bitfield");
        }
        self.0[(bit / 8) as usize] &= !(1u8 << ((bit % 8) as u8));
    }
}

/// Codec enum for how a bloom filter bitfield's fields are encoded
#[repr(u8)]
#[derive(Debug, Clone, PartialEq)]
enum BitFieldEncoding {
    Sparse = 0x01,
    Full = 0x02,
}

/// Encode the inner count array, using a sparse representation if it would save space
fn encode_bitfield<W: Write>(fd: &mut W, bytes: &[u8]) -> Result<(), codec_error> {
    let mut num_filled = 0;
    for bits in bytes.iter() {
        if *bits > 0 {
            num_filled += 1;
        }
    }

    if num_filled * 5 + 4 < bytes.len() {
        // more efficient to encode as (4-byte-index, 1-byte-value) pairs, with an extra 4-byte header
        write_next(fd, &(BitFieldEncoding::Sparse as u8))?;
        write_next(fd, &(bytes.len() as u32))?;
        write_next(fd, &(num_filled as u32))?;
        for (i, bits) in bytes.iter().enumerate() {
            if *bits > 0 {
                write_next(fd, &(i as u32))?;
                write_next(fd, bits)?;
            }
        }
    } else {
        // more efficient to encode as-is
        // (note that the array has a 4-byte length prefix)
        write_next(fd, &(BitFieldEncoding::Full as u8))?;
        write_next(fd, &bytes.to_vec())?;
    }
    Ok(())
}

/// Decode the inner count array, depending on whether or not it's sparse
fn decode_bitfield<R: Read>(fd: &mut R) -> Result<Vec<u8>, codec_error> {
    let encoding: u8 = read_next(fd)?;
    match encoding {
        x if x == BitFieldEncoding::Sparse as u8 => {
            // sparse encoding
            let vec_len: u32 = read_next(fd)?;
            let num_filled: u32 = read_next(fd)?;

            let mut ret = vec![0u8; vec_len as usize];
            for _ in 0..num_filled {
                let idx: u32 = read_next(fd)?;
                if idx >= vec_len {
                    return Err(codec_error::DeserializeError(format!(
                        "Index overflow: {} >= {}",
                        idx, vec_len
                    )));
                }
                let value: u8 = read_next(fd)?;
                ret[idx as usize] = value;
            }

            Ok(ret)
        }
        x if x == BitFieldEncoding::Full as u8 => {
            // full encoding
            let ret: Vec<u8> = read_next(fd)?;
            Ok(ret)
        }
        _ => Err(codec_error::DeserializeError(format!(
            "Unrecognized bloom count encoding: {}",
            encoding
        ))),
    }
}

impl StacksMessageCodec for BitField {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        write_next(fd, &self.1)?;
        // no need to write the array length prefix -- we already know it, per the above
        encode_bitfield(fd, &self.0)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<BitField, codec_error> {
        let num_bits: u32 = read_next(fd)?;
        let bits: Vec<u8> = decode_bitfield(fd)?;
        Ok(BitField(bits, num_bits))
    }
}

/// A node-specific collection of Bloom function hashes.
/// Works by using a node-local salt to ensure that the hash functions used to insert data into the
/// bloom structure will be unique (w.h.p.) to this node.
#[derive(Debug, Clone, PartialEq)]
pub struct BloomNodeHasher {
    seed: [u8; 32],
}

impl std::fmt::Display for BloomNodeHasher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(f, "BloomNodeHasher({})", to_hex(&self.seed))
    }
}

impl BloomNodeHasher {
    pub fn new(node_seed: &[u8]) -> BloomNodeHasher {
        let seed = Sha512Trunc256Sum::from_data(node_seed).0;
        BloomNodeHasher { seed }
    }

    pub fn new_random() -> BloomNodeHasher {
        let mut seed = [0u8; 32];
        thread_rng().fill(&mut seed[..]);
        BloomNodeHasher::new(&seed)
    }
}

impl StacksMessageCodec for BloomNodeHasher {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        write_next(fd, &(BloomHashID::BloomNodeHasher as u8))?;
        write_next(fd, &self.seed)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(fd: &mut R) -> Result<BloomNodeHasher, codec_error> {
        let hasher_type_u8: u8 = read_next(fd)?;
        match hasher_type_u8 as u8 {
            x if x == BloomHashID::BloomNodeHasher as u8 => {
                let seed: [u8; 32] = read_next(fd)?;
                Ok(BloomNodeHasher { seed })
            }
            _ => Err(codec_error::DeserializeError(format!(
                "Not a supported bloom hasher type ID: {}",
                hasher_type_u8
            ))),
        }
    }
}

/// A trait for picking a bin that will be set in a bloom struct
pub trait BloomHash {
    fn get_seed(&self) -> &[u8; 32];
    fn pick_bin(&self, count: u32, data: &[u8], num_bins: u32) -> u32;
}

/// Basic bloom filter with a given hash implementation that can suitably provide a given number of
/// distinct hash functions.
#[derive(Debug, Clone, PartialEq)]
pub struct BloomFilter<H: BloomHash> {
    hasher: H,
    bits: BitField,
    num_hashes: u32,
}

impl std::fmt::Display for BloomFilter<BloomNodeHasher> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> Result<(), std::fmt::Error> {
        write!(
            f,
            "BloomFilter({},nbits={},bits={})",
            &self.hasher,
            self.bits.1,
            Sha512Trunc256Sum::from_data(&self.bits.0)
        )
    }
}

/// Parameter calculation for bloom filters.
/// Returns (number of bins, number of hash functions)
fn bloom_hash_count(error_rate: f64, max_items: u32) -> (u32, u32) {
    // https://stackoverflow.com/questions/658439/how-many-hash-functions-does-my-bloom-filter-need
    let num_slots =
        (((-(max_items as f64)) * error_rate.ln()) / (2.0f64.ln() * 2.0f64.ln())).ceil() as u32;
    let num_hashes = ((num_slots as f64) / (max_items as f64) * 2.0f64.ln()).round() as u32;
    (num_slots, num_hashes)
}

/// Codec enum for the types of hashers we support
#[repr(u8)]
#[derive(Debug, Clone, PartialEq)]
enum BloomHashID {
    BloomNodeHasher = 0x01,
}

impl<H: BloomHash> BloomFilter<H> {
    /// Make a new bloom filter with a given error rate and expected maximum size
    pub fn new(error_rate: f64, max_items: u32, hasher: H) -> BloomFilter<H> {
        let (num_bits, num_hashes) = bloom_hash_count(error_rate, max_items);
        BloomFilter {
            hasher,
            bits: BitField::new(num_bits),
            num_hashes,
        }
    }

    /// Add a raw item, represented as a byte array (e.g. a serialized struct, perhaps)
    pub fn insert_raw(&mut self, item: &[u8]) -> bool {
        let mut false_positive = true;
        for i in 0..self.num_hashes {
            let slot = self.hasher.pick_bin(i, item, self.bits.num_bits());
            assert!(
                slot < self.bits.num_bits(),
                "BUG: hasher selected a slot outside the bitfield: {}",
                slot
            );

            if false_positive && !self.bits.test(slot) {
                false_positive = false;
            }

            self.bits.set(slot);
        }
        false_positive
    }

    /// Test to see if a given item (a byte array) is likely present
    pub fn contains_raw(&self, item: &[u8]) -> bool {
        for i in 0..self.num_hashes {
            let slot = self.hasher.pick_bin(i, item, self.bits.num_bits());
            assert!(
                slot < self.bits.num_bits(),
                "BUG: hasher selected a slot outside the bitfield: {}",
                slot
            );

            if !self.bits.test(slot) {
                // definitely not here
                return false;
            }
        }
        true
    }
}

impl StacksMessageCodec for BloomFilter<BloomNodeHasher> {
    fn consensus_serialize<W: Write>(&self, fd: &mut W) -> Result<(), codec_error> {
        write_next(fd, &(BloomHashID::BloomNodeHasher as u8))?;
        write_next(fd, &self.hasher.seed)?;
        write_next(fd, &self.num_hashes)?;
        write_next(fd, &self.bits)?;
        Ok(())
    }

    fn consensus_deserialize<R: Read>(
        fd: &mut R,
    ) -> Result<BloomFilter<BloomNodeHasher>, codec_error> {
        let hasher_type_u8: u8 = read_next(fd)?;
        match hasher_type_u8 as u8 {
            x if x == BloomHashID::BloomNodeHasher as u8 => {
                let seed: [u8; 32] = read_next(fd)?;
                let num_hashes: u32 = read_next(fd)?;
                let bits: BitField = read_next(fd)?;
                Ok(BloomFilter {
                    hasher: BloomNodeHasher { seed },
                    bits,
                    num_hashes,
                })
            }
            _ => Err(codec_error::DeserializeError(format!(
                "Not a supported bloom hasher type ID: {}",
                hasher_type_u8
            ))),
        }
    }
}

/// Disk-backed counting bloom filter with a given set of hash functions.  Uses a sqlite3 blob of
/// 32-bit bins to count things.  Meant to work alongside an existing database, in its own table
/// (e.g. the mempool).
#[derive(Debug, Clone, PartialEq)]
pub struct BloomCounter<H: BloomHash + Clone + StacksMessageCodec> {
    hasher: H,
    table_name: String,
    num_bins: u32,
    num_hashes: u32,
    counts_rowid: u32,
}

impl<H: BloomHash + Clone + StacksMessageCodec> BloomCounter<H> {
    /// Make a new bloom counter with the given error rate and expected number of items
    pub fn new(
        tx: &mut DBTx,
        table_name: &str,
        error_rate: f64,
        max_items: u32,
        hasher: H,
    ) -> Result<BloomCounter<H>, db_error> {
        let sql = format!("CREATE TABLE IF NOT EXISTS {table_name}(counts BLOB NOT NULL, num_bins INTEGER NOT NULL, num_hashes INTEGER NOT NULL, hasher BLOB NOT NULL);");
        tx.execute(&sql, NO_PARAMS).map_err(db_error::SqliteError)?;

        let (num_bins, num_hashes) = bloom_hash_count(error_rate, max_items);
        let counts_vec = vec![0u8; (num_bins * 4) as usize];
        let hasher_vec = hasher.serialize_to_vec();

        let sql = format!(
            "INSERT INTO {} (counts, num_bins, num_hashes, hasher) VALUES (?1, ?2, ?3, ?4)",
            table_name
        );
        let args = params![counts_vec, num_bins, num_hashes, hasher_vec];

        tx.execute(&sql, args).map_err(db_error::SqliteError)?;

        let sql = format!("SELECT rowid FROM {table_name}");
        let counts_rowid: u64 = query_expect_row(tx, &sql, NO_PARAMS)?
            .expect("BUG: inserted bloom counter but can't find row ID");

        Ok(BloomCounter {
            hasher,
            table_name: table_name.to_string(),
            num_bins,
            num_hashes,
            counts_rowid: counts_rowid as u32,
        })
    }

    pub fn try_load(conn: &DBConn, table_name: &str) -> Result<Option<BloomCounter<H>>, db_error> {
        let sql = format!("SELECT rowid,* FROM {table_name}");
        let result = conn.query_row_and_then(&sql, NO_PARAMS, |row| {
            let mut hasher_blob = row
                .get_ref("hasher")?
                .as_blob()
                .expect("Unable to read hasher as blob");
            let hasher =
                H::consensus_deserialize(&mut hasher_blob).map_err(|_| db_error::ParseError)?;
            let num_bins: u32 = row.get_unwrap("num_bins");
            let num_hashes: u32 = row.get_unwrap("num_hashes");
            let counts_rowid: u32 = row.get_unwrap("rowid");
            Ok(BloomCounter {
                hasher,
                table_name: table_name.to_string(),
                num_bins,
                num_hashes,
                counts_rowid,
            })
        });
        match result {
            Ok(x) => Ok(Some(x)),
            Err(db_error::SqliteError(sqlite_error::QueryReturnedNoRows)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn get_seed(&self) -> &[u8; 32] {
        self.hasher.get_seed()
    }

    /// Get a handle to the underlying bins list
    fn open_counts_blob<'a>(
        &self,
        conn: &'a DBConn,
        readwrite: bool,
    ) -> Result<Blob<'a>, db_error> {
        let blob = conn.blob_open(
            rusqlite::DatabaseName::Main,
            &self.table_name,
            "counts",
            self.counts_rowid.into(),
            !readwrite,
        )?;
        Ok(blob)
    }

    /// Get the 32-bit counter at a particular slot.  It's loaded from a big-endian representation
    /// within the readable handle, at offset 4*slot.
    fn get_counts_bin<R: Read + Seek>(counts_blob: &mut R, slot: u32) -> u32 {
        counts_blob
            .seek(SeekFrom::Start((slot as u64) * 4))
            .expect("BUG: failed to seek on counts blob");

        let mut bytes = [0u8; 4];
        counts_blob
            .read_exact(&mut bytes[..])
            .expect("BUG: failed to read from counts blob");

        u32::from_be_bytes(bytes)
    }

    /// Write the 32-bit counter at a particular slot.  It's stored in a big-endian representation
    /// within the writable handle, at offset 4*slot.
    fn set_counts_bin<W: Write + Seek>(counts_blob: &mut W, slot: u32, count: u32) {
        counts_blob
            .seek(SeekFrom::Start((slot as u64) * 4))
            .expect("BUG: failed to seek on counts blob");

        let bytes = count.to_be_bytes();
        counts_blob
            .write_all(&bytes)
            .expect("BUG: failed to write to counts blob");
    }

    /// Add a raw item to the bloom counter, and return the count it likely has (as an upper bound)
    /// Returns 0 if this item is absolutely new.
    /// Returns >0 if this item appears represented already.
    pub fn insert_raw(&self, tx: &mut DBTx, item: &[u8]) -> Result<u32, db_error> {
        let mut count = u32::MAX;
        let mut fd = self.open_counts_blob(tx, true)?;

        for i in 0..self.num_hashes {
            let slot = self.hasher.pick_bin(i, item, self.num_bins);
            assert!(
                slot < self.num_bins,
                "BUG: hasher selected a slot outside the bloom counters"
            );

            let bin = BloomCounter::<H>::get_counts_bin(&mut fd, slot);
            count = cmp::min(bin, count);
            BloomCounter::<H>::set_counts_bin(&mut fd, slot, bin.saturating_add(1));
        }

        Ok(count)
    }

    /// Return the upper bound on the number of times this item has been inserted.
    /// It will be 0 if it was never inserted (or was inserted and removed).
    pub fn count_raw(&self, conn: &DBConn, item: &[u8]) -> Result<u32, db_error> {
        let mut count = u32::MAX;
        let mut fd = self.open_counts_blob(conn, false)?;

        for i in 0..self.num_hashes {
            let slot = self.hasher.pick_bin(i, item, self.num_bins);
            assert!(
                slot < self.num_bins,
                "BUG: hasher selected a slot outside the bloom counters"
            );

            let bin = BloomCounter::<H>::get_counts_bin(&mut fd, slot);
            if bin == 0 {
                return Ok(0);
            } else {
                count = cmp::min(bin, count);
            }
        }
        Ok(count)
    }

    /// Remove an item from the bloom filter.  In order to use this correctly, you must ensure that
    /// it was actually inserted via insert_raw() earlier.  Returns the new lower bound on how many
    /// times this item was inserted.
    pub fn remove_raw(&self, tx: &mut DBTx, item: &[u8]) -> Result<u32, db_error> {
        if self.count_raw(tx, item)? == 0 {
            return Ok(0);
        }

        let mut count = u32::MAX;
        let mut fd = self.open_counts_blob(tx, true)?;

        for i in 0..self.num_hashes {
            let slot = self.hasher.pick_bin(i, item, self.num_bins);
            assert!(
                slot < self.num_bins,
                "BUG: hasher selected a slot outside the bloom counters"
            );

            let bin = BloomCounter::<H>::get_counts_bin(&mut fd, slot);
            if bin > 0 {
                let new_bin = bin - 1;
                BloomCounter::<H>::set_counts_bin(&mut fd, slot, new_bin);
                count = cmp::min(new_bin, count);
            }
        }

        Ok(count)
    }

    /// Extract a bloom filter from the bloom counter.
    /// There will be a 1-bit if the counter is positive
    pub fn to_bloom_filter(&self, conn: &DBConn) -> Result<BloomFilter<H>, db_error> {
        let new_hasher = self.hasher.clone();
        let mut bf = BitField::new(self.num_bins);

        let mut counts_blob = vec![0u8; (self.num_bins as usize) * 4];
        let mut fd = self.open_counts_blob(conn, false)?;

        fd.read_exact(&mut counts_blob).map_err(db_error::IOError)?;

        for i in 0..(self.num_bins as usize) {
            if counts_blob[4 * i] > 0
                || counts_blob[4 * i + 1] > 0
                || counts_blob[4 * i + 2] > 0
                || counts_blob[4 * i + 3] > 0
            {
                bf.set(i as u32);
            }
        }

        Ok(BloomFilter {
            hasher: new_hasher,
            bits: bf,
            num_hashes: self.num_hashes,
        })
    }
}

impl BloomHash for BloomNodeHasher {
    /// Pick a bin using the node seed and the count.
    /// Uses SipHash-2-4, with the count and seed used to set up the hash's initial state (thereby
    /// ensuring that a different initial state -- tantamount to a different hash function --
    /// will be used for each of the bloom struct's bins).
    /// A cryptographic hash isn't helpful here (and would be considerably slower), since the
    /// number of different bins is small enough that someone who's hell-bent on selecting items to
    /// create false positives would be able to do so no matter what we do (so why pay a
    /// performance penalty if it won't help?).
    fn pick_bin(&self, count: u32, data: &[u8], num_bins: u32) -> u32 {
        let mut initial_state = Vec::with_capacity(36 + data.len());
        initial_state.extend_from_slice(&count.to_be_bytes());
        initial_state.extend_from_slice(&self.seed);
        initial_state.extend_from_slice(data);

        let mut hasher = SipHasher::new();
        hasher.write(&initial_state);

        // be sure to remove modulus bias
        loop {
            let result_64 = hasher.finish();
            let result = (result_64 & 0x00000000ffffffff) as u32;
            if result < u32::MAX - (u32::MAX % num_bins) {
                return result % num_bins;
            } else {
                hasher.write_u64(result_64);
            }
        }
    }

    fn get_seed(&self) -> &[u8; 32] {
        &self.seed
    }
}

#[cfg(test)]
pub mod test {
    use std::fs;

    use rand::prelude::*;
    use rand::thread_rng;
    use rusqlite::OpenFlags;

    use super::*;
    use crate::util_lib::db::{sql_pragma, tx_begin_immediate, tx_busy_handler, DBConn, DBTx};

    pub fn setup_bloom_counter(db_name: &str) -> DBConn {
        let db_path = format!("/tmp/test_bloom_filter_{}.db", db_name);
        if fs::metadata(&db_path).is_ok() {
            fs::remove_file(&db_path).unwrap();
        }
        let open_flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;

        let conn = DBConn::open_with_flags(&db_path, open_flags).unwrap();

        conn.busy_handler(Some(tx_busy_handler)).unwrap();
        sql_pragma(&conn, "journal_mode", &"WAL".to_string()).unwrap();
        conn
    }

    #[test]
    fn test_bloom_hash_count() {
        // https://hur.st/bloomfilter/?n=8192&p=0.001&m=&k=8
        let (num_bits, num_hashes) = bloom_hash_count(0.001, 8192);
        assert_eq!(num_bits, 117_782);
        assert_eq!(num_hashes, 10);

        // https://hur.st/bloomfilter/?n=8192&p=1.0E-7&m=&k=
        let (num_bits, num_hashes) = bloom_hash_count(0.0000001, 8192);
        assert_eq!(num_bits, 274_823);
        assert_eq!(num_hashes, 23);
    }

    #[test]
    fn test_bloom_filter_has_all_inserted_items_with_error_rate() {
        let num_items = 8192;
        let err_rate = 0.001;

        let hasher = BloomNodeHasher::new(&[0u8; 32]);
        let mut bf = BloomFilter::new(err_rate, num_items, hasher);

        let mut fp_count = 0; // false positives

        for i in 0..num_items {
            let mut random_data = [0u8; 32];
            thread_rng().fill(&mut random_data[..]);

            if bf.contains_raw(&random_data) {
                fp_count += 1;
            }

            bf.insert_raw(&random_data);
            assert!(bf.contains_raw(&random_data));
        }

        let calculated_error_rate = (fp_count as f64) / (num_items as f64);
        eprintln!(
            "fp_count = {}, num_items = {}, err_rate = {}, calculated_error_rate = {}",
            fp_count, num_items, err_rate, calculated_error_rate
        );
        assert!(calculated_error_rate <= err_rate);
    }

    #[test]
    fn test_bloom_counter_has_all_inserted_items_with_error_rate() {
        let num_items = 8192;
        let err_rate = 0.001;

        let mut db = setup_bloom_counter(function_name!());
        let hasher = BloomNodeHasher::new(&[0u8; 32]);

        let bf = {
            let mut tx = tx_begin_immediate(&mut db).unwrap();
            let bf =
                BloomCounter::new(&mut tx, "bloom_counter", err_rate, num_items, hasher).unwrap();
            tx.commit().unwrap();
            bf
        };

        let mut fp_count = 0; // false positives

        let mut tx = tx_begin_immediate(&mut db).unwrap();
        for i in 0..num_items {
            let mut random_data = [0u8; 32];
            thread_rng().fill(&mut random_data[..]);

            if bf.count_raw(&tx, &random_data).unwrap() > 0 {
                fp_count += 1;
            }

            bf.insert_raw(&mut tx, &random_data).unwrap();
            assert!(bf.count_raw(&tx, &random_data).unwrap() > 0);
        }
        tx.commit().unwrap();

        let calculated_error_rate = (fp_count as f64) / (num_items as f64);
        eprintln!(
            "fp_count = {}, num_items = {}, err_rate = {}, calculated_error_rate = {}",
            fp_count, num_items, err_rate, calculated_error_rate
        );
        assert!(calculated_error_rate <= err_rate);
    }

    #[test]
    fn test_bloom_counter_is_invertible() {
        let num_items = 8192;
        let err_rate = 0.001;

        let mut db = setup_bloom_counter(function_name!());

        let hasher = BloomNodeHasher::new(&[0u8; 32]);

        let bf = {
            let mut tx = tx_begin_immediate(&mut db).unwrap();
            let bf =
                BloomCounter::new(&mut tx, "bloom_counter", err_rate, num_items, hasher).unwrap();
            tx.commit().unwrap();
            bf
        };

        let mut data = vec![];
        let mut fp_count = 0; // false positives

        let mut tx = tx_begin_immediate(&mut db).unwrap();
        for i in 0..num_items {
            let mut random_data = [0u8; 32];
            thread_rng().fill(&mut random_data[..]);

            if bf.count_raw(&tx, &random_data).unwrap() > 0 {
                fp_count += 1;
            }

            bf.insert_raw(&mut tx, &random_data).unwrap();
            assert!(bf.count_raw(&tx, &random_data).unwrap() > 0);

            data.push(random_data);
        }
        tx.commit().unwrap();

        let calculated_error_rate = (fp_count as f64) / (num_items as f64);
        eprintln!(
            "fp_count = {}, num_items = {}, err_rate = {}, calculated_error_rate = {}",
            fp_count, num_items, err_rate, calculated_error_rate
        );
        assert!(calculated_error_rate <= err_rate);

        let mut tx = tx_begin_immediate(&mut db).unwrap();

        fp_count = 0;
        for random_data in data.iter() {
            bf.remove_raw(&mut tx, random_data).unwrap();
            if bf.count_raw(&tx, random_data).unwrap() > 0 {
                fp_count += 1;
            }
        }
        tx.commit().unwrap();

        let calculated_error_rate = (fp_count as f64) / (num_items as f64);
        eprintln!(
            "fp_count = {}, num_items = {}, err_rate = {}, calculated_error_rate = {}",
            fp_count, num_items, err_rate, calculated_error_rate
        );
        assert!(calculated_error_rate <= err_rate);

        // everything is removed
        for random_data in data.iter() {
            assert_eq!(bf.count_raw(&db, random_data).unwrap(), 0);
        }
    }

    #[test]
    fn test_bloom_counter_is_invertible_over_iterations() {
        let num_items = 8192;
        let err_rate = 0.001;

        let mut db = setup_bloom_counter(function_name!());

        let hasher = BloomNodeHasher::new(&[0u8; 32]);

        let bf = {
            let mut tx = tx_begin_immediate(&mut db).unwrap();
            let bf =
                BloomCounter::new(&mut tx, "bloom_counter", err_rate, num_items, hasher).unwrap();
            tx.commit().unwrap();
            bf
        };

        let mut data = vec![];
        let mut fp_count = 0; // false positives
        let remove_delay = 2;

        for i in 0..(remove_delay * 10) {
            eprintln!("Add {} items for pass {}", num_items / remove_delay, i);
            let mut tx = tx_begin_immediate(&mut db).unwrap();
            for i in 0..(num_items / remove_delay) {
                let mut random_data = [0u8; 32];
                thread_rng().fill(&mut random_data[..]);

                if bf.count_raw(&tx, &random_data).unwrap() > 0 {
                    fp_count += 1;
                }

                bf.insert_raw(&mut tx, &random_data).unwrap();
                assert!(bf.count_raw(&tx, &random_data).unwrap() > 0);

                data.push(random_data);
            }
            tx.commit().unwrap();

            let calculated_error_rate = (fp_count as f64) / (num_items as f64);
            eprintln!(
                "fp_count = {}, num_items = {}, err_rate = {}, calculated_error_rate = {}",
                fp_count, num_items, err_rate, calculated_error_rate
            );
            assert!(calculated_error_rate <= err_rate);

            let mut tx = tx_begin_immediate(&mut db).unwrap();

            if i + 1 >= remove_delay {
                let remove_start = ((num_items / remove_delay) * (i + 1 - remove_delay)) as usize;
                let remove_end = remove_start + ((num_items / remove_delay) as usize);

                // this leaves $num_items in the bloom filter
                assert_eq!(data.len() - remove_start, num_items as usize);

                let remove_data = &data[remove_start..remove_end];
                eprintln!(
                    "Remove {} items from pass {}",
                    remove_data.len(),
                    i + 1 - remove_delay
                );
                fp_count = 0;
                for random_data in remove_data.iter() {
                    bf.remove_raw(&mut tx, random_data).unwrap();
                    if bf.count_raw(&tx, random_data).unwrap() > 0 {
                        fp_count += 1;
                    }
                }
                tx.commit().unwrap();

                let calculated_error_rate = (fp_count as f64) / (num_items as f64);
                eprintln!(
                    "fp_count = {}, num_items = {}, err_rate = {}, calculated_error_rate = {}",
                    fp_count, num_items, err_rate, calculated_error_rate
                );
                assert!(calculated_error_rate <= err_rate);

                // everything is removed, up to fp_rate
                let mut check_fp_count = 0;
                for random_data in remove_data.iter() {
                    if bf.count_raw(&db, random_data).unwrap() > 0 {
                        check_fp_count += 1;
                    }
                }
                assert!(check_fp_count <= fp_count);
            }
        }
    }

    #[test]
    fn test_bloom_bitfield_codec() {
        // aligned, full
        let bitfield = BitField(
            vec![
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0xff,
            ],
            128,
        );
        let bytes = bitfield.serialize_to_vec();
        assert_eq!(
            bytes,
            vec![
                0x00, 0x00, 0x00, 0x80, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x11, 0x22, 0x33, 0x44,
                0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff
            ]
        );

        assert_eq!(
            BitField::consensus_deserialize(&mut &bytes[..]).unwrap(),
            bitfield
        );

        // unaligned, full
        let bitfield = BitField(
            vec![
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
                0xee, 0x01,
            ],
            121,
        );
        let bytes = bitfield.serialize_to_vec();
        assert_eq!(
            bytes,
            vec![
                0x00, 0x00, 0x00, 0x79, 0x02, 0x00, 0x00, 0x00, 0x10, 0x00, 0x11, 0x22, 0x33, 0x44,
                0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0x01
            ]
        );

        assert_eq!(
            BitField::consensus_deserialize(&mut &bytes[..]).unwrap(),
            bitfield
        );

        // aligned, sparse
        let bitfield = BitField(
            vec![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x08,
            ],
            128,
        );
        let bytes = bitfield.serialize_to_vec();
        assert_eq!(
            bytes,
            vec![
                0x00, 0x00, 0x00, 0x80, 0x01, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00,
                0x00, 0x00, 0x0f, 0x08
            ]
        );

        // unaligned, sparse
        let bitfield = BitField(
            vec![
                0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                0x00, 0x08,
            ],
            121,
        );
        let bytes = bitfield.serialize_to_vec();
        assert_eq!(
            bytes,
            vec![
                0x00, 0x00, 0x00, 0x79, 0x01, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00,
                0x00, 0x00, 0x0f, 0x08
            ]
        );
    }

    #[test]
    fn test_bloom_filter_codec() {
        let num_items = 8192;
        let err_rate = 0.001;

        let hasher = BloomNodeHasher::new(&[0u8; 32]);
        let mut bf = BloomFilter::new(err_rate, num_items, hasher);

        for i in 0..num_items {
            let encoded_bf = bf.serialize_to_vec();
            let decoded_bf =
                BloomFilter::<BloomNodeHasher>::consensus_deserialize(&mut &encoded_bf[..])
                    .unwrap();
            assert_eq!(decoded_bf, bf);

            let mut random_data = [0u8; 32];
            thread_rng().fill(&mut random_data[..]);

            bf.insert_raw(&random_data);
            assert!(bf.contains_raw(&random_data));
        }
    }

    #[test]
    fn test_bloom_counter_to_filter() {
        let num_items = 8192;
        let err_rate = 0.001;

        let mut db = setup_bloom_counter(function_name!());

        let hasher = BloomNodeHasher::new(&[0u8; 32]);

        let bc = {
            let mut tx = tx_begin_immediate(&mut db).unwrap();
            let bc =
                BloomCounter::new(&mut tx, "bloom_counter", err_rate, num_items, hasher).unwrap();
            tx.commit().unwrap();
            bc
        };

        let mut tx = tx_begin_immediate(&mut db).unwrap();
        let mut data = vec![];
        for i in 0..num_items {
            let mut random_data = [0u8; 32];
            thread_rng().fill(&mut random_data[..]);

            bc.insert_raw(&mut tx, &random_data).unwrap();
            assert!(bc.count_raw(&tx, &random_data).unwrap() > 0);

            data.push(random_data);

            if i % 128 == 0 {
                let bf = bc.to_bloom_filter(&tx).unwrap();

                for random_data in data.iter() {
                    assert!(bf.contains_raw(random_data));
                }
            }
        }
        tx.commit().unwrap();
    }
}
