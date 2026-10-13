//! Shared bounded binary NBT parsing for the Mesher schematic readers.

use std::io::{Cursor, Read};

use flate2::read::GzDecoder;
use nucleation::formats::limits::DecodeLimits;
use quartz_nbt::{io::Flavor, NbtCompound};

pub(super) fn gzip_root(data: &[u8], limits: &DecodeLimits) -> Result<NbtCompound, String> {
    limits
        .check_input(data)
        .map_err(|error| error.to_string())?;
    let mut decoder = GzDecoder::new(data);
    let mut raw = Vec::new();
    let mut chunk = [0; 64 * 1024];
    loop {
        let count = decoder
            .read(&mut chunk)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        let next = raw
            .len()
            .checked_add(count)
            .ok_or("decompressed size overflow")?;
        if next > limits.max_decompressed_bytes {
            return Err("decompressed byte limit exceeded".into());
        }
        if next > raw.capacity() {
            let capacity = raw
                .capacity()
                .saturating_mul(2)
                .max(next)
                .min(limits.max_decompressed_bytes);
            raw.try_reserve_exact(capacity - raw.len())
                .map_err(|error| error.to_string())?;
        }
        raw.extend_from_slice(&chunk[..count]);
    }
    // Keep decompressed storage scoped to parsing so it is released before dense region allocation.
    raw_root(&raw, limits)
}

pub(super) fn binary_root(data: &[u8], limits: &DecodeLimits) -> Result<NbtCompound, String> {
    limits
        .check_input(data)
        .map_err(|error| error.to_string())?;
    if data.starts_with(&[0x1f, 0x8b]) {
        gzip_root(data, limits)
    } else {
        raw_root(data, limits)
    }
}

fn raw_root(data: &[u8], limits: &DecodeLimits) -> Result<NbtCompound, String> {
    if data.len() > limits.max_decompressed_bytes {
        return Err("decompressed byte limit exceeded".into());
    }
    validate_nbt(data, limits).map_err(str::to_string)?;
    quartz_nbt::io::read_nbt(&mut Cursor::new(data), Flavor::Uncompressed)
        .map(|(root, _)| root)
        .map_err(|error| error.to_string())
}

/// Validate NBT structure and declared bounds before `quartz_nbt` can allocate from unchecked lengths.
fn validate_nbt(bytes: &[u8], limits: &DecodeLimits) -> Result<(), &'static str> {
    struct Scan<'a> {
        rest: &'a [u8],
        limits: &'a DecodeLimits,
        nodes: usize,
    }
    impl Scan<'_> {
        fn take(&mut self, count: usize) -> Result<&[u8], &'static str> {
            if count > self.rest.len() {
                return Err("truncated NBT payload");
            }
            let (head, tail) = self.rest.split_at(count);
            self.rest = tail;
            Ok(head)
        }

        fn byte(&mut self) -> Result<u8, &'static str> {
            Ok(self.take(1)?[0])
        }

        fn string(&mut self) -> Result<(), &'static str> {
            let count = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as usize;
            if count > self.limits.max_nbt_string_bytes {
                return Err("NBT string limit exceeded");
            }
            self.take(count)?;
            Ok(())
        }

        fn count(&mut self) -> Result<usize, &'static str> {
            let count = i32::from_be_bytes(self.take(4)?.try_into().unwrap());
            if count < 0 || count as usize > self.limits.max_nbt_collection_items {
                return Err("NBT collection limit exceeded");
            }
            Ok(count as usize)
        }

        fn payload(&mut self, tag: u8, depth: usize) -> Result<(), &'static str> {
            self.nodes = self.nodes.checked_add(1).ok_or("NBT node count overflow")?;
            if depth > self.limits.max_nbt_depth || self.nodes > self.limits.max_nbt_nodes {
                return Err("NBT depth or node limit exceeded");
            }
            match tag {
                1 => {
                    self.take(1)?;
                }
                2 => {
                    self.take(2)?;
                }
                3 | 5 => {
                    self.take(4)?;
                }
                4 | 6 => {
                    self.take(8)?;
                }
                7 | 11 | 12 => {
                    let count = self.count()?;
                    let width = match tag {
                        7 => 1,
                        11 => 4,
                        _ => 8,
                    };
                    self.take(count.checked_mul(width).ok_or("NBT array size overflow")?)?;
                }
                8 => self.string()?,
                9 => {
                    let child = self.byte()?;
                    let count = self.count()?;
                    if child > 12 || (child == 0 && count != 0) {
                        return Err("invalid NBT list tag");
                    }
                    if count > self.limits.max_nbt_nodes.saturating_sub(self.nodes) {
                        return Err("NBT node limit exceeded");
                    }
                    for _ in 0..count {
                        self.payload(child, depth + 1)?;
                    }
                }
                10 => {
                    let mut count = 0usize;
                    loop {
                        let child = self.byte()?;
                        if child == 0 {
                            break;
                        }
                        count = count.checked_add(1).ok_or("NBT compound size overflow")?;
                        if count > self.limits.max_nbt_collection_items {
                            return Err("NBT collection limit exceeded");
                        }
                        self.string()?;
                        self.payload(child, depth + 1)?;
                    }
                }
                _ => return Err("invalid NBT tag"),
            }
            Ok(())
        }
    }
    let mut scan = Scan {
        rest: bytes,
        limits,
        nodes: 0,
    };
    if scan.byte()? != 10 {
        return Err("root NBT is not a compound");
    }
    scan.string()?;
    scan.payload(10, 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_nbt::{NbtList, NbtTag};

    fn encode(root: &NbtCompound) -> Vec<u8> {
        let mut bytes = Vec::new();
        quartz_nbt::io::write_nbt(&mut bytes, None, root, Flavor::Uncompressed).unwrap();
        bytes
    }

    #[test]
    fn structural_limits_reject_before_tree_allocation() {
        let mut root = NbtCompound::new();
        root.insert("text", "abc");
        let mut limits = super::super::preview_limits();
        limits.max_nbt_string_bytes = 2;
        assert!(binary_root(&encode(&root), &limits).is_err());

        root = NbtCompound::new();
        root.insert("list", NbtList::from(vec![NbtTag::Int(1), NbtTag::Int(2)]));
        limits = super::super::preview_limits();
        limits.max_nbt_nodes = 3;
        assert!(binary_root(&encode(&root), &limits).is_err());
        limits.max_nbt_nodes = 4;
        assert!(binary_root(&encode(&root), &limits).is_ok());
        limits.max_nbt_collection_items = 1;
        assert!(binary_root(&encode(&root), &limits).is_err());

        let mut child = NbtCompound::new();
        child.insert("nested", root);
        limits = super::super::preview_limits();
        limits.max_nbt_depth = 1;
        assert!(binary_root(&encode(&child), &limits).is_err());
    }

    #[test]
    fn gzip_expansion_and_truncated_lengths_fail_recoverably() {
        let limits = super::super::preview_limits();
        // ByteArray claiming i32::MAX bytes, with no payload.
        let bytes = [10, 0, 0, 7, 0, 1, b'a', 0x7f, 0xff, 0xff, 0xff];
        assert!(binary_root(&bytes, &limits).is_err());
        let mut root = NbtCompound::new();
        root.insert("bytes", NbtTag::ByteArray(vec![0; 1024]));
        let mut bytes = Vec::new();
        quartz_nbt::io::write_nbt(&mut bytes, None, &root, Flavor::GzCompressed).unwrap();
        let limits = DecodeLimits {
            max_decompressed_bytes: 1024,
            ..limits
        };
        assert!(gzip_root(&bytes, &limits).is_err());
    }
}
