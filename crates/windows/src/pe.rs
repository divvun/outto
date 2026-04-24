//! Read and write COFF PE sections.
//!
//! Used to embed .box payloads into installer executables and read them back at runtime.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

const DOS_MAGIC: u16 = 0x5A4D; // "MZ"
const PE_SIGNATURE: u32 = 0x00004550; // "PE\0\0"
const PE32_MAGIC: u16 = 0x10B;
const PE32PLUS_MAGIC: u16 = 0x20B;

const IMAGE_SCN_CNT_INITIALIZED_DATA: u32 = 0x00000040;
const IMAGE_SCN_MEM_READ: u32 = 0x40000000;

/// Find a named section in a PE file. Returns `(file_offset, size)` if found.
pub fn find_section(exe_path: &Path, section_name: &str) -> io::Result<Option<(u64, u64)>> {
    let mut f = File::open(exe_path)?;
    let info = parse_pe_info(&mut f)?;

    let name_bytes = pad_section_name(section_name);

    for i in 0..info.number_of_sections as u64 {
        let header_offset = info.section_table_offset + i * 40;
        f.seek(SeekFrom::Start(header_offset))?;

        let mut name = [0u8; 8];
        f.read_exact(&mut name)?;

        if name == name_bytes {
            let virtual_size = read_u32(&mut f)?;
            let _virtual_address = read_u32(&mut f)?;
            let _size_of_raw_data = read_u32(&mut f)?;
            let pointer_to_raw_data = read_u32(&mut f)?;

            return Ok(Some((pointer_to_raw_data as u64, virtual_size as u64)));
        }
    }

    Ok(None)
}

/// Embed data as a new named PE section. The file is modified in-place.
pub fn embed_section(exe_path: &Path, section_name: &str, data: &[u8]) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(exe_path)?;

    let info = parse_pe_info(&mut f)?;

    let mut last_section_raw_end: u64 = 0;
    let mut last_section_va_end: u64 = 0;

    for i in 0..info.number_of_sections as u64 {
        let header_offset = info.section_table_offset + i * 40;
        f.seek(SeekFrom::Start(header_offset + 8))?;

        let virtual_size = read_u32(&mut f)? as u64;
        let virtual_address = read_u32(&mut f)? as u64;
        let size_of_raw_data = read_u32(&mut f)? as u64;
        let pointer_to_raw_data = read_u32(&mut f)? as u64;

        let raw_end = pointer_to_raw_data + size_of_raw_data;
        let va_end = virtual_address + virtual_size;

        if raw_end > last_section_raw_end {
            last_section_raw_end = raw_end;
        }
        if va_end > last_section_va_end {
            last_section_va_end = va_end;
        }
    }

    let new_header_offset = info.section_table_offset + (info.number_of_sections as u64) * 40;

    let first_section_raw_start = {
        f.seek(SeekFrom::Start(info.section_table_offset + 20))?;
        read_u32(&mut f)? as u64
    };

    if new_header_offset + 40 > first_section_raw_start {
        return Err(io::Error::new(
            io::ErrorKind::Other,
            "No room for additional section header in PE file. \
             The section table is full.",
        ));
    }

    let file_alignment = info.file_alignment as u64;
    let section_alignment = info.section_alignment as u64;

    let new_data_offset = align_up(last_section_raw_end, file_alignment);
    let new_va = align_up(last_section_va_end, section_alignment);
    let raw_data_size = align_up(data.len() as u64, file_alignment);

    let name_bytes = pad_section_name(section_name);
    f.seek(SeekFrom::Start(new_header_offset))?;

    f.write_all(&name_bytes)?;
    write_u32(&mut f, data.len() as u32)?;
    write_u32(&mut f, new_va as u32)?;
    write_u32(&mut f, raw_data_size as u32)?;
    write_u32(&mut f, new_data_offset as u32)?;
    write_u32(&mut f, 0)?;
    write_u32(&mut f, 0)?;
    write_u16(&mut f, 0)?;
    write_u16(&mut f, 0)?;
    write_u32(&mut f, IMAGE_SCN_CNT_INITIALIZED_DATA | IMAGE_SCN_MEM_READ)?;

    f.seek(SeekFrom::Start(info.coff_header_offset + 2))?;
    write_u16(&mut f, info.number_of_sections + 1)?;

    let new_size_of_image = align_up(new_va + data.len() as u64, section_alignment) as u32;
    f.seek(SeekFrom::Start(info.optional_header_offset + 56))?;
    write_u32(&mut f, new_size_of_image)?;

    f.seek(SeekFrom::Start(info.optional_header_offset + 64))?;
    write_u32(&mut f, 0)?;

    f.seek(SeekFrom::Start(new_data_offset))?;
    f.write_all(data)?;

    let padding = raw_data_size as usize - data.len();
    if padding > 0 {
        f.write_all(&vec![0u8; padding])?;
    }

    f.flush()?;
    Ok(())
}

struct PeInfo {
    coff_header_offset: u64,
    optional_header_offset: u64,
    section_table_offset: u64,
    number_of_sections: u16,
    file_alignment: u32,
    section_alignment: u32,
}

fn parse_pe_info(f: &mut File) -> io::Result<PeInfo> {
    f.seek(SeekFrom::Start(0))?;
    let dos_magic = read_u16(f)?;
    if dos_magic != DOS_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a valid PE file (bad DOS magic)",
        ));
    }

    f.seek(SeekFrom::Start(0x3C))?;
    let e_lfanew = read_u32(f)? as u64;

    f.seek(SeekFrom::Start(e_lfanew))?;
    let pe_sig = read_u32(f)?;
    if pe_sig != PE_SIGNATURE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Not a valid PE file (bad PE signature)",
        ));
    }

    let coff_header_offset = e_lfanew + 4;

    f.seek(SeekFrom::Start(coff_header_offset))?;
    let _machine = read_u16(f)?;
    let number_of_sections = read_u16(f)?;
    f.seek(SeekFrom::Start(coff_header_offset + 16))?;
    let size_of_optional_header = read_u16(f)?;

    let optional_header_offset = coff_header_offset + 20;
    let section_table_offset = optional_header_offset + size_of_optional_header as u64;

    f.seek(SeekFrom::Start(optional_header_offset))?;
    let opt_magic = read_u16(f)?;
    if opt_magic != PE32_MAGIC && opt_magic != PE32PLUS_MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Unknown optional header magic",
        ));
    }

    f.seek(SeekFrom::Start(optional_header_offset + 32))?;
    let section_alignment = read_u32(f)?;
    let file_alignment = read_u32(f)?;

    Ok(PeInfo {
        coff_header_offset,
        optional_header_offset,
        section_table_offset,
        number_of_sections,
        file_alignment,
        section_alignment,
    })
}

fn pad_section_name(name: &str) -> [u8; 8] {
    let mut bytes = [0u8; 8];
    let name_bytes = name.as_bytes();
    let len = name_bytes.len().min(8);
    bytes[..len].copy_from_slice(&name_bytes[..len]);
    bytes
}

fn align_up(value: u64, alignment: u64) -> u64 {
    if alignment == 0 {
        return value;
    }
    (value + alignment - 1) & !(alignment - 1)
}

fn read_u16(f: &mut File) -> io::Result<u16> {
    let mut buf = [0u8; 2];
    f.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

fn read_u32(f: &mut File) -> io::Result<u32> {
    let mut buf = [0u8; 4];
    f.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

fn write_u16(f: &mut File, v: u16) -> io::Result<()> {
    f.write_all(&v.to_le_bytes())
}

fn write_u32(f: &mut File, v: u32) -> io::Result<()> {
    f.write_all(&v.to_le_bytes())
}
