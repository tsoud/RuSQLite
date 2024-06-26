#![allow(dead_code)]

use std::{
    error::Error,
    fmt,
    io::{BufReader, Read, Seek, SeekFrom},
};

use crate::{
    btree_page::{BtreePage, PageType},
    db::Database,
    varint::decode_be,
};

#[derive(Debug)]
pub struct InvalidFieldError {
    details: String,
}

impl InvalidFieldError {
    fn new(cell_type: &str, field_name: &str) -> Self {
        Self {
            details: format!(
                "Type `{}` does not have a `{}` field",
                cell_type, field_name
            ),
        }
    }
}

impl fmt::Display for InvalidFieldError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.details)
    }
}

impl Error for InvalidFieldError {}

#[derive(Debug, Default)]
pub struct Cell {
    pub offset: u64,
    pub size: usize,
}

#[derive(Debug, Default)]
pub struct Payload {
    pub size: u64, // in bytes, including overflow
    pub payload: Vec<u8>,
    pub overflow: Option<[u8; 4]>,
}

impl Payload {
    pub fn calculate_spillage(&self, db: &Database, page: &BtreePage) -> u64 {
        // Variables below are explained in SQLite documentation: https://www.sqlite.org/fileformat2.html#b_tree_pages
        let p = self.size;
        let u = db.page_size as u64 - db.reserved_space as u64;
        let m = ((u - 12) * 32 / 255) - 23;
        let k = m + ((p - m) % (u - 4));
        let x = match page.page_type {
            PageType::LeafTable => u - 35,
            PageType::LeafIndex | PageType::InteriorIndex => ((u - 12) * 64 / 255) - 23,
            _ => 0,
        };
        match p {
            p if (p > x && k <= x) => p - k,
            p if (p > x && k > x) => p - m,
            _ => 0,
        }
    }
}

#[derive(Debug)]
pub enum CellContent {
    LeafTable {
        cell_type: &'static str,
        row_id: u64,
        payload: Payload,
    },
    LeafIndex {
        cell_type: &'static str,
        payload: Payload,
    },
    InteriorIndex {
        cell_type: &'static str,
        left_child_ptr: u32,
        payload: Payload,
    },
    InteriorTable {
        cell_type: &'static str,
        left_child_ptr: u32,
        integer_key: u64,
    },
}

impl CellContent {
    pub fn get_cell_data(
        pg: &BtreePage,
        db: &mut Database,
        cell: Cell,
    ) -> Result<Self, Box<dyn Error>> {
        let mut reader = BufReader::new(&db.file);
        reader
            .seek(SeekFrom::Start(pg.file_starting_position + cell.offset))
            .map_err(|e| e.to_string())?;
        let mut cell_buf = vec![0u8; cell.size];
        reader
            .read_exact(&mut cell_buf)
            .map_err(|e| e.to_string())?;

        match pg.page_type {
            PageType::LeafTable => {
                let cell_type = "B-Tree Leaf Table";
                let (row_id, payload) =
                    parse_leaf_table_cell(cell, &mut cell_buf).map_err(|e| e.to_string())?;
                Ok(CellContent::LeafTable {
                    cell_type,
                    row_id,
                    payload,
                })
            }
            PageType::InteriorTable => {
                let cell_type = "B-Tree Interior Table";
                let (left_child_ptr, integer_key) =
                    parse_interior_table_cell(&mut cell_buf).map_err(|e| e.to_string())?;
                Ok(CellContent::InteriorTable {
                    cell_type,
                    left_child_ptr,
                    integer_key,
                })
            }
            PageType::LeafIndex => {
                let cell_type = "B-Tree Leaf Index";
                let payload =
                    parse_leaf_index_cell(cell, &mut cell_buf).map_err(|e| e.to_string())?;
                Ok(CellContent::LeafIndex { cell_type, payload })
            }
            PageType::InteriorIndex => {
                let cell_type = "B-Tree Interior Index";
                let (left_child_ptr, payload) =
                    parse_interior_index_cell(cell, &mut cell_buf).map_err(|e| e.to_string())?;
                Ok(CellContent::InteriorIndex {
                    cell_type,
                    left_child_ptr,
                    payload,
                })
            }
        }
    }

    pub fn get_payload(&self) -> Result<&[u8], InvalidFieldError> {
        match self {
            CellContent::LeafTable { payload, .. }
            | CellContent::LeafIndex { payload, .. }
            | CellContent::InteriorIndex { payload, .. } => Ok(&payload.payload),
            CellContent::InteriorTable { cell_type, .. } => {
                Err(InvalidFieldError::new(cell_type, "payload"))
            }
        }
    }

    pub fn get_left_child_pointer(&self) -> Result<u32, InvalidFieldError> {
        match self {
            CellContent::InteriorTable { left_child_ptr, .. }
            | CellContent::InteriorIndex { left_child_ptr, .. } => Ok(*left_child_ptr),
            CellContent::LeafTable { cell_type, .. } | CellContent::LeafIndex { cell_type, .. } => {
                Err(InvalidFieldError::new(cell_type, "left_child_ptr"))
            }
        }
    }

    pub fn get_row_id(&self) -> Result<u64, InvalidFieldError> {
        match self {
            CellContent::LeafTable { row_id, .. } => Ok(*row_id),
            CellContent::InteriorTable { cell_type, .. }
            | CellContent::InteriorIndex { cell_type, .. }
            | CellContent::LeafIndex { cell_type, .. } => {
                Err(InvalidFieldError::new(cell_type, "row_id"))
            }
        }
    }
}

fn parse_leaf_table_cell(
    cell: Cell,
    cell_buf: &mut [u8],
) -> Result<(u64, Payload), Box<dyn Error>> {
    let mut payload = Payload::default();
    let mut varint_len: usize;
    let mut position: usize = 0;

    (payload.size, varint_len) = decode_be(cell_buf).map_err(|e| e.to_string())?;
    position += varint_len;

    if payload.size > cell.size as u64 {
        let overflow: [u8; 4] = cell_buf[cell_buf.len() - 4..].try_into()?;
        payload.overflow = Some(overflow);
    }

    let rowid: u64;
    (rowid, varint_len) = decode_be(&cell_buf[position..]).map_err(|e| e.to_string())?;
    position += varint_len;

    payload.payload = match payload.overflow {
        Some(_) => cell_buf[position..cell_buf.len() - 4].to_vec(),
        None => cell_buf[position..].to_vec(),
    };
    Ok((rowid, payload))
}

fn parse_interior_table_cell(cell_buf: &mut [u8]) -> Result<(u32, u64), Box<dyn Error>> {
    let left_child_ptr_buf: [u8; 4] = cell_buf[..4].try_into()?;
    let left_child_ptr = u32::from_be_bytes(left_child_ptr_buf);
    let (int_key, _) = decode_be(&cell_buf[4..])?;
    Ok((left_child_ptr, int_key))
}

fn parse_leaf_index_cell(cell: Cell, cell_buf: &mut [u8]) -> Result<Payload, Box<dyn Error>> {
    let mut payload = Payload::default();
    let varint_len: usize;
    (payload.size, varint_len) = decode_be(cell_buf).map_err(|e| e.to_string())?;

    if payload.size > cell.size as u64 {
        let overflow: [u8; 4] = cell_buf[cell_buf.len() - 4..].try_into()?;
        payload.overflow = Some(overflow);
    }

    payload.payload = match payload.overflow {
        Some(_) => cell_buf[varint_len..cell_buf.len() - 4].to_vec(),
        None => cell_buf[varint_len..].to_vec(),
    };
    Ok(payload)
}

fn parse_interior_index_cell(
    cell: Cell,
    cell_buf: &mut [u8],
) -> Result<(u32, Payload), Box<dyn Error>> {
    let left_child_ptr_buf: [u8; 4] = cell_buf[..4].try_into()?;
    let left_child_ptr = u32::from_be_bytes(left_child_ptr_buf);
    let mut payload = Payload::default();
    let varint_len: usize;
    (payload.size, varint_len) = decode_be(&cell_buf[4..]).map_err(|e| e.to_string())?;

    if payload.size > cell.size as u64 {
        let overflow: [u8; 4] = cell_buf[cell_buf.len() - 4..].try_into()?;
        payload.overflow = Some(overflow);
    }

    payload.payload = match payload.overflow {
        Some(_) => cell_buf[4 + varint_len..cell_buf.len() - 4].to_vec(),
        None => cell_buf[4 + varint_len..].to_vec(),
    };
    Ok((left_child_ptr, payload))
}
