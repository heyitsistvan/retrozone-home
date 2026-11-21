//! Reading zips the way the browser does (vault.js: `directory`, `extract`): what a zip holds is
//! read off its list at the end, and one file is taken out by where that list says it is, and
//! checked against the CRC-32 the list gives for it. Nothing else of the zip is relied on, which
//! is what lets a zip a stricter reader turns down still be read, as the site already reads it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use anyhow::{bail, Result};
use flate2::read::DeflateDecoder;

pub struct Entry {
    pub name: String,
    pub method: u16,
    pub crc: u32,
    pub packed: u32,
    pub size: u32,
    pub offset: u32,
}

fn u16_at(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn u32_at(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn read_at(file: &mut File, offset: u64, length: usize) -> Result<Vec<u8>> {
    let mut bytes = vec![0u8; length];
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(&mut bytes)?;
    Ok(bytes)
}

/// What a zip holds, read off its end. None for what is no whole zip
pub fn entries(file: &mut File) -> Result<Option<Vec<Entry>>> {
    let size = file.metadata()?.len();
    let from = size.saturating_sub(65558);
    let tail = read_at(file, from, (size - from) as usize)?;
    if tail.len() < 22 {
        return Ok(None);
    }

    // The record that ends a zip is the last thing in it, bar a comment
    let Some(end) = (0..=tail.len() - 22)
        .rev()
        .find(|&at| u32_at(&tail, at) == 0x0605_4b50)
    else {
        return Ok(None);
    };

    let count = u16_at(&tail, end + 10) as usize;
    let list_size = u32_at(&tail, end + 12) as usize;
    let list_offset = u32_at(&tail, end + 16) as u64;
    if list_offset + list_size as u64 > size {
        return Ok(None);
    }
    let list = if list_offset >= from {
        let start = (list_offset - from) as usize;
        tail[start..(start + list_size).min(tail.len())].to_vec()
    } else {
        read_at(file, list_offset, list_size)?
    };

    let mut found = Vec::with_capacity(count);
    let mut at = 0;
    while found.len() < count && at + 46 <= list.len() && u32_at(&list, at) == 0x0201_4b50 {
        let name_length = u16_at(&list, at + 28) as usize;
        if at + 46 + name_length > list.len() {
            break;
        }
        found.push(Entry {
            name: String::from_utf8_lossy(&list[at + 46..at + 46 + name_length]).into_owned(),
            method: u16_at(&list, at + 10),
            crc: u32_at(&list, at + 16),
            packed: u32_at(&list, at + 20),
            size: u32_at(&list, at + 24),
            offset: u32_at(&list, at + 42),
        });
        at += 46 + name_length + u16_at(&list, at + 30) as usize + u16_at(&list, at + 32) as usize;
    }

    Ok(Some(found))
}

/// One file out of a zip, checked against what the zip's list says it is
pub fn extract(file: &mut File, entry: &Entry) -> Result<Vec<u8>> {
    let head = read_at(file, entry.offset as u64, 30)?;
    if u32_at(&head, 0) != 0x0403_4b50 {
        bail!("not a zip entry");
    }

    let start = entry.offset as u64 + 30 + u16_at(&head, 26) as u64 + u16_at(&head, 28) as u64;
    let packed = read_at(file, start, entry.packed as usize)?;
    let bytes = match entry.method {
        0 => packed,
        8 => {
            let mut bytes = Vec::with_capacity(entry.size as usize);
            DeflateDecoder::new(&packed[..]).read_to_end(&mut bytes)?;
            bytes
        }
        method => bail!("packed in a way that is not read here ({method})"),
    };

    if crc32fast::hash(&bytes) != entry.crc {
        bail!("not what the zip says it is");
    }

    Ok(bytes)
}
